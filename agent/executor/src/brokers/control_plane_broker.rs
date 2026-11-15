use async_trait::async_trait;
use k8s_openapi::api::core::v1::{
    Event, HostPathVolumeSource, Node, ObjectReference, PersistentVolume, PersistentVolumeClaim,
    PersistentVolumeSpec, Pod, Service,
};
use k8s_openapi::api::discovery::v1::EndpointSlice;
use k8s_openapi::api::storage::v1::StorageClass;
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::{
    api::{Api, Patch, PatchParams, PostParams},
    Client,
};
use serde_json::json;
use tracing::{debug, info, instrument, warn};

use crate::error::{DroidError, Result};
use crate::models::{NodeEvent, NodeProfile, NodeStatus};

/// A18: one incremental update from the Pod watch stream.
#[derive(Debug, Clone)]
pub enum PodWatchEvent {
    Applied(Pod),
    /// UID of the removed Pod.
    Deleted(String),
    /// Full snapshot delivered after (re)connect; replaces the cache.
    Restarted(Vec<Pod>),
}

/// Free-space threshold below which the node reports DiskPressure (C4b).
/// Every pod gets its own rootfs copy, so running out of disk is a real risk.
const DISK_PRESSURE_FREE_BYTES: u64 = 1024 * 1024 * 1024;

// ─── Trait ────────────────────────────────────────────────────────────────────

#[async_trait]
pub trait ControlPlaneBroker: Send + Sync {
    async fn register_node(&self, profile: &NodeProfile) -> Result<()>;
    async fn send_heartbeat(&self, status: &NodeStatus) -> Result<()>;
    async fn get_assigned_pods(&self) -> Result<Vec<Pod>>;

    /// A18: incremental Pod stream for this node.
    ///
    /// `Restarted` carries a full snapshot and must replace the local cache;
    /// `Applied` / `Deleted` are deltas. The caller keeps a periodic full LIST
    /// as a backstop (watch events can be lost while disconnected).
    async fn watch_assigned_pods(&self, tx: tokio::sync::mpsc::Sender<PodWatchEvent>)
        -> Result<()>;
    async fn report_pod_status(
        &self,
        pod_name: &str,
        namespace: &str,
        status: &crate::models::PodRunStatus,
    ) -> Result<()>;
    async fn delete_pod(&self, pod_name: &str, namespace: &str) -> Result<()>;
    async fn record_event(&self, event: &NodeEvent) -> Result<()>;
    async fn deregister_node(&self) -> Result<()>;

    /// G2: `kubectl cordon` equivalent — set `spec.unschedulable`.
    /// Used by the maintenance-mode endpoint so the scheduler stops placing new
    /// pods while the node is being drained; clearing it resumes scheduling.
    async fn set_unschedulable(&self, unschedulable: bool) -> Result<()>;

    /// ConfigMap data for volume mounts / `envFrom` (B9, B13).
    async fn get_config_map(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<std::collections::BTreeMap<String, String>>;

    /// Secret data (already base64-decoded) for volume mounts / `envFrom`.
    async fn get_secret(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<std::collections::BTreeMap<String, Vec<u8>>>;

    /// Services in a namespace, used to build the classic `*_SERVICE_HOST`
    /// environment variables a real kubelet injects (B13).
    async fn list_services(&self, namespace: &str) -> Result<Vec<ServiceInfo>>;

    /// D3: all Services, across namespaces, for the in-container ClusterIP proxy.
    async fn list_all_services(&self) -> Result<Vec<ServiceInfo>>;

    /// D3: ready EndpointSlice addresses grouped by service/port.
    async fn list_ready_endpoints(&self) -> Result<Vec<EndpointInfo>>;

    /// B10: host path backing a Bound PVC, resolved through its PV.
    async fn pvc_host_path(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Option<std::path::PathBuf>>;

    /// B10: create local hostPath PVs for `droidnode-local` PVCs.
    async fn reconcile_local_pvcs(&self, base: &std::path::Path) -> Result<()>;

    /// The agent's own credentials, reused to give pods a service-account token
    /// (B14): token, CA certificate, and the namespace to report.
    async fn agent_credentials(&self) -> Result<AgentCredentials>;
}

/// Minimal service view needed for env var synthesis and D3.
#[derive(Debug, Clone)]
pub struct ServiceInfo {
    pub namespace: String,
    pub name: String,
    pub cluster_ip: String,
    /// (port name, service port) for the classic `*_SERVICE_PORT` env vars.
    pub ports: Vec<(String, i32)>,
    /// (service port, numeric target port) for the loopback proxy listeners.
    pub service_ports: Vec<(i32, i32)>,
}

/// D3: ready backend address for one service port.
#[derive(Debug, Clone)]
pub struct EndpointInfo {
    pub namespace: String,
    pub service: String,
    pub port: i32,
    pub addresses: Vec<String>,
}

fn service_info(svc: Service) -> Option<ServiceInfo> {
    let namespace = svc.metadata.namespace.clone()?;
    let name = svc.metadata.name.clone()?;
    let spec = svc.spec?;
    let cluster_ip = spec.cluster_ip.unwrap_or_default();
    if cluster_ip.is_empty() || cluster_ip == "None" {
        return None;
    }
    let ports = spec.ports.unwrap_or_default();
    Some(ServiceInfo {
        namespace,
        name,
        cluster_ip,
        ports: ports
            .iter()
            .map(|p| (p.name.clone().unwrap_or_else(|| "tcp".to_string()), p.port))
            .collect(),
        service_ports: ports
            .iter()
            .map(|p| {
                let target = match p.target_port {
                    Some(IntOrString::Int(n)) => n,
                    _ => p.port,
                };
                (p.port, target)
            })
            .collect(),
    })
}

/// What we can hand to a container's projected service-account files.
#[derive(Debug, Clone)]
pub struct AgentCredentials {
    pub token: String,
    pub ca_pem: Vec<u8>,
    pub namespace: String,
}

// ─── Implementation ───────────────────────────────────────────────────────────

pub struct ControlPlaneBrokerImpl {
    client: Client,
    node_name: String,
    kubelet_port: u16,
    /// condition type -> (last status, last transition time). Used so
    /// `lastTransitionTime` only moves when a condition actually flips.
    condition_state:
        std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, (String, String)>>>,
    /// Last known DiskPressure state, so the taint is only patched on change.
    disk_pressure_active: std::sync::atomic::AtomicBool,
}

impl ControlPlaneBrokerImpl {
    pub async fn new(node_name: String, kubelet_port: u16) -> Result<Self> {
        let client = Client::try_default().await.map_err(DroidError::KubeApi)?;
        Ok(Self::with_client(node_name, kubelet_port, client))
    }

    /// Same as [`new`](Self::new) but with a caller-supplied client, so tests can
    /// point the broker at a mock API server instead of the ambient kubeconfig.
    pub fn with_client(node_name: String, kubelet_port: u16, client: Client) -> Self {
        Self {
            client,
            node_name,
            kubelet_port,
            condition_state: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            disk_pressure_active: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn local_ip() -> String {
        // DROIDNODE_NODE_IP wins when the operator needs to pin the advertised
        // address (multi-homed phone where the auto pick is still wrong).
        if let Ok(raw) = std::env::var("DROIDNODE_NODE_IP") {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        // Prefer a private LAN address (the one a cluster will reach us on);
        // the UDP trick alone can return a cellular IP on phones, and
        // Wi-Fi Direct / USB tether addresses are not routable from the cluster.
        crate::net_util::primary_ipv4()
    }

    /// Local IP used for both the Node InternalIP and `podIP`.
    ///
    /// Deliberately not cached: Wi-Fi/cellular handover changes the address,
    /// and a stale `podIP` would keep EndpointSlices pointing at the old
    /// interface. `primary_ipv4()` is a cheap getifaddrs scan.
    fn self_ip() -> String {
        Self::local_ip()
    }

    /// Build a Node object from our capability profile.
    fn build_node_object(&self, profile: &NodeProfile) -> Node {
        let memory_str = format!("{}", profile.memory.total_bytes);
        let cpu_str = format!("{}", profile.cpu.cores);

        let node = Node {
            metadata: ObjectMeta {
                name: Some(self.node_name.clone()),
                labels: Some(
                    [
                        ("kubernetes.io/hostname".into(), self.node_name.clone()),
                        ("kubernetes.io/arch".into(), profile.cpu.arch.clone()),
                        ("kubernetes.io/os".into(), "linux".into()),
                        ("droidnode/managed".into(), "true".into()),
                        // D9: svclb's DaemonSet excludes nodes with this label.
                        // It used to be applied by hand, so a re-created Node lost
                        // it and the svclb pod landed on the phone again.
                        ("droidnode/proot".into(), "true".into()),
                    ]
                    .into(),
                ),
                ..Default::default()
            },
            ..Default::default()
        };

        // Status is set via a PATCH after creation
        let _ = memory_str;
        let _ = cpu_str;

        node
    }

    /// Add/remove `node.kubernetes.io/disk-pressure:NoSchedule` (C4b).
    ///
    /// Inherent helper (not part of the broker contract) used by
    /// `send_heartbeat` so the scheduler stops placing pods on a full disk.
    async fn sync_disk_pressure_taint(&self, pressured: bool) {
        // Only touch the API when the state actually changes: doing a GET+patch on
        // every 15 s heartbeat was enough to stall the registration task.
        let previous = self
            .disk_pressure_active
            .swap(pressured, std::sync::atomic::Ordering::Relaxed);
        if previous == pressured {
            return;
        }

        let nodes: Api<Node> = Api::all(self.client.clone());
        let key = "node.kubernetes.io/disk-pressure";

        let current = match nodes.get(&self.node_name).await {
            Ok(n) => n,
            Err(_) => return,
        };
        let mut taints = current
            .spec
            .as_ref()
            .and_then(|s| s.taints.clone())
            .unwrap_or_default();
        let has = taints.iter().any(|t| t.key == key);

        if pressured && !has {
            taints.push(k8s_openapi::api::core::v1::Taint {
                key: key.to_string(),
                value: None,
                effect: "NoSchedule".to_string(),
                time_added: None,
            });
            warn!(node = %self.node_name, "disk pressure: tainting node NoSchedule");
        } else if !pressured && has {
            taints.retain(|t| t.key != key);
            info!(node = %self.node_name, "disk pressure cleared: removing taint");
        } else {
            return;
        }

        let patch = json!({ "spec": { "taints": taints } });
        if let Err(e) = nodes
            .patch(
                &self.node_name,
                &PatchParams::default(),
                &Patch::Merge(&patch),
            )
            .await
        {
            warn!(error = %e, "could not update disk-pressure taint");
        }
    }
}

#[async_trait]
impl ControlPlaneBroker for ControlPlaneBrokerImpl {
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
        // C4b: DiskPressure is real here - every pod gets its own rootfs copy
        // (B17), so the phone's data partition fills up. Below the threshold we
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
        let max_pods = std::env::var("DROIDNODE_MAX_PODS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(20)
            .to_string();

        // G1: `ephemeral-storage` is the kubelet-side capacity for the image /
        // pod-data partition. We only had free bytes before, so the scheduler
        // could not account for emptyDir/logs/rootfs copies at all (B17 copies
        // a full rootfs per pod). Allocatable reserves ~10% for the OS.
        let storage_total = status.storage.total_bytes;
        let ephemeral_capacity = format!("{}Ki", storage_total / 1024);
        let ephemeral_allocatable = format!("{}Ki", (storage_total / 10 * 9) / 1024);
        let filesystems = json!([{
            "name": "rootfs",
            "mountpoint": "/",
            "capacityBytes": storage_total,
            "availableBytes": status.storage.available_bytes
        }]);
        // G1: `kubectl describe node` renders this as the "Images:" section.
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

        // C4b: mirror the condition as the standard taint so the scheduler stops
        // placing new pods while the disk is full.
        self.sync_disk_pressure_taint(disk_pressure == "True").await;

        Ok(())
    }

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

    #[instrument(skip(self, event), fields(node = %self.node_name))]
    async fn record_event(&self, event: &NodeEvent) -> Result<()> {
        use chrono::Utc;

        let namespace = event
            .pod
            .as_ref()
            .map(|p| p.namespace.clone())
            .unwrap_or_else(|| "default".to_string());
        let events: Api<Event> = Api::namespaced(self.client.clone(), &namespace);

        let event_type = match event.event_type {
            crate::models::EventType::Normal => "Normal",
            crate::models::EventType::Warning => "Warning",
        };

        // involvedObject is what makes `kubectl describe pod` / `kubectl get
        // events --field-selector involvedObject.name=...` find the event.
        let involved = event
            .pod
            .as_ref()
            .map(|p| k8s_openapi::api::core::v1::ObjectReference {
                api_version: Some("v1".to_string()),
                kind: Some("Pod".to_string()),
                name: Some(p.name.clone()),
                namespace: Some(p.namespace.clone()),
                uid: p.uid.clone(),
                ..Default::default()
            });
        let pod_name = event.pod.as_ref().map(|p| p.name.clone());

        let ev = Event {
            metadata: ObjectMeta {
                // Prefer a human-readable name so `kubectl get events` is legible.
                name: pod_name
                    .as_ref()
                    .map(|n| format!("{}.{:x}", n, Utc::now().timestamp_millis())),
                generate_name: if pod_name.is_none() {
                    Some(format!("{}-", self.node_name))
                } else {
                    None
                },
                namespace: Some(namespace.clone()),
                ..Default::default()
            },
            involved_object: involved.unwrap_or_default(),
            event_time: Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime(
                Utc::now(),
            )),
            action: Some(event.reason.clone()),
            reason: Some(event.reason.clone()),
            message: Some(event.message.clone()),
            type_: Some(event_type.into()),
            reporting_component: Some("droidnode".into()),
            reporting_instance: Some(self.node_name.clone()),
            ..Default::default()
        };

        events
            .create(&PostParams::default(), &ev)
            .await
            .map_err(DroidError::KubeApi)?;

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
    /// forever (B28).
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

    #[instrument(skip(self), fields(ns = %namespace, name = %name))]
    async fn get_config_map(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<std::collections::BTreeMap<String, String>> {
        use k8s_openapi::api::core::v1::ConfigMap;
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), namespace);
        let cm = api.get(name).await.map_err(DroidError::KubeApi)?;
        Ok(cm.data.unwrap_or_default())
    }

    #[instrument(skip(self), fields(ns = %namespace, name = %name))]
    async fn get_secret(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<std::collections::BTreeMap<String, Vec<u8>>> {
        use k8s_openapi::api::core::v1::Secret;

        let api: Api<Secret> = Api::namespaced(self.client.clone(), namespace);
        let secret = api.get(name).await.map_err(DroidError::KubeApi)?;

        let mut out = std::collections::BTreeMap::new();
        if let Some(data) = secret.data {
            for (k, v) in data {
                out.insert(k, v.0.clone());
            }
        }
        // `stringData` is write-only in the API, but decode it if present anyway.
        if let Some(string_data) = secret.string_data {
            for (k, v) in string_data {
                out.insert(k, v.into_bytes());
            }
        }
        // k8s-openapi gives us the already-decoded `ByteString` values; no
        // base64 handling is needed here.
        Ok(out)
    }

    #[instrument(skip(self), fields(ns = %namespace))]
    async fn list_services(&self, namespace: &str) -> Result<Vec<ServiceInfo>> {
        let api: Api<Service> = Api::namespaced(self.client.clone(), namespace);
        let list = api
            .list(&Default::default())
            .await
            .map_err(DroidError::KubeApi)?;
        Ok(list.items.into_iter().filter_map(service_info).collect())
    }

    async fn list_all_services(&self) -> Result<Vec<ServiceInfo>> {
        let api: Api<Service> = Api::all(self.client.clone());
        let list = api
            .list(&Default::default())
            .await
            .map_err(DroidError::KubeApi)?;
        Ok(list.items.into_iter().filter_map(service_info).collect())
    }

    async fn list_ready_endpoints(&self) -> Result<Vec<EndpointInfo>> {
        let api: Api<EndpointSlice> = Api::all(self.client.clone());
        let list = api
            .list(&Default::default())
            .await
            .map_err(DroidError::KubeApi)?;
        let mut grouped: std::collections::HashMap<(String, String, i32), Vec<String>> =
            std::collections::HashMap::new();
        for slice in list.items {
            let Some(service) = slice
                .metadata
                .labels
                .as_ref()
                .and_then(|l| l.get("kubernetes.io/service-name"))
                .cloned()
            else {
                continue;
            };
            let namespace = slice.metadata.namespace.clone().unwrap_or_default();
            let Some(ports) = slice.ports else { continue };
            for port in ports {
                let Some(number) = port.port else { continue };
                for endpoint in &slice.endpoints {
                    if endpoint.conditions.as_ref().and_then(|c| c.ready) == Some(false) {
                        continue;
                    }
                    let entry = grouped
                        .entry((namespace.clone(), service.clone(), number))
                        .or_default();
                    for address in &endpoint.addresses {
                        if !entry.contains(address) {
                            entry.push(address.clone());
                        }
                    }
                }
            }
        }
        Ok(grouped
            .into_iter()
            .map(|((namespace, service, port), addresses)| EndpointInfo {
                namespace,
                service,
                port,
                addresses,
            })
            .collect())
    }

    async fn pvc_host_path(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Option<std::path::PathBuf>> {
        let pvc_api: Api<PersistentVolumeClaim> = Api::namespaced(self.client.clone(), namespace);
        let pvc = match pvc_api.get(name).await {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        let Some(volume_name) = pvc.spec.and_then(|s| s.volume_name) else {
            return Ok(None);
        };
        let pv_api: Api<PersistentVolume> = Api::all(self.client.clone());
        let pv = match pv_api.get(&volume_name).await {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        Ok(pv
            .spec
            .and_then(|s| s.host_path)
            .map(|h| std::path::PathBuf::from(h.path)))
    }

    async fn reconcile_local_pvcs(&self, base: &std::path::Path) -> Result<()> {
        let pv_api: Api<PersistentVolume> = Api::all(self.client.clone());
        let pvc_api: Api<PersistentVolumeClaim> = Api::all(self.client.clone());
        let sc_api: Api<StorageClass> = Api::all(self.client.clone());

        if sc_api.get("droidnode-local").await.is_err() {
            let sc = StorageClass {
                metadata: ObjectMeta {
                    name: Some("droidnode-local".into()),
                    ..Default::default()
                },
                provisioner: "droidnode.io/local".into(),
                reclaim_policy: Some("Retain".into()),
                volume_binding_mode: Some("Immediate".into()),
                ..Default::default()
            };
            if let Err(e) = sc_api.create(&PostParams::default(), &sc).await {
                debug!(error = %e, "B10: could not create StorageClass (may already exist)");
            }
        }

        let list = pvc_api
            .list(&Default::default())
            .await
            .map_err(DroidError::KubeApi)?;
        for pvc in list.items {
            if pvc
                .spec
                .as_ref()
                .and_then(|s| s.storage_class_name.as_deref())
                != Some("droidnode-local")
            {
                continue;
            }
            let Some(uid) = pvc.metadata.uid.clone() else {
                continue;
            };
            let namespace = pvc.metadata.namespace.clone().unwrap_or_default();
            let pvc_name = pvc.metadata.name.clone().unwrap_or_default();
            let dir = base.join(&uid);
            tokio::fs::create_dir_all(&dir).await?;

            let pv_name = format!("droidnode-{uid}");
            if pv_api.get(&pv_name).await.is_ok() {
                continue;
            }
            let spec = PersistentVolumeSpec {
                capacity: Some(
                    [("storage".to_string(), Quantity("1Gi".to_string()))]
                        .into_iter()
                        .collect(),
                ),
                access_modes: Some(vec!["ReadWriteOnce".to_string()]),
                persistent_volume_reclaim_policy: Some("Retain".to_string()),
                storage_class_name: Some("droidnode-local".to_string()),
                host_path: Some(HostPathVolumeSource {
                    path: dir.to_string_lossy().to_string(),
                    type_: Some("DirectoryOrCreate".to_string()),
                }),
                claim_ref: Some(ObjectReference {
                    api_version: Some("v1".into()),
                    kind: Some("PersistentVolumeClaim".into()),
                    namespace: Some(namespace.clone()),
                    name: Some(pvc_name.clone()),
                    uid: Some(uid.clone()),
                    ..Default::default()
                }),
                volume_mode: Some("Filesystem".into()),
                ..Default::default()
            };
            let pv = PersistentVolume {
                metadata: ObjectMeta {
                    name: Some(pv_name.clone()),
                    ..Default::default()
                },
                spec: Some(spec),
                ..Default::default()
            };
            match pv_api.create(&PostParams::default(), &pv).await {
                Ok(_) => info!(
                    pv = %pv_name,
                    pvc = %format!("{namespace}/{pvc_name}"),
                    path = %dir.display(),
                    "B10: created local PV"
                ),
                Err(e) => warn!(pv = %pv_name, error = %e, "B10: PV create failed"),
            }
        }
        Ok(())
    }

    async fn agent_credentials(&self) -> Result<AgentCredentials> {
        // Reuse the kubeconfig the agent already runs with: token + CA.
        let path = std::env::var("KUBECONFIG").map_err(|_| {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        BatteryInfo, CpuInfo, EventType, MemoryInfo, NetworkInfo, NodeConditions, NodeEvent,
        NodeProfile, NodeStatus, RuntimeInfo, StorageInfo,
    };

    fn test_profile(node_id: &str) -> NodeProfile {
        NodeProfile {
            node_id: node_id.into(),
            cpu: CpuInfo {
                arch: "amd64".into(),
                cores: 4,
            },
            memory: MemoryInfo {
                total_bytes: 8 * 1024 * 1024 * 1024,
                available_bytes: 4 * 1024 * 1024 * 1024,
            },
            storage: StorageInfo {
                total_bytes: 64 * 1024 * 1024 * 1024,
                available_bytes: 10 * 1024 * 1024 * 1024,
            },
            runtime: RuntimeInfo {
                name: "proot-oci-runner".into(),
                version: "0.1.0".into(),
            },
            battery: BatteryInfo {
                percent: 100,
                charging: true,
            },
            network: NetworkInfo {
                network_type: "wifi".into(),
            },
            conditions: NodeConditions {
                ready: true,
                battery_pressure: false,
                memory_pressure: false,
                network_available: true,
            },
            images: vec![],
        }
    }

    #[tokio::test]
    async fn test_register_heartbeat_get_pods_deregister() {
        if std::env::var("KUBECONFIG").is_err()
            && !std::path::Path::new("/etc/rancher/k3s/k3s.yaml").exists()
        {
            eprintln!("KUBECONFIG not set and k3s not found — skipping control plane test");
            return;
        }

        let node_name = "droidnode-test-broker".to_string();
        let broker = ControlPlaneBrokerImpl::new(node_name.clone(), 10255)
            .await
            .unwrap();
        let profile = test_profile(&node_name);

        // Register
        broker.register_node(&profile).await.unwrap();
        println!("node registered: {node_name}");

        // Heartbeat
        let status = NodeStatus {
            node_id: node_name.clone(),
            conditions: profile.conditions.clone(),
            memory: profile.memory.clone(),
            storage: profile.storage.clone(),
            battery: profile.battery.clone(),
            network: profile.network.clone(),
            images: profile.images.clone(),
        };
        broker.send_heartbeat(&status).await.unwrap();
        println!("heartbeat sent");

        // Assigned pods — should be empty for a fresh test node
        let pods = broker.get_assigned_pods().await.unwrap();
        println!("assigned pods: {}", pods.len());
        assert!(pods.is_empty(), "expected no pods assigned to test node");

        // Record an event
        broker
            .record_event(&NodeEvent {
                node_id: node_name.clone(),
                reason: "TestEvent".into(),
                message: "control plane broker integration test".into(),
                event_type: EventType::Normal,
                pod_id: None,
                pod: None,
            })
            .await
            .unwrap();
        println!("event recorded");

        // Deregister — Ok(()) return is sufficient; k8s deletion is async
        broker.deregister_node().await.unwrap();
        println!("node deregistered");
    }
    #[test]
    fn condition_transition_time_is_stable_until_status_flips() {
        let mut prev = std::collections::HashMap::new();
        let ready = [("Ready", "True", "DroidNodeReady", "ok")];
        let first = condition_patch(&mut prev, &ready, "T1");
        assert_eq!(first[0]["lastTransitionTime"], "T1");

        // Same status, later heartbeat: transition must not move.
        let second = condition_patch(&mut prev, &ready, "T2");
        assert_eq!(second[0]["lastTransitionTime"], "T1");
        assert_eq!(second[0]["lastHeartbeatTime"], "T2");

        // Flip: transition moves to the flip time.
        let down = [("Ready", "False", "DroidNodeReady", "down")];
        let third = condition_patch(&mut prev, &down, "T3");
        assert_eq!(third[0]["lastTransitionTime"], "T3");
    }

    #[test]
    fn new_condition_starts_at_now() {
        let mut prev = std::collections::HashMap::new();
        let out = condition_patch(
            &mut prev,
            &[("DiskPressure", "False", "DroidNodeDisk", "")],
            "T9",
        );
        assert_eq!(out[0]["type"], "DiskPressure");
        assert_eq!(out[0]["lastTransitionTime"], "T9");
    }
}
