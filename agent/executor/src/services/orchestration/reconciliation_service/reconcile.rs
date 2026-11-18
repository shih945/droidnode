use super::*;

impl ReconciliationServiceImpl {
    /// One reconciliation pass.
    ///
    /// Ordering is deliberate. Pods that are going away are stopped *before*
    /// anything new starts, and the slow path (image pull + unpack + proot
    /// spawn) is queued to the serial start worker instead of being awaited
    /// here: one 120 s registry timeout used to stall every delete on the node,
    /// which made `kubectl delete` look hung for minutes.
    #[instrument(skip(self))]
    pub(super) async fn reconcile_once(&self) -> Result<()> {
        let desired_pods = self.desired_pods().await?;
        let desired_uids: HashSet<String> = desired_pods
            .iter()
            .filter_map(|p| p.metadata.uid.clone())
            .collect();

        // Keep the spec around for `/pods` (kubelet returns containers too).
        for pod in &desired_pods {
            self.workload_lifecycle.store_pod(pod).await;
        }

        // ---- phase 0: drained (E7) -----------------------------------------
        // Low battery: stop everything gracefully but keep the agent alive so it
        // can report status and resume the moment power comes back.
        let drained = self.drained.load(Ordering::Relaxed);
        if drained {
            let to_stop: Vec<(String, Arc<RunningWorkload>)> = {
                let mut running = self.running.lock().await;
                let ids: Vec<String> = running.keys().cloned().collect();
                ids.into_iter()
                    .filter_map(|uid| running.remove(&uid).map(|w| (uid, w)))
                    .collect()
            };
            for (uid, workload) in to_stop {
                info!(pod = %workload.pod_name, "node drained - stopping pod gracefully");
                if let Err(e) = self.workload_lifecycle.stop(&workload).await {
                    warn!(pod = %workload.pod_name, error = %e, "stop on drain failed");
                }
                self.release_ports(&workload.pod_name).await;
                self.backoff.lock().await.remove(&uid);

                // Report the containers as not-running so Services drop this pod
                // as an endpoint instead of sending traffic to a dead port.
                if let Some(pod) = desired_pods
                    .iter()
                    .find(|p| p.metadata.uid.as_deref() == Some(uid.as_str()))
                {
                    self.report_waiting(
                        pod,
                        &uid,
                        "NodeDraining",
                        "node drained (low battery): containers stopped",
                    )
                    .await;
                }
            }
            // Do not start anything while drained.
            return Ok(());
        }

        // ---- phase 1: stop pods that are no longer assigned -----------------
        // The lock is only held long enough to move handles out of the map.
        let orphaned: Vec<(String, Arc<RunningWorkload>)> = {
            let mut running = self.running.lock().await;
            let ids: Vec<String> = running
                .keys()
                .filter(|uid| !desired_uids.contains(*uid))
                .cloned()
                .collect();
            ids.into_iter()
                .filter_map(|uid| running.remove(&uid).map(|w| (uid, w)))
                .collect()
        };
        for (uid, workload) in orphaned {
            info!(pod = %workload.pod_name, "pod no longer desired - stopping");
            if let Err(e) = self.workload_lifecycle.stop(&workload).await {
                warn!(pod = %workload.pod_name, error = %e, "stop failed");
            }
            self.release_ports(&workload.pod_name).await;
            self.backoff.lock().await.remove(&uid);
            self.starting.lock().await.remove(&uid);
        }

        // ---- phase 2: pods being deleted (always before any start) ----------
        for pod in &desired_pods {
            let Some(uid) = pod.metadata.uid.clone() else {
                continue;
            };
            if pod.metadata.deletion_timestamp.is_some() {
                self.stop_terminating(pod, &uid).await;
            }
        }

        // ---- phase 3: pods we already run -----------------------------------
        for pod in &desired_pods {
            if pod.metadata.deletion_timestamp.is_some() {
                continue;
            }
            let Some(uid) = pod.metadata.uid.clone() else {
                continue;
            };
            let restart_policy = restart_policy(pod);

            let handle = { self.running.lock().await.get(&uid).cloned() };
            let Some(workload) = handle else {
                continue;
            };
            let status = self.workload_lifecycle.status(&workload).await?;

            if status.phase == PodPhase::Running {
                self.backoff.lock().await.remove(&uid);
                self.cp_broker
                    .report_pod_status(&workload.pod_name, &workload.namespace, &status)
                    .await?;
                continue;
            }

            let should_restart = want_restart(&restart_policy, status.phase.clone());

            if should_restart {
                // k8s must see Running + waiting(CrashLoopBackOff), never
                // a terminal phase, otherwise the pod can never come back.
                let crash_message = "Back-off restarting failed container".to_string();
                let mut waiting = status.clone();
                waiting.phase = PodPhase::Running;
                waiting.message = Some(crash_message.clone());
                for ct in waiting.containers.iter_mut() {
                    ct.waiting_reason = Some("CrashLoopBackOff".to_string());
                    ct.waiting_message = Some(crash_message.clone());
                    ct.running = false;
                    ct.ready = false;
                }
                self.cp_broker
                    .report_pod_status(&workload.pod_name, &workload.namespace, &waiting)
                    .await?;
                self.workload_lifecycle.publish_status(&waiting).await;

                let due = {
                    let mut bo = self.backoff.lock().await;
                    let st = bo.entry(uid.clone()).or_default();
                    let now = Instant::now();
                    if st.next_at.is_none_or(|t| now >= t) {
                        st.attempts += 1;
                        let delay = backoff_delay(st.attempts);
                        st.next_at = Some(now + delay);
                        Some((st.attempts, delay))
                    } else {
                        None
                    }
                };

                if let Some((attempt, delay)) = due {
                    info!(
                        pod = %workload.pod_name,
                        phase = ?status.phase,
                        attempt,
                        retry_in_secs = delay.as_secs(),
                        policy = %restart_policy,
                        "restarting container"
                    );
                    eprintln!(
                        "POD_EVENT restarted pod={} attempt={} phase={:?}",
                        workload.pod_name, attempt, status.phase
                    );
                    // A crashed container is replaced immediately; waiting out
                    // the grace period would only slow the restart down.
                    let _ = self.workload_lifecycle.stop_immediately(&workload).await;
                    self.release_ports(&workload.pod_name).await;
                    let restarts = workload.restart_count() + 1;
                    self.take_handle(&uid).await;
                    self.schedule_start(pod, &uid, restarts).await;
                }
                continue;
            }

            // restartPolicy Never (or OnFailure + success): report once
            info!(pod = %workload.pod_name, phase = ?status.phase, "workload finished");
            eprintln!(
                "POD_EVENT finished pod={} phase={:?}",
                workload.pod_name, status.phase
            );
            self.cp_broker
                .report_pod_status(&workload.pod_name, &workload.namespace, &status)
                .await?;
            self.workload_lifecycle.publish_status(&status).await;
            self.take_handle(&uid).await;
            self.release_ports(&workload.pod_name).await;
            self.backoff.lock().await.remove(&uid);
        }

        // ---- phase 4: queue starts for pods without a handle -----------------
        for pod in &desired_pods {
            if pod.metadata.deletion_timestamp.is_some() {
                continue;
            }
            let Some(uid) = pod.metadata.uid.clone() else {
                continue;
            };
            if self.running.lock().await.contains_key(&uid) {
                // Started while this pass was running.
                continue;
            }
            let restart_policy = restart_policy(pod);
            let k8s_phase = pod
                .status
                .as_ref()
                .and_then(|s| s.phase.as_deref())
                .unwrap_or("");

            // Only Never pods stay terminal. Always/OnFailure pods that were
            // marked terminal by an older agent get re-created.
            if terminal_for_policy(&restart_policy, k8s_phase) {
                self.mirror_terminal_status(pod).await;
                continue;
            }

            // Still inside the start-failure backoff window? Report the actual
            // error (kubelet calls this ImagePullBackOff) and wait it out
            // instead of hammering the registry every pass.
            let failure = {
                let failures = self.start_failures.lock().await;
                failures.get(&uid).map(|f| (f.message.clone(), f.next_at))
            };
            if let Some((message, Some(next_at))) = failure {
                if Instant::now() < next_at {
                    let message = if message.is_empty() {
                        "back-off pulling image".to_string()
                    } else {
                        message
                    };
                    self.report_waiting(pod, &uid, "ImagePullBackOff", &message)
                        .await;
                    continue;
                }
            }

            if self.starting.lock().await.contains(&uid) {
                // A pull is already in flight; leave the status to the worker.
                continue;
            }

            eprintln!(
                "POD_EVENT assigned pod={} image={}",
                pod.metadata.name.as_deref().unwrap_or("?"),
                pod.spec
                    .as_ref()
                    .and_then(|s| s.containers.first())
                    .and_then(|c| c.image.clone())
                    .unwrap_or_default()
            );
            // Show the pod immediately: a real kubelet reports Pending as soon
            // as the pod is bound, not only once a container exists.
            self.report_waiting(pod, &uid, "ContainerCreating", "Waiting for image pull")
                .await;
            self.schedule_start(pod, &uid, 0).await;
        }

        Ok(())
    }

    /// Stop a pod the API server is deleting.
    ///
    /// `deletionGracePeriodSeconds == 0` is what `kubectl delete --force
    /// --grace-period=0` produces, and must be immediate: no SIGTERM dance.
    async fn stop_terminating(&self, pod: &Pod, uid: &str) {
        let pod_name = pod.metadata.name.clone().unwrap_or_default();
        let namespace = pod
            .metadata
            .namespace
            .clone()
            .unwrap_or_else(|| "default".to_string());
        info!(pod = %pod_name, "pod is terminating - stopping containers");

        // A start that is still queued must not resurrect a deleted pod.
        self.starting.lock().await.remove(uid);

        if let Some(workload) = self.take_handle(uid).await {
            // B6: honour the grace window the API server granted, not the
            // pod's own default. `kubectl delete --grace-period=5` must
            // finish in ~5s even if the pod asked for 30.
            let granted = pod.metadata.deletion_grace_period_seconds;
            let stopped = if granted == Some(0) {
                info!(pod = %pod_name, "grace period 0 - killing immediately");
                self.workload_lifecycle.stop_immediately(&workload).await
            } else {
                let grace = effective_grace(granted, workload.grace_seconds);
                self.workload_lifecycle
                    .stop_with_grace(&workload, grace)
                    .await
            };
            if let Err(e) = stopped {
                warn!(pod = %pod_name, error = %e, "stop on delete failed");
            }
            self.release_ports(&pod_name).await;
        }
        self.backoff.lock().await.remove(uid);
        self.workload_lifecycle.forget_status(uid).await;
        self.workload_lifecycle.forget_pod(uid).await;
        if let Err(e) = self.cp_broker.delete_pod(&pod_name, &namespace).await {
            warn!(pod = %pod_name, error = %e, "delete pod failed");
        }
    }

    /// Queue a start on the serial worker. Returns false when the queue is not
    /// up yet or another start for this pod is already in flight.
    async fn schedule_start(&self, pod: &Pod, uid: &str, previous_restarts: i32) -> bool {
        if !self.starting.lock().await.insert(uid.to_string()) {
            return false;
        }
        let tx = self.start_tx.lock().await.clone();
        let sent = match tx {
            Some(tx) => tx
                .send(StartRequest {
                    pod: pod.clone(),
                    uid: uid.to_string(),
                    previous_restarts,
                })
                .is_ok(),
            None => false,
        };
        if !sent {
            self.starting.lock().await.remove(uid);
        }
        sent
    }
}

/// Container restart policy of a pod, defaulting to Kubernetes' own default.
fn restart_policy(pod: &Pod) -> String {
    pod.spec
        .as_ref()
        .and_then(|s| s.restart_policy.clone())
        .unwrap_or_else(|| "Always".to_string())
}
