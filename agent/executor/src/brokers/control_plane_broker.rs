use async_trait::async_trait;
use k8s_openapi::api::core::v1::{Node, Pod, Service};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::{
    api::{Api, Patch, PatchParams},
    Client,
};
use serde_json::json;
use tracing::{info, warn};

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
pub trait NodeApi: Send + Sync {
    async fn register_node(&self, profile: &NodeProfile) -> Result<()>;
    async fn send_heartbeat(&self, status: &NodeStatus) -> Result<()>;
    async fn set_unschedulable(&self, unschedulable: bool) -> Result<()>;
    async fn deregister_node(&self) -> Result<()>;
    async fn agent_credentials(&self) -> Result<AgentCredentials>;
}

#[async_trait]
pub trait PodWatch: Send + Sync {
    async fn get_assigned_pods(&self) -> Result<Vec<Pod>>;
    async fn watch_assigned_pods(&self, tx: tokio::sync::mpsc::Sender<PodWatchEvent>)
        -> Result<()>;
    async fn report_pod_status(
        &self,
        pod_name: &str,
        namespace: &str,
        status: &crate::models::PodRunStatus,
    ) -> Result<()>;
    async fn delete_pod(&self, pod_name: &str, namespace: &str) -> Result<()>;
}

#[async_trait]
pub trait EventRecorder: Send + Sync {
    async fn record_event(&self, event: &NodeEvent) -> Result<()>;
}

#[async_trait]
pub trait PvcApi: Send + Sync {
    async fn get_config_map(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<std::collections::BTreeMap<String, String>>;
    async fn get_secret(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<std::collections::BTreeMap<String, Vec<u8>>>;
    async fn list_services(&self, namespace: &str) -> Result<Vec<ServiceInfo>>;
    async fn list_all_services(&self) -> Result<Vec<ServiceInfo>>;
    async fn list_ready_endpoints(&self) -> Result<Vec<EndpointInfo>>;
    async fn pvc_host_path(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Option<std::path::PathBuf>>;
    async fn reconcile_local_pvcs(&self, base: &std::path::Path) -> Result<()>;
}

#[async_trait]
pub trait ControlPlaneBroker: NodeApi + PodWatch + EventRecorder + PvcApi + Send + Sync {}

impl<T> ControlPlaneBroker for T where T: NodeApi + PodWatch + EventRecorder + PvcApi + Send + Sync {}

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
        if let Some(ip) = crate::config::get().node_ip() {
            return ip.to_string();
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

#[path = "control_plane/events.rs"]
mod events;
#[path = "control_plane/node.rs"]
mod node;
#[path = "control_plane/pods.rs"]
mod pods;
#[path = "control_plane/pvc.rs"]
mod pvc;

#[cfg(test)]
mod tests {
    use super::node::condition_patch;
    use super::*;
    use crate::models::{
        BatteryInfo, CpuInfo, EventType, MemoryInfo, NetworkInfo, NodeConditions, NodeEvent,
        NodeProfile, NodeStatus, RuntimeInfo, StorageInfo,
    };

    pub(crate) fn test_profile(node_id: &str) -> NodeProfile {
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

// ── mock API-server tests (no cluster needed) ─────────────────────────
#[cfg(test)]
mod mock_api {
    use super::tests::test_profile;
    use super::*;
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client_for(server: &MockServer) -> Client {
        // `Config::new` wants the http 0.2 Uri that kube-client uses; let
        // inference pick it rather than importing a second `http` crate.
        Client::try_from(kube::Config::new(server.uri().parse().expect("uri"))).expect("client")
    }

    async fn empty_ok_server() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn register_posts_node_with_labels() {
        let server = empty_ok_server().await;
        let broker =
            ControlPlaneBrokerImpl::with_client("dn-test".into(), 10255, client_for(&server));
        broker
            .register_node(&test_profile("dn-test"))
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        let post = requests
            .iter()
            .find(|r| r.method == wiremock::http::Method::POST && r.url.path() == "/api/v1/nodes")
            .expect("POST /api/v1/nodes");
        let body: serde_json::Value = serde_json::from_slice(&post.body).unwrap();
        assert_eq!(body["metadata"]["name"], "dn-test");
        assert_eq!(body["metadata"]["labels"]["droidnode/managed"], "true");
        assert_eq!(body["metadata"]["labels"]["droidnode/proot"], "true");
        assert_eq!(body["metadata"]["labels"]["kubernetes.io/os"], "linux");
    }

    #[tokio::test]
    async fn heartbeat_patches_ready_condition() {
        let server = empty_ok_server().await;
        let broker =
            ControlPlaneBrokerImpl::with_client("dn-test".into(), 10255, client_for(&server));
        let profile = test_profile("dn-test");
        let status = NodeStatus {
            node_id: profile.node_id.clone(),
            conditions: profile.conditions.clone(),
            memory: profile.memory.clone(),
            storage: profile.storage.clone(),
            battery: profile.battery.clone(),
            network: profile.network.clone(),
            images: profile.images.clone(),
        };
        broker.send_heartbeat(&status).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let patch = requests
            .iter()
            .find(|r| {
                r.method == wiremock::http::Method::PATCH && r.url.path().ends_with("/status")
            })
            .expect("PATCH .../status");
        let body: serde_json::Value = serde_json::from_slice(&patch.body).unwrap();
        let conditions = body["status"]["conditions"].as_array().expect("conditions");
        let ready = conditions
            .iter()
            .find(|c| c["type"] == "Ready")
            .expect("Ready condition");
        assert_eq!(ready["status"], "True");
        assert!(ready["lastHeartbeatTime"].is_string());
        assert_eq!(
            body["status"]["daemonEndpoints"]["kubeletEndpoint"]["Port"],
            10255
        );
    }
}
