use super::*;

#[async_trait]
impl WorkloadLifecycleService for WorkloadLifecycleServiceImpl {
    #[instrument(skip(self, pod, roots), fields(
        pod = pod.metadata.name.as_deref().unwrap_or("?"),
        containers = roots.len()
    ))]
    async fn start(
        &self,
        pod: &Pod,
        roots: std::collections::HashMap<String, PathBuf>,
    ) -> Result<Arc<RunningWorkload>> {
        let pod_uid = pod.metadata.uid.clone().unwrap_or_default();
        let pod_name = pod.metadata.name.clone().unwrap_or_default();
        let namespace = pod
            .metadata
            .namespace
            .clone()
            .unwrap_or_else(|| "default".into());
        let spec = pod
            .spec
            .as_ref()
            .ok_or_else(|| crate::error::DroidError::Workload("pod has no spec".into()))?;
        let grace_seconds = spec
            .termination_grace_period_seconds
            .unwrap_or(DEFAULT_GRACE_SECONDS as i64)
            .max(0) as u64;

        let mut containers: Vec<Arc<RunningContainer>> = Vec::new();

        // B2: init containers run first, strictly in order, and must succeed.
        // A non-zero exit is a pod-level failure; the reconciler then applies the
        // restartPolicy/backoff just like for a crashed main container.
        if let Some(init_containers) = spec.init_containers.as_ref() {
            let total = init_containers.len();
            for (idx, init) in init_containers.iter().enumerate() {
                let Some(image_rootfs) = roots.get(&init.name).cloned() else {
                    warn!(pod = %pod_name, container = %init.name, "no rootfs for init container");
                    continue;
                };
                let rootfs = match self.pod_rootfs(&pod_uid, &init.name, &image_rootfs).await {
                    Ok(p) => p,
                    Err(_) => image_rootfs,
                };

                info!(
                    pod = %pod_name,
                    container = %init.name,
                    step = idx + 1,
                    total,
                    "running init container"
                );
                eprintln!(
                    "POD_EVENT init pod={} container={} step={}/{}",
                    pod_name,
                    init.name,
                    idx + 1,
                    total
                );

                let mut child = self
                    .execution_service
                    .execute_container(pod, init, &rootfs)
                    .await?;

                self.kubelet_state
                    .begin_log(&namespace, &pod_name, &init.name)
                    .await;
                if let Some(stdout) = child.stdout.take() {
                    spawn_log_drain(
                        stdout,
                        self.kubelet_state.clone(),
                        namespace.clone(),
                        pod_name.clone(),
                        init.name.clone(),
                        1,
                        None,
                    );
                }
                if let Some(stderr) = child.stderr.take() {
                    spawn_log_drain(
                        stderr,
                        self.kubelet_state.clone(),
                        namespace.clone(),
                        pod_name.clone(),
                        init.name.clone(),
                        2,
                        None,
                    );
                }

                let waited = tokio::time::timeout(
                    std::time::Duration::from_secs(INIT_CONTAINER_TIMEOUT_SECS),
                    child.wait(),
                )
                .await;

                match waited {
                    Ok(Ok(status)) if status.success() => {
                        info!(pod = %pod_name, container = %init.name, "init container completed");
                    }
                    Ok(Ok(status)) => {
                        return Err(crate::error::DroidError::Workload(format!(
                            "init container \"{}\" failed with exit code {:?}",
                            init.name,
                            status.code()
                        )));
                    }
                    Ok(Err(e)) => {
                        return Err(crate::error::DroidError::Workload(format!(
                            "init container \"{}\" could not be waited on: {e}",
                            init.name
                        )));
                    }
                    Err(_) => {
                        let _ = child.start_kill();
                        let _ = child.wait().await;
                        return Err(crate::error::DroidError::Workload(format!(
                            "init container \"{}\" timed out after {INIT_CONTAINER_TIMEOUT_SECS}s",
                            init.name
                        )));
                    }
                }
            }
        }

        for container in &spec.containers {
            let Some(image_rootfs) = roots.get(&container.name).cloned() else {
                warn!(pod = %pod_name, container = %container.name, "no rootfs prepared - skipping container");
                continue;
            };

            // B17: writable copy per (pod, container) instead of the shared image.
            let rootfs = match self
                .pod_rootfs(&pod_uid, &container.name, &image_rootfs)
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    warn!(pod = %pod_name, container = %container.name, error = %e, "could not create per-pod rootfs; falling back to the shared image");
                    image_rootfs.clone()
                }
            };

            let mut child = match self
                .execution_service
                .execute_container(pod, container, &rootfs)
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    // Roll back whatever already started so we do not leave a
                    // half-created pod behind.
                    for started in &containers {
                        let _ = self.stop_container(&pod_name, started, 0).await;
                    }
                    return Err(e);
                }
            };

            self.kubelet_state
                .begin_log(&namespace, &pod_name, &container.name)
                .await;
            // A2: keep stdin open and mirror output so `kubectl attach` can
            // join the running process.
            let (attach_in_tx, mut attach_in_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
            let (attach_out_tx, _) = tokio::sync::broadcast::channel::<AttachFrame>(256);
            if let Some(mut stdin) = child.stdin.take() {
                tokio::spawn(async move {
                    use tokio::io::AsyncWriteExt;
                    while let Some(data) = attach_in_rx.recv().await {
                        if stdin.write_all(&data).await.is_err() {
                            break;
                        }
                        let _ = stdin.flush().await;
                    }
                });
            }
            if let Some(stdout) = child.stdout.take() {
                spawn_log_drain(
                    stdout,
                    self.kubelet_state.clone(),
                    namespace.clone(),
                    pod_name.clone(),
                    container.name.clone(),
                    1,
                    Some(attach_out_tx.clone()),
                );
            }
            if let Some(stderr) = child.stderr.take() {
                spawn_log_drain(
                    stderr,
                    self.kubelet_state.clone(),
                    namespace.clone(),
                    pod_name.clone(),
                    container.name.clone(),
                    2,
                    Some(attach_out_tx.clone()),
                );
            }

            let started_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let ready = Arc::new(AtomicBool::new(true));
            let probe_alive = Arc::new(AtomicBool::new(true));
            let pgid = child.id().map(|p| p as i32);

            let handle = Arc::new(RunningContainer {
                name: container.name.clone(),
                rootfs: rootfs.clone(),
                image_rootfs: image_rootfs.clone(),
                started_at: started_at.clone(),
                restart_count: AtomicI32::new(0),
                ready: Arc::clone(&ready),
                probe_alive: Arc::clone(&probe_alive),
                child: Mutex::new(child),
            });

            if let Some(pgid) = pgid {
                self.spawn_probes(pod, container, &rootfs, pgid, ready, probe_alive)
                    .await;
                self.kubelet_state.mark_in_use(&rootfs).await;
                self.kubelet_state.mark_in_use(&image_rootfs).await;
                self.kubelet_state
                    .register_pod_proc(
                        &format!("{pod_uid}/{}", container.name),
                        &namespace,
                        &pod_name,
                        &container.name,
                        pgid,
                    )
                    .await;
                // A1: exec needs the same per-pod rootfs (and the fake-root
                // flag) as the running container.
                self.kubelet_state
                    .register_exec_target(&namespace, &pod_name, &container.name, rootfs.clone())
                    .await;
                self.kubelet_state
                    .register_attach_target(
                        &namespace,
                        &pod_name,
                        &container.name,
                        attach_in_tx.clone(),
                        attach_out_tx.clone(),
                    )
                    .await;
            }

            info!(pod = %pod_name, container = %container.name, "container started");
            containers.push(handle);
        }

        if containers.is_empty() {
            return Err(crate::error::DroidError::Workload(
                "no container could be started".into(),
            ));
        }

        eprintln!("POD_EVENT started pod={pod_name}");

        let workload = Arc::new(RunningWorkload {
            pod_uid: pod_uid.clone(),
            pod_name: pod_name.clone(),
            namespace: namespace.clone(),
            grace_seconds,
            containers,
        });

        // Publish immediately so the pod does not vanish from the on-device page
        // between start() and the next reconcile pass.
        let snapshot = PodRunStatus {
            pod_uid,
            pod_name,
            namespace,
            phase: PodPhase::Running,
            message: None,
            containers: workload
                .containers
                .iter()
                .map(|c| ContainerRunStatus {
                    name: c.name.clone(),
                    ready: true,
                    running: true,
                    exit_code: None,
                    restart_count: 0,
                    started_at: Some(c.started_at.clone()),
                    finished_at: None,
                    reason: None,
                    waiting_reason: None,
                })
                .collect(),
        };
        self.kubelet_state.update_status(snapshot).await;

        Ok(workload)
    }

    #[instrument(skip(self, workload), fields(pod = %workload.pod_name))]
    async fn stop(&self, workload: &RunningWorkload) -> Result<()> {
        self.stop_with_grace(workload, workload.grace_seconds).await
    }

    #[instrument(skip(self, workload), fields(pod = %workload.pod_name, grace))]
    async fn stop_with_grace(&self, workload: &RunningWorkload, grace: u64) -> Result<()> {
        for c in &workload.containers {
            self.stop_container(&workload.pod_name, c, grace).await;
            self.kubelet_state.unmark_in_use(&c.rootfs).await;
            self.kubelet_state.unmark_in_use(&c.image_rootfs).await;
            self.kubelet_state
                .unregister_pod_proc(&format!("{}/{}", workload.pod_uid, c.name))
                .await;
        }
        self.kubelet_state.remove_status(&workload.pod_uid).await;
        self.kubelet_state
            .remove_exec_targets(&workload.namespace, &workload.pod_name)
            .await;
        self.kubelet_state
            .remove_attach_targets(&workload.namespace, &workload.pod_name)
            .await;
        info!(pod = %workload.pod_name, grace, "workload stopped");
        Ok(())
    }

    #[instrument(skip(self, workload), fields(pod = %workload.pod_name))]
    async fn stop_immediately(&self, workload: &RunningWorkload) -> Result<()> {
        for c in &workload.containers {
            self.stop_container(&workload.pod_name, c, 0).await;
            self.kubelet_state.unmark_in_use(&c.rootfs).await;
            self.kubelet_state.unmark_in_use(&c.image_rootfs).await;
            self.kubelet_state
                .unregister_pod_proc(&format!("{}/{}", workload.pod_uid, c.name))
                .await;
        }
        self.kubelet_state.remove_status(&workload.pod_uid).await;
        self.kubelet_state
            .remove_exec_targets(&workload.namespace, &workload.pod_name)
            .await;
        self.kubelet_state
            .remove_attach_targets(&workload.namespace, &workload.pod_name)
            .await;
        Ok(())
    }

    #[instrument(skip(self, workload), fields(pod = %workload.pod_name))]
    async fn status(&self, workload: &RunningWorkload) -> Result<PodRunStatus> {
        let mut statuses = Vec::with_capacity(workload.containers.len());
        let mut any_running = false;
        let mut any_failed = false;
        let mut all_done = true;

        for c in &workload.containers {
            let mut child = c.child.lock().await;
            let (running, exit_code) = match child.try_wait() {
                Ok(Some(status)) => (false, status.code()),
                Ok(None) => (true, None),
                Err(e) => {
                    warn!(pod = %workload.pod_name, container = %c.name, error = %e, "could not check child status");
                    (false, None)
                }
            };
            if running {
                any_running = true;
                all_done = false;
            } else {
                if exit_code != Some(0) {
                    any_failed = true;
                }
            }

            let reason = if running {
                None
            } else if exit_code == Some(0) {
                Some("Completed".to_string())
            } else {
                Some("Error".to_string())
            };

            statuses.push(ContainerRunStatus {
                name: c.name.clone(),
                ready: running && c.is_ready(),
                running,
                exit_code,
                restart_count: c.restart_count.load(Ordering::Relaxed),
                started_at: Some(c.started_at.clone()),
                finished_at: if running {
                    None
                } else {
                    Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                },
                reason,
                waiting_reason: None,
            });
        }

        let phase = if any_failed {
            PodPhase::Failed
        } else if any_running {
            PodPhase::Running
        } else if all_done {
            PodPhase::Succeeded
        } else {
            PodPhase::Unknown
        };

        let snapshot = PodRunStatus {
            pod_uid: workload.pod_uid.clone(),
            pod_name: workload.pod_name.clone(),
            namespace: workload.namespace.clone(),
            phase,
            message: None,
            containers: statuses,
        };

        self.kubelet_state.update_status(snapshot.clone()).await;
        Ok(snapshot)
    }

    async fn publish_status(&self, status: &PodRunStatus) {
        self.kubelet_state.update_status(status.clone()).await;
    }

    async fn forget_status(&self, pod_uid: &str) {
        self.kubelet_state.remove_status(pod_uid).await;
    }
}
