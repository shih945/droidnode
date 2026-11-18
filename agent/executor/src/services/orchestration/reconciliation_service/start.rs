use super::*;

impl ReconciliationServiceImpl {
    /// Start (or restart) a pod. Returns true when a handle was stored.
    ///
    /// This is the slow path (image pull + unpack + proot spawn). It is called
    /// WITHOUT holding the `running` lock, so one slow pull no longer blocks
    /// every other pod on the node (B20).
    #[instrument(skip(self, pod), fields(pod = %pod.metadata.name.as_deref().unwrap_or("?")))]
    pub(super) async fn start_pod(&self, pod: &Pod, uid: &str, previous_restarts: i32) -> bool {
        let pod_name = pod.metadata.name.clone().unwrap_or_default();
        // B1: a pod may have several containers, each with its own image/rootfs.
        // B2: init containers are prepared here too - they run before the main
        // containers and may use a completely different image.
        let containers: Vec<(String, String)> = pod
            .spec
            .as_ref()
            .map(|s| {
                s.init_containers
                    .iter()
                    .flatten()
                    .chain(s.containers.iter())
                    .map(|c| (c.name.clone(), c.image.clone().unwrap_or_default()))
                    .collect()
            })
            .unwrap_or_default();

        if containers.is_empty() || containers.iter().any(|(_, i)| i.is_empty()) {
            warn!(pod = %pod_name, "pod has no image; skipping");
            return false;
        }
        let images_label = containers
            .iter()
            .map(|(_, i)| i.as_str())
            .collect::<Vec<_>>()
            .join(", ");

        // C5: fetch the docker config payloads referenced by imagePullSecrets
        // once per pod. A missing/broken secret is a warning (like kubelet's
        // FailedToRetrieveImagePullSecret); anonymous pull may still work.
        let namespace = pod
            .metadata
            .namespace
            .clone()
            .unwrap_or_else(|| "default".into());
        let mut pull_secret_payloads: Vec<Vec<u8>> = Vec::new();
        if let Some(spec) = pod.spec.as_ref() {
            for secret_ref in spec.image_pull_secrets.iter().flatten() {
                let secret_name = secret_ref.name.clone().unwrap_or_default();
                if secret_name.is_empty() {
                    continue;
                }
                match self.cp_broker.get_secret(&namespace, &secret_name).await {
                    Ok(data) => {
                        if let Some(payload) = data
                            .get(".dockerconfigjson")
                            .or_else(|| data.get(".dockercfg"))
                        {
                            pull_secret_payloads.push(payload.clone());
                        } else {
                            warn!(
                                pod = %pod_name,
                                secret = %secret_name,
                                "imagePullSecret has no .dockerconfigjson/.dockercfg entry"
                            );
                        }
                    }
                    Err(e) => {
                        warn!(
                            pod = %pod_name,
                            secret = %secret_name,
                            error = %e,
                            "could not read imagePullSecret"
                        );
                        self.emit_event(
                            pod,
                            "FailedToRetrieveImagePullSecret",
                            &format!("Unable to retrieve imagePullSecret \"{secret_name}\": {e}"),
                            true,
                        )
                        .await;
                    }
                }
            }
        }

        // Port conflict (B23): never start something that cannot bind.
        let ports = Self::declared_ports(pod);
        if let Some(problem) = self.port_conflict(&pod_name, &ports).await {
            let (reason, msg) = match problem {
                PortProblem::InUse(port, owner) => (
                    "PortConflict",
                    format!("port {port} is already in use by {owner}"),
                ),
                PortProblem::Privileged(port) => (
                    "PrivilegedPort",
                    format!(
                        "containerPort {port} is below 1024; the non-root agent cannot bind it, use a port >= 1024"
                    ),
                ),
            };
            warn!(pod = %pod_name, reason, message = %msg, "container port unusable - not starting");
            eprintln!(
                "POD_EVENT {} pod={} message={}",
                reason.to_lowercase(),
                pod_name,
                msg
            );
            self.emit_event(pod, reason, &msg, true).await;
            self.report_waiting(pod, uid, reason, &msg).await;
            return false;
        }

        // B21: if the last attempt failed, wait out the backoff window instead of
        // hammering the registry every 15 s (and spamming events).
        {
            let failures = self.start_failures.lock().await;
            if let Some(st) = failures.get(uid) {
                if st.next_at.is_some_and(|t| Instant::now() < t) {
                    let msg = if st.message.is_empty() {
                        "back-off pulling image".to_string()
                    } else {
                        st.message.clone()
                    };
                    drop(failures);
                    self.report_waiting(pod, uid, "ImagePullBackOff", &msg)
                        .await;
                    return false;
                }
            }
        }

        // A20: show ContainerCreating while the images are being pulled, the way
        // a real kubelet does, instead of leaving the pod in Pending with no status.
        self.report_waiting(
            pod,
            uid,
            "ContainerCreating",
            &format!("pulling image(s) {images_label}"),
        )
        .await;
        self.emit_event(
            pod,
            "Pulling",
            &format!("Pulling image(s) {images_label}"),
            false,
        )
        .await;

        // Prepare every container's image; any failure fails the whole start.
        let pull_started = Instant::now();
        let mut roots: std::collections::HashMap<String, std::path::PathBuf> =
            std::collections::HashMap::new();
        for (container_name, image_name) in &containers {
            let image_ref = match crate::models::ImageRef::parse(image_name) {
                Ok(r) => r,
                Err(e) => {
                    error!(pod = %pod_name, container = %container_name, error = %e, "invalid image ref");
                    self.emit_event(
                        pod,
                        "Failed",
                        &format!("Invalid image reference \"{image_name}\": {e}"),
                        true,
                    )
                    .await;
                    return false;
                }
            };

            info!(pod = %pod_name, container = %container_name, image = %image_name, "preparing image");
            // C5: resolve credentials against the image's upstream registry
            // (the pull itself may go through a mirror host).
            let auth = crate::services::foundation::pull_secret_service::resolve(
                &pull_secret_payloads,
                image_name,
            );
            if auth.is_some() {
                info!(
                    pod = %pod_name,
                    container = %container_name,
                    image = %image_name,
                    "using imagePullSecret credentials"
                );
            }
            // Serialise per image, not per node.
            let policy = crate::models::PullPolicy::resolve(
                pod.spec
                    .as_ref()
                    .and_then(|s| s.containers.iter().find(|c| &c.name == container_name))
                    .and_then(|c| c.image_pull_policy.as_deref()),
                &image_ref.reference,
            );
            let image_guard = self.image_lock(image_name).await;
            let prepared = {
                let _held = image_guard.lock().await;
                self.image_orch
                    .prepare_image(&image_ref, policy, auth.as_ref())
                    .await
            };

            match prepared {
                Ok(p) => {
                    roots.insert(container_name.clone(), p);
                }
                Err(e) => {
                    error!(pod = %pod_name, container = %container_name, error = %e, "image preparation failed");
                    self.emit_event(
                        pod,
                        "Failed",
                        &format!("Failed to pull image \"{image_name}\": {e}"),
                        true,
                    )
                    .await;
                    self.report_waiting(
                        pod,
                        uid,
                        "ErrImagePull",
                        &format!("image pull failed: {e}"),
                    )
                    .await;
                    // B21: exponential backoff for the next attempt.
                    let delay = {
                        let mut failures = self.start_failures.lock().await;
                        let st = failures.entry(uid.to_string()).or_default();
                        st.attempts += 1;
                        st.message = format!("back-off pulling image \"{image_name}\": {e}");
                        let delay = backoff_delay(st.attempts);
                        st.next_at = Some(Instant::now() + delay);
                        delay
                    };
                    info!(pod = %pod_name, retry_in_secs = delay.as_secs(), "image pull will be retried");
                    return false;
                }
            }
        }

        self.emit_event(
            pod,
            "Pulled",
            &format!(
                "Successfully pulled image(s) {images_label} in {:.1}s",
                pull_started.elapsed().as_secs_f64()
            ),
            false,
        )
        .await;

        match self.workload_lifecycle.start(pod, roots).await {
            Ok(w) => {
                if previous_restarts > 0 {
                    for c in &w.containers {
                        c.restart_count.store(previous_restarts, Ordering::Relaxed);
                    }
                }
                self.start_failures.lock().await.remove(uid);
                self.emit_event(pod, "Started", "Started container(s)", false)
                    .await;
                self.running.lock().await.insert(uid.to_string(), w);
                self.claim_ports(&pod_name, &ports).await;
                true
            }
            Err(e) => {
                error!(pod = %pod_name, error = %e, "workload start failed");
                self.emit_event(
                    pod,
                    "Failed",
                    &format!("Failed to start container: {e}"),
                    true,
                )
                .await;

                // B2: with restartPolicy=Never a failed init container is final.
                // Leaving the pod in ContainerCreating while we retry forever is
                // exactly what confuses people debugging a broken init step.
                let policy = pod
                    .spec
                    .as_ref()
                    .and_then(|s| s.restart_policy.clone())
                    .unwrap_or_else(|| "Always".to_string());
                if policy == "Never" {
                    let status = crate::models::PodRunStatus {
                        pod_uid: uid.to_string(),
                        pod_name: pod_name.clone(),
                        namespace: pod
                            .metadata
                            .namespace
                            .clone()
                            .unwrap_or_else(|| "default".into()),
                        phase: crate::models::PodPhase::Failed,
                        message: Some(e.to_string()),
                        containers: pod
                            .spec
                            .as_ref()
                            .map(|s| {
                                s.init_containers
                                    .iter()
                                    .flatten()
                                    .map(|c| crate::models::ContainerRunStatus {
                                        name: c.name.clone(),
                                        ready: false,
                                        running: false,
                                        exit_code: Some(1),
                                        restart_count: 0,
                                        started_at: None,
                                        finished_at: Some(
                                            chrono::Utc::now()
                                                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                                        ),
                                        reason: Some("InitContainerFailed".to_string()),
                                        waiting_reason: None,
                                        waiting_message: None,
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    };
                    let _ = self
                        .cp_broker
                        .report_pod_status(&pod_name, &status.namespace, &status)
                        .await;
                    self.workload_lifecycle.publish_status(&status).await;
                }
                false
            }
        }
    }
}
