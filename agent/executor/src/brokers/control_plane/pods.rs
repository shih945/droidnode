use async_trait::async_trait;
use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, Patch, PatchParams};
use serde_json::json;
use tracing::{debug, info, instrument};

use crate::error::{DroidError, Result};

use super::{ControlPlaneBrokerImpl, PodWatch, PodWatchEvent};

#[async_trait]
impl PodWatch for ControlPlaneBrokerImpl {
    #[instrument(skip(self), fields(node = %self.node_name))]
    async fn get_assigned_pods(&self) -> Result<Vec<Pod>> {
        use kube::api::ListParams;
        let pods: Api<Pod> = Api::all(self.client.clone());
        let field_selector = format!("spec.nodeName={}", self.node_name);
        let lp = ListParams::default().fields(&field_selector);
        let pod_list = pods.list(&lp).await.map_err(DroidError::KubeApi)?;
        Ok(pod_list.items)
    }

    #[instrument(skip(self, tx), fields(node = %self.node_name))]
    async fn watch_assigned_pods(
        &self,
        tx: tokio::sync::mpsc::Sender<PodWatchEvent>,
    ) -> Result<()> {
        use futures::StreamExt;
        use kube::runtime::watcher;

        let pods: Api<Pod> = Api::all(self.client.clone());
        let cfg = watcher::Config::default().fields(&format!("spec.nodeName={}", self.node_name));
        let stream = watcher(pods, cfg);
        let mut stream = std::pin::pin!(stream);

        while let Some(event) = stream.next().await {
            let event = event.map_err(|e| DroidError::Watch(e.to_string()))?;
            let mapped = match event {
                watcher::Event::Applied(pod) => PodWatchEvent::Applied(pod),
                watcher::Event::Deleted(pod) => {
                    PodWatchEvent::Deleted(pod.metadata.uid.clone().unwrap_or_default())
                }
                watcher::Event::Restarted(pods) => PodWatchEvent::Restarted(pods),
            };
            if tx.send(mapped).await.is_err() {
                // Reconcile service dropped the receiver.
                break;
            }
        }
        Ok(())
    }

    #[instrument(skip(self, status), fields(pod = %pod_name, ns = %namespace))]
    async fn report_pod_status(
        &self,
        pod_name: &str,
        namespace: &str,
        status: &crate::models::PodRunStatus,
    ) -> Result<()> {
        use chrono::Utc;
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), namespace);

        let container_statuses: Vec<serde_json::Value> = status
            .containers
            .iter()
            .map(|c| {
                let state = if c.running {
                    json!({ "running": { "startedAt": c.started_at } })
                } else if let Some(waiting) = &c.waiting_reason {
                    // CrashLoopBackOff: kubectl shows this in the STATUS column
                    json!({ "waiting": {
                        "reason": waiting,
                        "message": "back-off restarting failed container"
                    }})
                } else {
                    // reason (Completed / Error) is what kubectl renders instead
                    // of the raw `ExitCode:N` fallback.
                    json!({ "terminated": {
                        "exitCode": c.exit_code.unwrap_or(0),
                        "reason": c.reason,
                        "startedAt": c.started_at,
                        "finishedAt": c.finished_at
                    }})
                };
                json!({
                    "name": c.name,
                    "ready": c.ready,
                    "started": c.running,
                    "restartCount": c.restart_count,
                    "state": state
                })
            })
            .collect();

        // Every container of this pod shares the device's network namespace, so
        // there is no per-pod IP to invent: report the node IP as the pod IP.
        // That keeps `kubectl get pod -o wide` meaningful and lets the endpoints
        // controller publish nodeIP:containerPort, which kube-proxy can DNAT to.
        let node_ip = Self::self_ip();
        let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let all_ready = !status.containers.is_empty() && status.containers.iter().all(|c| c.ready);
        let phase = status.phase.as_str();
        let ready_status = if all_ready && phase == "Running" {
            "True"
        } else {
            "False"
        };

        let patch = json!({
            "status": {
                "phase": phase,
                "message": status.message,
                "hostIP": node_ip,
                "podIP": node_ip,
                "podIPs": [ { "ip": node_ip } ],
                "conditions": [
                    {
                        "type": "PodScheduled",
                        "status": "True",
                        "reason": "DroidNodeScheduled",
                        "lastTransitionTime": now
                    },
                    {
                        "type": "Initialized",
                        "status": "True",
                        "reason": "DroidNodeInitialized",
                        "lastTransitionTime": now
                    },
                    {
                        "type": "ContainersReady",
                        "status": ready_status,
                        "reason": "DroidNodeContainers",
                        "lastTransitionTime": now
                    },
                    {
                        "type": "Ready",
                        "status": ready_status,
                        "reason": "DroidNodeReady",
                        "lastTransitionTime": now
                    }
                ],
                "containerStatuses": container_statuses
            }
        });

        match pods
            .patch_status(
                pod_name,
                &PatchParams::apply("droidnode"),
                &Patch::Merge(&patch),
            )
            .await
        {
            Ok(_) => {}
            // Pod already in a terminal state — k8s forbids transitioning back to non-terminal.
            // This is fine: the pod ran and completed, our status is stale. Ignore.
            Err(kube::Error::Api(ref e))
                if e.code == 422 && e.message.contains("non-terminated") =>
            {
                debug!(pod = %pod_name, "pod already terminal, skipping status patch");
            }
            Err(e) => return Err(DroidError::KubeApi(e)),
        }

        Ok(())
    }

    #[instrument(skip(self), fields(pod = %pod_name, ns = %namespace))]
    async fn delete_pod(&self, pod_name: &str, namespace: &str) -> Result<()> {
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), namespace);
        let params = kube::api::DeleteParams {
            grace_period_seconds: Some(0),
            ..Default::default()
        };
        match pods.delete(pod_name, &params).await {
            Ok(_) => {
                info!(pod = %pod_name, "pod object deleted");
                Ok(())
            }
            Err(kube::Error::Api(e)) if e.code == 404 => Ok(()),
            Err(e) => Err(DroidError::KubeApi(e)),
        }
    }
}
