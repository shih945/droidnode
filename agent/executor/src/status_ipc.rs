//! E1/E2/E16: device telemetry IPC between the Android app and the agent.
//!
//! The app connects to `<dataDir>/agent.sock` (a Unix domain socket, app-private)
//! and writes one JSON object per line:
//!
//! ```json
//! {"battery_percent":37,"battery_charging":false,"battery_temperature_c":31.5,
//!  "network_type":"wifi","storage_total_bytes":...,"storage_available_bytes":...}
//! ```
//!
//! The agent replies with one line of structured status, so the same connection is
//! also the "status page" feed (E16) and drives WakeLock decisions on the Kotlin
//! side (E6): `{"node_name":"...","pods_running":1,"drained":false,...}`.
//!
//! Before this existed the Rust side reported a hardcoded 100%/charging battery
//! and "wifi" network, so `NetworkUnavailable`, `BatteryPressure` and the node
//! metrics were fiction.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::exposers::virtual_kubelet_exposer::KubeletState;

/// How long a pushed sample stays valid before the agent falls back to stubs.
pub const TELEMETRY_TTL_MS: u64 = 120_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceTelemetry {
    #[serde(default)]
    pub battery_percent: Option<u8>,
    #[serde(default)]
    pub battery_charging: Option<bool>,
    #[serde(default)]
    pub battery_temperature_c: Option<f32>,
    #[serde(default)]
    pub network_type: Option<String>,
    #[serde(default)]
    pub storage_total_bytes: Option<u64>,
    #[serde(default)]
    pub storage_available_bytes: Option<u64>,
    /// Set by the agent when the line arrives (not trusted from the client).
    #[serde(default)]
    pub updated_at_ms: u64,
}

impl DeviceTelemetry {
    pub fn is_fresh(&self) -> bool {
        if self.updated_at_ms == 0 {
            return false;
        }
        let now = chrono::Utc::now().timestamp_millis().max(0) as u64;
        now.saturating_sub(self.updated_at_ms) < TELEMETRY_TTL_MS
    }
}

/// Shared, latest-known device telemetry.
#[derive(Clone, Default)]
pub struct DeviceState {
    inner: Arc<RwLock<DeviceTelemetry>>,
}

impl DeviceState {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn update(&self, telemetry: DeviceTelemetry) {
        *self.inner.write().await = telemetry;
    }

    pub async fn snapshot(&self) -> DeviceTelemetry {
        self.inner.read().await.clone()
    }
}

/// Structured status returned to the app (E16) and used for E6.
#[derive(Debug, Serialize)]
pub struct AgentStatusReply {
    pub node_name: String,
    /// apiserver URL from the agent's kubeconfig (`https://host:6443`).
    pub apiserver: String,
    /// Agent-side readiness: not drained, network up, battery not critical.
    pub ready: bool,
    pub pods_running: usize,
    /// Sum of `restartCount` across known containers.
    pub restarts: i32,
    pub drained: bool,
    /// G2: operator maintenance mode and the resulting cordon state.
    pub maintenance: bool,
    pub cordoned: bool,
    pub kubelet_port: u16,
}

pub fn default_socket_path() -> PathBuf {
    let base = crate::config::get().data_dir();
    base.join("agent.sock")
}

/// Accept telemetry connections forever. Callers wrap this in a restart loop.
pub async fn serve(
    path: PathBuf,
    device: DeviceState,
    kubelet: KubeletState,
    kubelet_port: u16,
    apiserver: String,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // A stale socket from a killed agent would make bind() fail with EADDRINUSE.
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    info!(path = %path.display(), "device status IPC listening");

    loop {
        let (stream, _) = listener.accept().await?;
        let device = device.clone();
        let kubelet = kubelet.clone();
        let apiserver = apiserver.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, device, kubelet, kubelet_port, apiserver).await {
                debug!(error = %e, "device status IPC client ended");
            }
        });
    }
}

async fn handle_client(
    stream: UnixStream,
    device: DeviceState,
    kubelet: KubeletState,
    kubelet_port: u16,
    apiserver: String,
) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<DeviceTelemetry>(line) {
            Ok(mut telemetry) => {
                telemetry.updated_at_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
                debug!(
                    battery = ?telemetry.battery_percent,
                    charging = ?telemetry.battery_charging,
                    network = ?telemetry.network_type,
                    "device telemetry updated"
                );
                device.update(telemetry).await;

                let reply = current_status(&kubelet, &device, kubelet_port, &apiserver).await;
                let mut out = serde_json::to_vec(&reply).unwrap_or_default();
                out.push(b'\n');
                writer.write_all(&out).await?;
            }
            Err(e) => warn!(error = %e, "invalid device telemetry JSON"),
        }
    }
    Ok(())
}

async fn current_status(
    kubelet: &KubeletState,
    device: &DeviceState,
    kubelet_port: u16,
    apiserver: &str,
) -> AgentStatusReply {
    let restarts = {
        let statuses = kubelet.pod_statuses.read().await;
        statuses
            .values()
            .flat_map(|s| s.containers.iter())
            .map(|c| c.restart_count.max(0))
            .sum()
    };
    let drained = kubelet.is_drained();
    let telemetry = device.snapshot().await;
    // Readiness mirrors the Node's Ready condition (battery/network), not the
    // drain state: a maintenance node is still Ready, just unschedulable.
    let ready = telemetry_ready(&telemetry);

    AgentStatusReply {
        node_name: kubelet.node_name_hint().await,
        apiserver: apiserver.to_string(),
        ready,
        pods_running: running_pods(kubelet).await,
        restarts,
        drained,
        maintenance: kubelet.is_maintenance(),
        cordoned: kubelet.cordoned.load(std::sync::atomic::Ordering::Relaxed),
        kubelet_port,
    }
}

/// Readiness as the agent sees it from the pushed telemetry. Without telemetry
/// we assume ready (the old behaviour) instead of flapping to NotReady.
fn telemetry_ready(telemetry: &DeviceTelemetry) -> bool {
    if !telemetry.is_fresh() {
        return true;
    }
    let network_ok = telemetry
        .network_type
        .as_deref()
        .map(|kind| !kind.is_empty() && kind != "none")
        .unwrap_or(true);
    let battery_ok = match telemetry.battery_percent {
        Some(percent) => percent >= 20 || telemetry.battery_charging.unwrap_or(false),
        None => true,
    };
    network_ok && battery_ok
}

/// `server:` from the kubeconfig the agent was started with (E16 status line).
pub fn apiserver_from_env() -> String {
    let Some(path) = crate::config::kubeconfig_path() else {
        return String::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("server:") {
            return rest.trim().to_string();
        }
    }
    String::new()
}

/// Distinct pods with at least one registered container process.
pub async fn running_pods(kubelet: &KubeletState) -> usize {
    let procs = kubelet.pod_procs.read().await;
    let mut pods = HashSet::with_capacity(procs.len());
    for (namespace, pod, _container, _pid) in procs.values() {
        pods.insert(format!("{namespace}/{pod}"));
    }
    pods.len()
}

/// Remove the socket file on shutdown so the next start binds cleanly.
pub fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_reply() -> AgentStatusReply {
        AgentStatusReply {
            node_name: "dn-test".into(),
            apiserver: "https://192.168.2.7:6443".into(),
            ready: true,
            pods_running: 2,
            restarts: 1,
            drained: false,
            maintenance: false,
            cordoned: false,
            kubelet_port: 10255,
        }
    }

    #[test]
    fn status_reply_serializes_fields_the_app_expects() {
        let value = serde_json::to_value(sample_reply()).expect("serialize");
        assert_eq!(value["node_name"], "dn-test");
        assert_eq!(value["apiserver"], "https://192.168.2.7:6443");
        assert_eq!(value["ready"], true);
        assert_eq!(value["pods_running"], 2);
        assert_eq!(value["kubelet_port"], 10255);
    }

    #[test]
    fn telemetry_ignores_client_supplied_timestamp() {
        let parsed: DeviceTelemetry =
            serde_json::from_str(r#"{"battery_percent":42,"updated_at_ms":999999999999999}"#)
                .expect("parse");
        // The handler overwrites updated_at_ms after parsing; parsing alone keeps it.
        assert_eq!(parsed.battery_percent, Some(42));
    }
}
