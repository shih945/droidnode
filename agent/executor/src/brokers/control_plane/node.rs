use async_trait::async_trait;
use k8s_openapi::api::core::v1::Node;
use kube::{
    api::{Api, Patch, PatchParams, PostParams},
    Client,
};
use serde_json::json;
use tracing::{debug, info, instrument, warn};

use crate::error::{DroidError, Result};
use crate::models::{NodeProfile, NodeStatus};

use super::{AgentCredentials, ControlPlaneBrokerImpl, NodeApi, DISK_PRESSURE_FREE_BYTES};

#[async_trait]
impl NodeApi for ControlPlaneBrokerImpl {
    #[instrument(skip(self, profile), fields(node = %self.node_name))]
    async fn register_node(&self, profile: &NodeProfile) -> Result<()> {
        let nodes: Api<Node> = Api::all(self.client.clone());
        let node_obj = self.build_node_object(profile);

        // Create or replace
        match nodes.create(&PostParams::default(), &node_obj).await {
            Ok(_) => {}
            Err(kube::Error::Api(e)) if e.code == 409 => {
                // Node already exists — update status on the next heartbeat.
                // Re-assert our labels: a Node re-created by `kubectl delete node`
                // gets them from build_node_object, but nodes created by older
                // builds may be missing `droidnode/proot`, which lets svclb in.
                let labels = json!({
                    "metadata": { "labels": {
                        "droidnode/managed": "true",
                        "droidnode/proot": "true"
                    }}
                });
                if let Err(patch_err) = nodes
                    .patch(
                        &self.node_name,
                        &PatchParams::default(),
                        &Patch::Merge(&labels),
                    )
                    .await
                {
                    debug!(node = %self.node_name, error = %patch_err, "could not re-assert node labels");
                }
                debug!(node = %self.node_name, "node already registered");
            }
            Err(e) => return Err(DroidError::KubeApi(e)),
        }

        // Immediately post the full status so the node appears Ready
        self.send_heartbeat(&NodeStatus {
            node_id: profile.node_id.clone(),
            conditions: profile.conditions.clone(),
            memory: profile.memory.clone(),
            storage: profile.storage.clone(),
            battery: profile.battery.clone(),
            network: profile.network.clone(),
            images: profile.images.clone(),
        })
        .await?;

        check_cluster_version(&self.client, &self.node_name).await;
        Ok(())
    }

    #[instrument(skip(self, status), fields(node = %self.node_name))]
    async fn send_heartbeat(&self, status: &NodeStatus) -> Result<()> {
        use chrono::Utc;

        let nodes: Api<Node> = Api::all(self.client.clone());

        let ready_status = if status.conditions.ready {
            "True"
        } else {
            "False"
        };
        let mem_pressure = if status.conditions.memory_pressure {
            "True"
        } else {
            "False"
        };
        let net_unavail = if status.conditions.network_available {
            "False"
        } else {
            "True"
        };

        // k8s node-lifecycle-controller uses lastHeartbeatTime to decide readiness.
        // Without it (or with a stale value), the node stays NotReady regardless of
        // the Ready condition status field.
        let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

        // `lastTransitionTime` must only move when the condition's status actually
        // flips; a real kubelet keeps it stable otherwise. Rewriting it every 15s
        // made `kubectl describe node` useless for "when did this node go NotReady".
        // DiskPressure is real here - every pod gets its own rootfs copy
        //, so the phone's data partition fills up. Below the threshold we
        // report the condition AND add the standard taint, so the scheduler stops
        // placing pods instead of silently failing on a full disk.
        let free_bytes = status.storage.available_bytes;
        let disk_pressure = if free_bytes < DISK_PRESSURE_FREE_BYTES {
            "True"
        } else {
            "False"
        };
        let disk_pressure_msg = if disk_pressure == "True" {
            format!(
                "only {} MiB free (threshold {} MiB)",
                free_bytes / (1024 * 1024),
                DISK_PRESSURE_FREE_BYTES / (1024 * 1024)
            )
        } else {
            String::new()
        };

        let conditions: Vec<(&str, &str, &str, &str)> = vec![
            (
                "Ready",
                ready_status,
                "DroidNodeReady",
                "droidnode agent is running",
            ),
            ("MemoryPressure", mem_pressure, "DroidNodeMemory", ""),
            ("NetworkUnavailable", net_unavail, "DroidNodeNetwork", ""),
            (
                "DiskPressure",
                disk_pressure,
                "DroidNodeDisk",
                &disk_pressure_msg,
            ),
            ("PIDPressure", "False", "DroidNodePID", ""),
        ];
        let condition_json: Vec<serde_json::Value> = {
            let mut prev = self.condition_state.lock().await;
            condition_patch(&mut prev, &conditions, &now)
        };

        // Report the device's real core count instead of a hardcoded "1":
        // the scheduler otherwise refuses pods that request more than one core
        // even though the phone has plenty. The pod count cap is configurable
        // because every pod here is a proot process on the same kernel.
        let cpu_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .to_string();
        let max_pods = crate::config::get().max_pods().to_string();

        // `ephemeral-storage` is the kubelet-side capacity for the image /
        // pod-data partition. We only had free bytes before, so the scheduler
        // could not account for emptyDir/logs/rootfs copies at all (every pod
        // gets its own full rootfs copy). Allocatable reserves ~10% for the OS.
        let storage_total = status.storage.total_bytes;
        let ephemeral_capacity = format!("{}Ki", storage_total / 1024);
        let ephemeral_allocatable = format!("{}Ki", (storage_total / 10 * 9) / 1024);
        let filesystems = json!([{
            "name": "rootfs",
            "mountpoint": "/",
            "capacityBytes": storage_total,
            "availableBytes": status.storage.available_bytes
        }]);
        // `kubectl describe node` renders this as the "Images:" section.
        // Size is compressed layers + unpacked rootfs (an upper bound: layers
        // are shared between images).
        let images: Vec<serde_json::Value> = status
            .images
            .iter()
            .map(|image| {
                json!({
                    "names": image.names,
                    "sizeBytes": image.size_bytes
                })
            })
            .collect();

        let patch = json!({
            "status": {
                "conditions": condition_json,
                // Not part of core/v1 NodeStatus (the apiserver prunes it), but
                // it is what kubelet's Summary API exposes and what dashboards
                // read for per-filesystem capacity. Kept best-effort.
                "filesystems": filesystems,
                "images": images,
                "addresses": [
                    { "type": "InternalIP", "address": Self::local_ip() },
                    { "type": "Hostname",   "address": &self.node_name }
                ],
                "daemonEndpoints": {
                    "kubeletEndpoint": { "Port": self.kubelet_port }
                },
                "nodeInfo": {
                    "machineID": &self.node_name,
                    "systemUUID": &self.node_name,
                    "bootID": &self.node_name,
                    "kernelVersion": "linux-android",
                    "osImage": "Android (proot-oci-runner)",
                    "containerRuntimeVersion": "proot://0.1.0",
                    "kubeletVersion": crate::models::KUBELET_VERSION,
                    "kubeProxyVersion": crate::models::KUBELET_VERSION,
                    "operatingSystem": "linux",
                    "architecture": if cfg!(target_arch = "aarch64") { "arm64" } else { "amd64" }
                },
                // KEP-4006: advertise that this kubelet terminates the
                // WebSocket streaming protocols itself. Without this the
                // apiserver falls back to SPDY (which proot does not speak).
                "declaredFeatures": [
                    "KubeletPortForwardWebsockets",
                    "KubeletWebSocketStreaming"
                ],
                "allocatable": {
                    "cpu": cpu_cores,
                    // Stable number, not "free memory right now": kubectl top
                    // computes usage/allocatable, and a fluctuating denominator
                    // produced nonsense like 255%. Reserve ~10% for the OS.
                    "memory": format!("{}Ki", (status.memory.total_bytes / 10 * 9) / 1024),
                    "ephemeral-storage": ephemeral_allocatable,
                    "pods": max_pods
                },
                "capacity": {
                    "cpu": cpu_cores,
                    "memory": format!("{}Ki", status.memory.total_bytes / 1024),
                    "ephemeral-storage": ephemeral_capacity,
                    "pods": max_pods
                }
            }
        });

        nodes
            .patch_status(
                &self.node_name,
                &PatchParams::apply("droidnode"),
                &Patch::Merge(&patch),
            )
            .await
            .map_err(DroidError::KubeApi)?;

        // Mirror the condition as the standard taint so the scheduler stops
        // placing new pods while the disk is full.
        self.sync_disk_pressure_taint(disk_pressure == "True").await;

        Ok(())
    }

    #[instrument(skip(self), fields(node = %self.node_name))]
    async fn set_unschedulable(&self, unschedulable: bool) -> Result<()> {
        let nodes: Api<Node> = Api::all(self.client.clone());
        let patch = json!({ "spec": { "unschedulable": unschedulable } });
        nodes
            .patch(
                &self.node_name,
                &PatchParams::default(),
                &Patch::Merge(&patch),
            )
            .await
            .map_err(DroidError::KubeApi)?;
        info!(node = %self.node_name, unschedulable, "node schedulability updated");
        Ok(())
    }

    #[instrument(skip(self), fields(node = %self.node_name))]
    async fn deregister_node(&self) -> Result<()> {
        use chrono::Utc;
        // Do NOT delete the Node object: kube-apiserver keeps pods of a deleted
        // node in Terminating forever, and the scheduler may reschedule them in
        // confusing ways. Marking the node NotReady is what a real kubelet does
        // when it stops; deletion stays an explicit operator action.
        let nodes: Api<Node> = Api::all(self.client.clone());
        let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let patch = json!({
            "status": {
                "conditions": [{
                    "type": "Ready",
                    "status": "False",
                    "reason": "DroidNodeStopped",
                    "message": "droidnode agent stopped",
                    "lastHeartbeatTime": now,
                    "lastTransitionTime": now
                }]
            }
        });
        match nodes
            .patch_status(
                &self.node_name,
                &PatchParams::default(),
                &Patch::Merge(&patch),
            )
            .await
        {
            Ok(_) => info!(node = %self.node_name, "node marked NotReady (not deleted)"),
            Err(e) => debug!(node = %self.node_name, error = %e, "could not mark node NotReady"),
        }
        Ok(())
    }

    /// Delete a pod object after its containers are confirmed stopped.
    ///
    /// A real kubelet sends this with gracePeriodSeconds=0 once the sandbox is
    /// torn down. Without the explicit 0 the apiserver only marks the object as
    /// "deleting" and nothing ever removes it, so the pod sits in Terminating
    /// forever.

    async fn agent_credentials(&self) -> Result<AgentCredentials> {
        // Reuse the kubeconfig the agent already runs with: token + CA.
        let path = crate::config::kubeconfig_path().ok_or_else(|| {
            DroidError::Config("KUBECONFIG is not set; cannot provide a pod token".into())
        })?;
        let text = std::fs::read_to_string(&path)?;

        let mut token = String::new();
        let mut ca_b64 = String::new();
        let mut namespace = "default".to_string();
        for line in text.lines() {
            let t = line.trim();
            if let Some(v) = t.strip_prefix("token:") {
                token = v.trim().trim_matches('"').to_string();
            } else if let Some(v) = t.strip_prefix("certificate-authority-data:") {
                ca_b64 = v.trim().trim_matches('"').to_string();
            } else if let Some(v) = t.strip_prefix("namespace:") {
                let ns = v.trim().trim_matches('"');
                if !ns.is_empty() {
                    namespace = ns.to_string();
                }
            }
        }

        if token.is_empty() {
            return Err(DroidError::Config(
                "kubeconfig has no token; cannot provide a pod token".into(),
            ));
        }

        let ca_pem = if ca_b64.is_empty() {
            Vec::new()
        } else {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(ca_b64.trim())
                .unwrap_or_default()
        };

        Ok(AgentCredentials {
            token,
            ca_pem,
            namespace,
        })
    }
}

/// Record the control plane version on the Node and warn when it is outside the
/// range this build was compiled for.
///
/// We report `KUBELET_VERSION` (1.29, the schema we are built against) and put
/// the real cluster version in an annotation, so the node never lies about the
/// control plane while staying honest about what we implement.
async fn check_cluster_version(client: &Client, node_name: &str) {
    let version = match client.apiserver_version().await {
        Ok(v) => v,
        Err(e) => {
            debug!(error = %e, "could not read apiserver version");
            return;
        }
    };
    let git = version.git_version.clone();

    // "v1.36.5+k3s1" -> 36
    let server_minor = git
        .trim_start_matches('v')
        .split('.')
        .nth(1)
        .and_then(|m| m.parse::<u32>().ok());

    match server_minor {
        Some(minor) if minor > crate::models::KUBELET_MINOR => {
            warn!(
                cluster = %git,
                kubelet = crate::models::KUBELET_VERSION,
                skew = minor - crate::models::KUBELET_MINOR,
                "control plane is newer than the API version this build targets; \
                 fields added after 1.29 are not surfaced (by design)"
            );
        }
        Some(minor) if minor < crate::models::KUBELET_MINOR => {
            warn!(
                cluster = %git,
                kubelet = crate::models::KUBELET_VERSION,
                "control plane is older than this build targets"
            );
        }
        _ => {}
    }

    let nodes: Api<Node> = Api::all(client.clone());
    let patch = json!({
        "metadata": { "annotations": { "droidnode.io/cluster-version": git } }
    });
    if let Err(e) = nodes
        .patch(node_name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
    {
        debug!(error = %e, "could not annotate cluster version");
    }
}

/// Build the `conditions` array for a heartbeat patch. `lastTransitionTime`
/// only moves when the condition status actually flips; rewriting it every
/// heartbeat made `kubectl describe node` useless for "when did it go NotReady".
pub(crate) fn condition_patch(
    prev: &mut std::collections::HashMap<String, (String, String)>,
    conditions: &[(&str, &str, &str, &str)],
    now: &str,
) -> Vec<serde_json::Value> {
    conditions
        .iter()
        .map(|(type_, st, reason, message)| {
            let transition = match prev.get(*type_) {
                Some((old_status, old_time)) if old_status == st => old_time.clone(),
                _ => {
                    prev.insert(type_.to_string(), (st.to_string(), now.to_string()));
                    now.to_string()
                }
            };
            json!({
                "type": type_,
                "status": st,
                "reason": reason,
                "message": message,
                "lastHeartbeatTime": now,
                "lastTransitionTime": transition
            })
        })
        .collect()
}
