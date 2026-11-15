use super::*;

impl ReconciliationServiceImpl {
    #[instrument(skip(self))]
    pub(super) async fn reconcile_once(&self) -> Result<()> {
        let desired_pods = self.desired_pods().await?;
        let desired_uids: HashSet<String> = desired_pods
            .iter()
            .filter_map(|p| p.metadata.uid.clone())
            .collect();

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
        }

        // ---- phase 2: reconcile every assigned pod --------------------------
        for pod in &desired_pods {
            let uid = match &pod.metadata.uid {
                Some(u) => u.clone(),
                None => continue,
            };
            let pod_name = pod.metadata.name.clone().unwrap_or_default();
            let restart_policy = pod
                .spec
                .as_ref()
                .and_then(|s| s.restart_policy.clone())
                .unwrap_or_else(|| "Always".to_string());

            // -- being deleted? --
            if pod.metadata.deletion_timestamp.is_some() {
                let namespace = pod
                    .metadata
                    .namespace
                    .clone()
                    .unwrap_or_else(|| "default".to_string());
                info!(pod = %pod_name, "pod is terminating - stopping containers");
                if let Some(w) = self.take_handle(&uid).await {
                    // B6: honour the grace window the API server granted, not the
                    // pod's own default. `kubectl delete --grace-period=5` must
                    // finish in ~5s even if the pod asked for 30.
                    let grace = effective_grace(
                        pod.metadata.deletion_grace_period_seconds,
                        w.grace_seconds,
                    );
                    if let Err(e) = self.workload_lifecycle.stop_with_grace(&w, grace).await {
                        warn!(pod = %pod_name, error = %e, "stop on delete failed");
                    }
                    self.release_ports(&pod_name).await;
                }
                self.backoff.lock().await.remove(&uid);
                self.workload_lifecycle.forget_status(&uid).await;
                if let Err(e) = self.cp_broker.delete_pod(&pod_name, &namespace).await {
                    warn!(pod = %pod_name, error = %e, "delete pod failed");
                }
                continue;
            }

            // -- already have a handle? --
            let handle = { self.running.lock().await.get(&uid).cloned() };
            if let Some(workload) = handle {
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
                    let mut waiting = status.clone();
                    waiting.phase = PodPhase::Running;
                    waiting.message = Some("Back-off restarting failed container".to_string());
                    for ct in waiting.containers.iter_mut() {
                        ct.waiting_reason = Some("CrashLoopBackOff".to_string());
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
                        self.start_pod(pod, &uid, restarts).await;
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
                continue;
            }

            // -- no handle yet --
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

            eprintln!(
                "POD_EVENT assigned pod={} image={}",
                pod_name,
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

            self.start_pod(pod, &uid, 0).await;
        }

        Ok(())
    }
}
