use axum::{
    body::Body,
    extract::{Extension, Path, Query, RawQuery, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use futures::StreamExt;
use k8s_openapi::api::certificates::v1::{
    CertificateSigningRequest, CertificateSigningRequestSpec,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use k8s_openapi::ByteString;
use kube::api::{Api, Patch, PatchParams, PostParams};
use kube::Client;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;
use tokio_stream::wrappers::BroadcastStream;
use tracing::{info, instrument};

use crate::error::Result;
use crate::brokers::ControlPlaneBroker;
use crate::models::{NodeProfile, PodRunStatus};
use crate::status_ipc::DeviceState;

// ─── Shared state ─────────────────────────────────────────────────────────────

/// One log line as captured from a container stream.
#[derive(Clone, Debug)]
pub struct LogLine {
    pub at: chrono::DateTime<chrono::Utc>,
    pub text: String,
    /// Set on the sentinel event that terminates a follow stream.
    pub eof: bool,
}

impl LogLine {
    fn new(text: String) -> Self {
        Self {
            at: chrono::Utc::now(),
            text,
            eof: false,
        }
    }

    fn eof() -> Self {
        Self {
            at: chrono::Utc::now(),
            text: String::new(),
            eof: true,
        }
    }

    /// Render the way kubelet does: optional RFC3339Nano prefix.
    pub fn render(&self, timestamps: bool) -> String {
        if timestamps {
            format!(
                "{} {}",
                self.at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
                self.text
            )
        } else {
            self.text.clone()
        }
    }
}

/// Per-container log buffers plus a broadcast channel so `kubectl logs -f`
/// and dashboards (Headlamp, Lens, k9s) can stream instead of getting a snapshot.
///
/// Lines are also appended to `<data>/logs/<ns>/<pod>/<container>.log` so that
/// - the output survives an agent restart (the in-memory buffer does not), and
/// - `kubectl logs --previous` can serve the previous container instance, which
///   is rotated to `.log.1` when a container starts again (B19).
#[derive(Clone, Default)]
pub struct LogStore {
    inner: Arc<RwLock<HashMap<String, PodLog>>>,
    dir: Arc<RwLock<Option<std::path::PathBuf>>>,
}

struct PodLog {
    lines: std::collections::VecDeque<LogLine>,
    tx: tokio::sync::broadcast::Sender<LogLine>,
    file: Option<std::fs::File>,
    /// Bytes written to `file`, used for size-based rotation (B19b).
    size: u64,
}

/// Rotate a container log once it grows past this size.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

impl LogStore {
    fn new_entry() -> PodLog {
        let (tx, _rx) = tokio::sync::broadcast::channel(2048);
        PodLog {
            lines: std::collections::VecDeque::new(),
            tx,
            file: None,
            size: 0,
        }
    }

    /// Move `.log` to `.log.1` (dropping any older `.log.1`).
    fn rotate(dir: &std::path::Path, key: &str) {
        let current = Self::file_path(dir, key, false);
        let previous = Self::file_path(dir, key, true);
        if current.exists() {
            let _ = std::fs::remove_file(&previous);
            let _ = std::fs::rename(&current, &previous);
        }
    }

    pub async fn set_dir(&self, dir: std::path::PathBuf) {
        if std::fs::create_dir_all(&dir).is_ok() {
            *self.dir.write().await = Some(dir);
        }
    }

    fn file_path(dir: &std::path::Path, key: &str, previous: bool) -> std::path::PathBuf {
        let mut path = dir.join(format!("{key}.log"));
        if previous {
            path.set_extension("log.1");
        }
        path
    }

    fn open_append(dir: &std::path::Path, key: &str) -> Option<std::fs::File> {
        let path = Self::file_path(dir, key, false);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
    }

    /// Called when a container instance starts: the previous instance's log is
    /// rotated to `.log.1` so `--previous` keeps working.
    pub async fn begin(&self, key: &str) {
        let dir = self.dir.read().await.clone();
        let Some(dir) = dir else { return };

        let current = Self::file_path(&dir, key, false);
        let previous = Self::file_path(&dir, key, true);
        if current.exists() {
            let _ = std::fs::remove_file(&previous);
            let _ = std::fs::rename(&current, &previous);
        }

        let mut map = self.inner.write().await;
        let entry = map.entry(key.to_string()).or_insert_with(Self::new_entry);
        entry.lines.clear();
        entry.file = Self::open_append(&dir, key);
    }

    pub async fn append(&self, key: &str, line: String) {
        let dir = self.dir.read().await.clone();
        let mut map = self.inner.write().await;
        let entry = map.entry(key.to_string()).or_insert_with(Self::new_entry);
        let line = LogLine::new(line);

        if entry.file.is_none() {
            if let Some(dir) = dir.as_deref() {
                entry.file = Self::open_append(dir, key);
            }
        }
        if let Some(file) = entry.file.as_mut() {
            use std::io::Write;
            let text = format!(
                "{}\t{}\n",
                line.at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
                line.text
            );
            if file.write_all(text.as_bytes()).is_ok() {
                entry.size += text.len() as u64;
            }
            let _ = file.flush();
        }

        // B19b: keep each container log bounded instead of growing forever.
        if entry.size > MAX_LOG_BYTES {
            if let Some(dir) = dir.as_deref() {
                entry.file = None;
                Self::rotate(dir, key);
                entry.file = Self::open_append(dir, key);
                entry.size = 0;
            }
        }

        entry.lines.push_back(line.clone());
        while entry.lines.len() > 10_000 {
            entry.lines.pop_front();
        }
        // Ignores the error when nobody is following.
        let _ = entry.tx.send(line);
    }

    /// Read the previous instance's log from disk (`kubectl logs --previous`).
    pub async fn tail_previous(&self, key: &str, n: usize) -> Vec<LogLine> {
        let dir = self.dir.read().await.clone();
        let Some(dir) = dir else { return Vec::new() };
        let path = Self::file_path(&dir, key, true);
        let Ok(text) = std::fs::read_to_string(path) else {
            return Vec::new();
        };
        let mut lines: Vec<LogLine> = text
            .lines()
            .filter_map(|raw| {
                let (ts, body) = raw.split_once('\t')?;
                let at = chrono::DateTime::parse_from_rfc3339(ts)
                    .ok()?
                    .with_timezone(&chrono::Utc);
                Some(LogLine {
                    at,
                    text: body.to_string(),
                    eof: false,
                })
            })
            .collect();
        if lines.len() > n {
            lines.drain(..lines.len() - n);
        }
        lines
    }

    /// Read the tail of the current log file (used after an agent restart, when
    /// the in-memory buffer is empty but the file still has the output).
    pub async fn tail_file(&self, key: &str, n: usize) -> Vec<LogLine> {
        let dir = self.dir.read().await.clone();
        let Some(dir) = dir else { return Vec::new() };
        let path = Self::file_path(&dir, key, false);
        let Ok(text) = std::fs::read_to_string(path) else {
            return Vec::new();
        };
        let mut lines: Vec<LogLine> = text
            .lines()
            .filter_map(|raw| {
                let (ts, body) = raw.split_once('\t')?;
                let at = chrono::DateTime::parse_from_rfc3339(ts)
                    .ok()?
                    .with_timezone(&chrono::Utc);
                Some(LogLine {
                    at,
                    text: body.to_string(),
                    eof: false,
                })
            })
            .collect();
        if lines.len() > n {
            lines.drain(..lines.len() - n);
        }
        lines
    }

    /// Mark the stream as finished; followers get an EOF event so their HTTP
    /// request completes instead of hanging forever (dashboards show a spinner
    /// otherwise).
    pub async fn finish(&self, key: &str) {
        let mut map = self.inner.write().await;
        if let Some(entry) = map.get_mut(key) {
            let _ = entry.tx.send(LogLine::eof());
        }
    }

    /// Last `n` lines, optionally only those newer than `since`.
    pub async fn tail(
        &self,
        key: &str,
        n: usize,
        since: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Vec<LogLine> {
        let map = self.inner.read().await;
        match map.get(key) {
            None => Vec::new(),
            Some(l) => {
                let start = l.lines.len().saturating_sub(n);
                l.lines
                    .iter()
                    .skip(start)
                    .filter(|line| since.is_none_or(|s| line.at >= s))
                    .cloned()
                    .collect()
            }
        }
    }

    pub async fn subscribe(&self, key: &str) -> tokio::sync::broadcast::Receiver<LogLine> {
        let mut map = self.inner.write().await;
        let entry = map.entry(key.to_string()).or_insert_with(Self::new_entry);
        entry.tx.subscribe()
    }

    pub async fn remove(&self, key: &str) {
        self.inner.write().await.remove(key);
    }

    /// True when at least one line has been captured (dashboards use this to
    /// distinguish "no output yet" from "container gone").
    pub async fn exists(&self, key: &str) -> bool {
        self.inner.read().await.contains_key(key)
    }
}

/// A10: DER of the verified client certificate (None when the peer did not
/// present one). Inserted into request extensions by the TLS connection task.
#[derive(Clone)]
pub struct PeerCert(pub Option<Vec<u8>>);

/// F1: remote address of the TLS peer, used to allow the local Android UI
/// (loopback) without a client certificate.
#[derive(Clone)]
pub struct PeerAddr(pub Option<SocketAddr>);

/// A1: everything `kubectl exec` needs to start a helper process in the same
/// rootfs as a running container.
#[derive(Clone)]
pub struct ExecTarget {
    pub rootfs: std::path::PathBuf,
    pub fake_root: bool,
}

/// A2: live container IO used by `kubectl attach`.
#[derive(Clone, Debug)]
pub struct AttachFrame {
    pub channel: u8,
    pub data: Vec<u8>,
}

#[derive(Clone)]
pub struct AttachTarget {
    pub stdin: tokio::sync::mpsc::Sender<Vec<u8>>,
    pub output: tokio::sync::broadcast::Sender<AttachFrame>,
}

#[derive(Clone)]
pub struct KubeletState {
    pub node_profile: Arc<RwLock<Option<NodeProfile>>>,
    /// "namespace/pod/container" → buffered log lines
    pub logs: LogStore,
    pub pod_statuses: Arc<RwLock<HashMap<String, PodRunStatus>>>,
    /// Where unpacked rootfs trees live (for the image list / delete API).
    pub rootfs_base: Arc<std::path::PathBuf>,
    /// Where compressed layers live (layer GC on delete).
    pub layers_base: Arc<std::path::PathBuf>,
    /// Rootfs directories currently used by a running container - deleting one
    /// of these would break the pod, so the API refuses.
    pub in_use: Arc<RwLock<std::collections::HashSet<String>>>,
    /// Where per-container log files are written.
    pub logs_dir: Arc<std::path::PathBuf>,
    /// pod_uid -> (namespace, pod, container, process-group id). The pgid is what
    /// `/metrics/resource` uses to sum per-container CPU and RSS.
    pub pod_procs: Arc<RwLock<HashMap<String, (String, String, String, i32)>>>,
    /// Last (time, cumulative CPU seconds) sample, used to derive a CPU rate for
    /// the summary API.
    pub summary_cpu: Arc<RwLock<Option<(chrono::DateTime<chrono::Utc>, f64)>>>,
    /// Node name to report in the summary API.
    pub node_name: Arc<RwLock<String>>,
    /// E7: when set the agent stops its pods gracefully instead of the Android
    /// side killing the whole process on low battery.
    pub drained: Arc<std::sync::atomic::AtomicBool>,
    /// E1: latest device telemetry pushed by the Android app.
    pub device: Arc<std::sync::RwLock<Option<DeviceState>>>,
    /// G2: operator maintenance mode (`/maintenance`). Kept separate from the
    /// battery-driven `drained` flag so undrain-on-recovery cannot clear it.
    pub maintenance: Arc<std::sync::atomic::AtomicBool>,
    /// Whether the Node is currently cordoned by us.
    pub cordoned: Arc<std::sync::atomic::AtomicBool>,
    /// Control-plane handle used to patch `spec.unschedulable`.
    pub control_plane: Arc<std::sync::RwLock<Option<Arc<dyn ControlPlaneBroker>>>>,
    /// A1: "namespace/pod/container" → rootfs of the running container.
    pub exec_targets: Arc<RwLock<HashMap<String, ExecTarget>>>,
    /// A1: proot launcher, injected by main after the brokers are built.
    pub proot: Arc<RwLock<Option<Arc<dyn crate::brokers::ProotBroker>>>>,
    /// A2: "namespace/pod/container" → live stdin/output.
    pub attach_targets: Arc<RwLock<HashMap<String, AttachTarget>>>,
}

impl Default for KubeletState {
    fn default() -> Self {
        Self::new()
    }
}

impl KubeletState {
    pub fn new() -> Self {
        Self {
            node_profile: Arc::new(RwLock::new(None)),
            logs: LogStore::default(),
            pod_statuses: Arc::new(RwLock::new(HashMap::new())),
            rootfs_base: Arc::new(std::path::PathBuf::from("/tmp/droidnode/rootfs")),
            layers_base: Arc::new(std::path::PathBuf::from("/tmp/droidnode/layers")),
            in_use: Arc::new(RwLock::new(std::collections::HashSet::new())),
            logs_dir: Arc::new(std::path::PathBuf::from("/tmp/droidnode/logs")),
            pod_procs: Arc::new(RwLock::new(HashMap::new())),
            summary_cpu: Arc::new(RwLock::new(None)),
            node_name: Arc::new(RwLock::new(String::new())),
            drained: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            device: Arc::new(std::sync::RwLock::new(None)),
            maintenance: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            cordoned: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            control_plane: Arc::new(std::sync::RwLock::new(None)),
            exec_targets: Arc::new(RwLock::new(HashMap::new())),
            proot: Arc::new(RwLock::new(None)),
            attach_targets: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// A2: register the running container's live IO.
    pub async fn register_attach_target(
        &self,
        namespace: &str,
        pod: &str,
        container: &str,
        stdin: tokio::sync::mpsc::Sender<Vec<u8>>,
        output: tokio::sync::broadcast::Sender<AttachFrame>,
    ) {
        self.attach_targets.write().await.insert(
            format!("{namespace}/{pod}/{container}"),
            AttachTarget { stdin, output },
        );
    }

    pub async fn remove_attach_targets(&self, namespace: &str, pod: &str) {
        let prefix = format!("{namespace}/{pod}/");
        self.attach_targets
            .write()
            .await
            .retain(|key, _| !key.starts_with(&prefix));
    }

    pub async fn attach_target(
        &self,
        namespace: &str,
        pod: &str,
        container: &str,
    ) -> Option<AttachTarget> {
        self.attach_targets
            .read()
            .await
            .get(&format!("{namespace}/{pod}/{container}"))
            .cloned()
    }

    /// A1: main injects the same proot broker used to start containers.
    pub async fn attach_proot(&self, broker: Arc<dyn crate::brokers::ProotBroker>) {
        *self.proot.write().await = Some(broker);
    }

    pub async fn proot(&self) -> Option<Arc<dyn crate::brokers::ProotBroker>> {
        self.proot.read().await.clone()
    }

    /// A1: remember where a container's rootfs lives so `kubectl exec` can use it.
    pub async fn register_exec_target(
        &self,
        namespace: &str,
        pod: &str,
        container: &str,
        rootfs: std::path::PathBuf,
    ) {
        self.exec_targets.write().await.insert(
            format!("{namespace}/{pod}/{container}"),
            ExecTarget {
                rootfs,
                fake_root: true,
            },
        );
    }

    pub async fn remove_exec_targets(&self, namespace: &str, pod: &str) {
        let prefix = format!("{namespace}/{pod}/");
        self.exec_targets
            .write()
            .await
            .retain(|key, _| !key.starts_with(&prefix));
    }

    pub async fn exec_target(
        &self,
        namespace: &str,
        pod: &str,
        container: &str,
    ) -> Option<ExecTarget> {
        self.exec_targets
            .read()
            .await
            .get(&format!("{namespace}/{pod}/{container}"))
            .cloned()
    }

    /// Attach the control-plane broker so `/maintenance` can patch the Node.
    pub fn attach_control_plane(&self, broker: Arc<dyn ControlPlaneBroker>) {
        *self
            .control_plane
            .write()
            .expect("control plane lock poisoned") = Some(broker);
    }

    pub fn control_plane(&self) -> Option<Arc<dyn ControlPlaneBroker>> {
        self.control_plane
            .read()
            .expect("control plane lock poisoned")
            .clone()
    }

    pub fn is_maintenance(&self) -> bool {
        self.maintenance
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Attach the shared telemetry store (E1). Called once at startup.
    pub fn attach_device(&self, device: DeviceState) {
        *self.device.write().expect("device state lock poisoned") = Some(device);
    }

    pub fn device_state(&self) -> Option<DeviceState> {
        self.device
            .read()
            .expect("device state lock poisoned")
            .clone()
    }

    /// E7: true when the node asked to be drained (low battery etc.).
    pub fn is_drained(&self) -> bool {
        self.drained.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn drain_flag(&self) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::clone(&self.drained)
    }

    /// Tell the kubelet API which node it belongs to (used by the summary API).
    pub async fn set_node_name(&self, name: String) {
        *self.node_name.write().await = name;
    }

    /// Node name advertised in the summary API: the configured one, else the
    /// profile registered with the control plane.
    pub async fn node_name_hint(&self) -> String {
        let configured = self.node_name.read().await.clone();
        if !configured.is_empty() {
            return configured;
        }
        self.node_profile
            .read()
            .await
            .as_ref()
            .map(|p| p.node_id.clone())
            .unwrap_or_default()
    }

    /// Remember the process-group of a running container for metrics.
    pub async fn register_pod_proc(
        &self,
        uid: &str,
        namespace: &str,
        pod: &str,
        container: &str,
        pgid: i32,
    ) {
        self.pod_procs.write().await.insert(
            uid.to_string(),
            (
                namespace.to_string(),
                pod.to_string(),
                container.to_string(),
                pgid,
            ),
        );
    }

    pub async fn unregister_pod_proc(&self, uid: &str) {
        self.pod_procs.write().await.remove(uid);
    }

    pub async fn mark_in_use(&self, rootfs: &std::path::Path) {
        self.in_use
            .write()
            .await
            .insert(rootfs.to_string_lossy().to_string());
    }

    pub async fn unmark_in_use(&self, rootfs: &std::path::Path) {
        self.in_use
            .write()
            .await
            .remove(&rootfs.to_string_lossy().to_string());
    }

    pub async fn is_in_use(&self, rootfs: &str) -> bool {
        self.in_use.read().await.contains(rootfs)
    }

    pub fn with_paths(rootfs: std::path::PathBuf, layers: std::path::PathBuf) -> Self {
        let mut s = Self::new();
        // Logs live next to the image store: <files>/rootfs + <files>/logs
        let logs = rootfs
            .parent()
            .map(|p| p.join("logs"))
            .unwrap_or_else(|| rootfs.join("logs"));
        s.logs_dir = Arc::new(logs);
        s.rootfs_base = Arc::new(rootfs);
        s.layers_base = Arc::new(layers);
        s
    }

    /// Start (or restart) a container's log: rotate the old file to `.log.1`
    /// and open a fresh one for this instance.
    pub async fn begin_log(&self, namespace: &str, pod: &str, container: &str) {
        self.logs
            .begin(&format!("{}/{}/{}", namespace, pod, container))
            .await;
    }

    /// Directory for on-disk logs (set up by `with_paths`).
    pub fn ensure_log_dir(&self) -> std::path::PathBuf {
        (*self.logs_dir).clone()
    }

    pub async fn append_log(&self, namespace: &str, pod: &str, container: &str, line: String) {
        self.logs
            .append(&format!("{}/{}/{}", namespace, pod, container), line)
            .await;
    }

    /// Signal "no more output" for a container, so any open `-f` stream closes.
    pub async fn finish_log(&self, namespace: &str, pod: &str, container: &str) {
        self.logs
            .finish(&format!("{}/{}/{}", namespace, pod, container))
            .await;
    }

    pub async fn update_status(&self, status: PodRunStatus) {
        let mut statuses = self.pod_statuses.write().await;
        statuses.insert(status.pod_uid.clone(), status);
    }

    /// Forget a pod once its containers are gone (deleted or finalized).
    pub async fn remove_status(&self, pod_uid: &str) {
        self.pod_statuses.write().await.remove(pod_uid);
    }
}

// ─── Exposer ──────────────────────────────────────────────────────────────────

pub struct VirtualKubeletExposer {
    state: KubeletState,
    bind_addr: SocketAddr,
}

impl VirtualKubeletExposer {
    pub fn new(bind_addr: SocketAddr) -> Self {
        Self {
            state: KubeletState::new(),
            bind_addr,
        }
    }

    /// Constructor used by the agent: gives the local API access to the image
    /// store so the on-device page can list and delete images.
    pub fn with_paths(
        bind_addr: SocketAddr,
        rootfs_base: std::path::PathBuf,
        layers_base: std::path::PathBuf,
    ) -> Self {
        Self {
            state: KubeletState::with_paths(rootfs_base, layers_base),
            bind_addr,
        }
    }

    pub fn state(&self) -> KubeletState {
        self.state.clone()
    }

    #[instrument(skip(self), fields(addr = %self.bind_addr))]
    pub async fn serve(&self) -> Result<()> {
        use hyper::service::service_fn;
        use hyper_util::rt::{TokioExecutor, TokioIo};
        use hyper_util::server::conn::auto::Builder as ConnBuilder;
        use std::sync::Arc;
        use tokio_rustls::TlsAcceptor;

        let app = build_router(self.state.clone());

        // Enable on-disk logs before serving so restarts keep the history.
        self.state
            .logs
            .set_dir(self.state.ensure_log_dir())
            .await;

        // Every local IPv4 goes into the cert's SAN: the cluster may reach us
        // over Wi-Fi while the UDP default-route trick reports a cellular IP.
        let local_ip = crate::net_util::primary_ipv4();
        let mut sans = vec!["droidnode".to_string()];
        for ip in crate::net_util::local_ipv4_addresses() {
            if !sans.contains(&ip) {
                sans.push(ip);
            }
        }
        if !sans.contains(&local_ip) {
            sans.push(local_ip.clone());
        }

        let preissued = try_csr_serving_cert(&sans, &self.state.node_name_hint().await).await;
        let tls_cfg = make_tls_config(&sans, preissued)?;
        let acceptor = TlsAcceptor::from(Arc::new(tls_cfg));

        // A failed bind must not take the whole agent down (that used to exit
        // the process and left the node flapping). Retry with backoff instead.
        let listener = loop {
            match tokio::net::TcpListener::bind(self.bind_addr).await {
                Ok(l) => break l,
                Err(e) => {
                    tracing::error!(
                        addr = %self.bind_addr,
                        error = %e,
                        "kubelet bind failed - is another agent instance running? retrying in 5s"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        };

        info!(addr = %self.bind_addr, "kubelet HTTPS server listening");

        loop {
            let (tcp, _) = match listener.accept().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("accept error: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    continue;
                }
            };
            let acceptor = acceptor.clone();
            let app = app.clone();
            let peer = tcp.peer_addr().ok();

            tokio::spawn(async move {
                // Log the handshake outcome: without this a rejection (wrong CA,
                // no shared TLS version, missing SNI) looks exactly like the
                // peer never connecting, and dashboards just show 502.
                let tls = match acceptor.accept(tcp).await {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!(?peer, error = %e, "kubelet TLS handshake failed");
                        return;
                    }
                };
                let peer_cert = tls
                    .get_ref()
                    .1
                    .peer_certificates()
                    .and_then(|certs| certs.first())
                    .map(|c| c.as_ref().to_vec());
                info!(
                    ?peer,
                    client_cert = peer_cert.is_some(),
                    "kubelet TLS connection accepted"
                );
                let io = TokioIo::new(tls);
                let svc = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                    let mut app = app.clone();
                    let peer_cert = peer_cert.clone();
                    let peer_addr = peer;
                    async move {
                        use tower::Service;
                        let mut req = req.map(axum::body::Body::new);
                        // F1: the local Android UI talks over loopback and has no
                        // client cert. Everything else must be the apiserver (or
                        // another client-CA-signed component). Metrics and
                        // healthz stay public for metrics-server / probes.
                        let path = req.uri().path().to_string();
                        let public = path == "/healthz"
                            || path == "/metrics"
                            || path == "/metrics/resource";
                        let remote = peer_addr
                            .map(|addr| !addr.ip().is_loopback())
                            .unwrap_or(true);
                        if remote && peer_cert.is_none() && !public {
                            return Ok::<_, std::convert::Infallible>(
                                Response::builder()
                                    .status(StatusCode::FORBIDDEN)
                                    .header("content-type", "text/plain")
                                    .body(Body::from(
                                        "client certificate required by DroidNode kubelet",
                                    ))
                                    .unwrap(),
                            );
                        }
                        req.extensions_mut().insert(PeerCert(peer_cert));
                        req.extensions_mut().insert(PeerAddr(peer_addr));
                        app.call(req).await
                    }
                });
                ConnBuilder::new(TokioExecutor::new())
                    .serve_connection_with_upgrades(io, svc)
                    .await
                    .ok();
            });
        }
    }
}

// ─── TLS config ───────────────────────────────────────────────────────────────

/// Build a rustls ServerConfig for the kubelet HTTPS endpoint.
///
/// Generates a persistent CA on first run and saves it to
/// `$DROIDNODE_DATA_DIR/kubelet-ca.{crt,key}` (default: ~/.droidnode/).
/// The kubelet serving cert is signed by that CA on every startup.
///
/// On first run the agent prints the CA cert path; add it to k3s once:
///   kube-apiserver-arg:
///     - "kubelet-certificate-authority=<path>"
/// After that one restart k3s trusts every cert we sign.
/// A16: obtain a kubelet serving cert signed by the cluster CA through the
/// `kubernetes.io/kubelet-serving` CSR signer. Returns None when the API/RBAC
/// or signer is unavailable, in which case the caller falls back to the
/// device-local CA.
async fn try_csr_serving_cert(
    sans: &[String],
    node: &str,
) -> Option<(
    Vec<tokio_rustls::rustls::pki_types::CertificateDer<'static>>,
    tokio_rustls::rustls::pki_types::PrivateKeyDer<'static>,
)> {
    use rcgen::{CertificateParams, DnType, KeyPair};

    if node.is_empty() {
        return None;
    }
    let key = KeyPair::generate().ok()?;
    let mut params = CertificateParams::new(sans.to_vec()).ok()?;
    params
        .distinguished_name
        .push(DnType::CommonName, format!("system:node:{node}"));
    params
        .distinguished_name
        .push(DnType::OrganizationName, "system:nodes");
    params.is_ca = rcgen::IsCa::NoCa;
    let csr = params.serialize_request(&key).ok()?;

    let client = Client::try_default().await.ok()?;
    let api: Api<CertificateSigningRequest> = Api::all(client);
    let name = format!("droidnode-kubelet-{node}");
    if api.get(&name).await.is_ok() {
        let _ = api.delete(&name, &Default::default()).await;
    }
    let request = CertificateSigningRequest {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            ..Default::default()
        },
        spec: CertificateSigningRequestSpec {
            request: ByteString(csr.pem().ok()?.into_bytes()),
            signer_name: "kubernetes.io/kubelet-serving".into(),
            usages: Some(vec![
                "server auth".into(),
                "digital signature".into(),
                "key encipherment".into(),
            ]),
            username: Some(format!("system:node:{node}")),
            groups: Some(vec!["system:nodes".into()]),
            ..Default::default()
        },
        ..Default::default()
    };
    if let Err(e) = api.create(&PostParams::default(), &request).await {
        tracing::warn!(error = %e, "A16: could not create serving CSR");
        return None;
    }
    let approval = serde_json::json!({
        "status": { "conditions": [{
            "type": "Approved",
            "status": "True",
            "reason": "DroidNodeBootstrap",
            "message": "auto-approved by node agent"
        }]}
    });
    if let Err(e) = api
        .patch_approval(
            &name,
            &PatchParams::default(),
            &Patch::Merge(&approval),
        )
        .await
    {
        tracing::warn!(error = %e, "A16: could not approve serving CSR");
        return None;
    }
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let got = match api.get(&name).await {
            Ok(c) => c,
            Err(_) => continue,
        };
        let Some(cert) = got.status.and_then(|s| s.certificate) else {
            continue;
        };
        let pem = String::from_utf8_lossy(&cert.0).to_string();
        let certs = parse_pem_certs(&pem);
        if !certs.is_empty() {
            tracing::info!(csr = %name, certs = certs.len(), "A16: serving cert issued by cluster CA");
            return Some((
                certs,
                tokio_rustls::rustls::pki_types::PrivateKeyDer::Pkcs8(
                    key.serialize_der().into(),
                ),
            ));
        }
    }
    tracing::warn!(csr = %name, "A16: CSR was not signed in time");
    None
}

fn parse_pem_certs(pem: &str) -> Vec<tokio_rustls::rustls::pki_types::CertificateDer<'static>> {
    use base64::Engine;
    use tokio_rustls::rustls::pki_types::CertificateDer;

    let mut out = Vec::new();
    let mut rest = pem;
    while let Some(begin) = rest.find("-----BEGIN CERTIFICATE-----") {
        let after = &rest[begin + "-----BEGIN CERTIFICATE-----".len()..];
        let Some(end) = after.find("-----END CERTIFICATE-----") else {
            break;
        };
        let body: String = after[..end].lines().map(str::trim).collect();
        rest = &after[end + "-----END CERTIFICATE-----".len()..];
        if let Ok(der) = base64::engine::general_purpose::STANDARD.decode(body) {
            out.push(CertificateDer::from(der));
        }
    }
    out
}

fn make_tls_config(
    sans: &[String],
    preissued: Option<(
        Vec<tokio_rustls::rustls::pki_types::CertificateDer<'static>>,
        tokio_rustls::rustls::pki_types::PrivateKeyDer<'static>,
    )>,
) -> Result<tokio_rustls::rustls::ServerConfig> {
    use base64::Engine;
    use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
    use tokio_rustls::rustls;

    // Resolve data dir (same logic as Config::from_env in main.rs)
    let data_dir = std::env::var("DROIDNODE_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
                .join(".droidnode")
        });
    let _ = std::fs::create_dir_all(&data_dir);

    let ca_cert_path = data_dir.join("kubelet-ca.crt");
    let ca_key_path = data_dir.join("kubelet-ca.key");

    // CA params are identical every run — same Subject DN → same Issuer field
    // in every cert we sign → k3s can match against the persisted CA cert.
    //
    // EVERY field must be deterministic: rcgen assigns a RANDOM serial and
    // now±1h validity by default, so re-deriving the CA on each start used to
    // produce a *different* certificate than the one on disk. kube-apiserver
    // then verified our serving cert against a CA it had never signed with and
    // every `kubectl logs` request died with 502 Bad Gateway.
    let mut ca_params = CertificateParams::default();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    // Go (kube-apiserver) rejects a CA whose KeyUsage omits keyCertSign as soon
    // as the extension is present; emitting the standard set is what a real
    // kubelet CA looks like. OpenSSL is far more forgiving, so this only shows
    // up as `BadCertificate` from the apiserver.
    ca_params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
        rcgen::KeyUsagePurpose::DigitalSignature,
    ];
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "droidnode-kubelet-ca");
    ca_params.serial_number = Some(rcgen::SerialNumber::from(vec![0x01]));
    ca_params.not_before = rcgen::date_time_ymd(2020, 1, 1);
    ca_params.not_after = rcgen::date_time_ymd(2120, 1, 1);

    // Load or generate the CA key (persisted across restarts so the public key
    // stored in kubelet-ca.crt never changes after the first run).
    let ca_key = if ca_key_path.exists() {
        let pem = std::fs::read_to_string(&ca_key_path)
            .map_err(|e| crate::error::DroidError::Config(format!("CA key read: {e}")))?;
        KeyPair::from_pem(&pem)
            .map_err(|e| crate::error::DroidError::Config(format!("CA key parse: {e}")))?
    } else {
        KeyPair::generate()
            .map_err(|e| crate::error::DroidError::Config(format!("CA key gen: {e}")))?
    };

    // Derive (or re-derive) the CA Certificate object from the stable params+key.
    // rcgen 0.13: self_signed / signed_by return Certificate (not CertifiedKey).
    let ca_cert = ca_params
        .self_signed(&ca_key)
        .map_err(|e| crate::error::DroidError::Config(format!("CA self-sign: {e}")))?;

    // Persist the CA material. Compare the *public key* rather than the PEM:
    // rcgen signs the CA with ECDSA, whose signatures are randomized, so the DER
    // bytes differ on every start even though the trust anchor (key + subject)
    // is identical. The old byte comparison printed "CA changed, refresh the k3s
    // bundle" on every restart, which is both noisy and wrong.
    if !ca_key_path.exists() {
        std::fs::write(&ca_key_path, ca_key.serialize_pem())
            .map_err(|e| crate::error::DroidError::Config(format!("CA key write: {e}")))?;
    }
    let ca_pem = format!(
        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
        base64::engine::general_purpose::STANDARD.encode(ca_cert.der())
    );
    let pub_path = data_dir.join("kubelet-ca.pub");
    let pub_pem = ca_key.public_key_pem();
    let on_disk_pub = std::fs::read_to_string(&pub_path).unwrap_or_default();
    let pub_changed = on_disk_pub.trim() != pub_pem.trim();
    if pub_changed || !ca_cert_path.exists() {
        std::fs::write(&ca_cert_path, &ca_pem)
            .map_err(|e| crate::error::DroidError::Config(format!("CA cert write: {e}")))?;
        std::fs::write(&pub_path, &pub_pem)
            .map_err(|e| crate::error::DroidError::Config(format!("CA pub write: {e}")))?;
        tracing::warn!(
            "kubelet CA changed (new key) at {path} - refresh the k3s bundle:\n  \
            cat /var/lib/rancher/k3s/server/tls/server-ca.crt <this file> \
            > /etc/rancher/k3s/kubelet-ca-bundle.crt\n  \
            then: sudo systemctl restart k3s   (and re-copy it off the device)",
            path = ca_cert_path.display()
        );
    } else {
        tracing::debug!("kubelet CA unchanged (same public key)");
    }

    // A16: prefer a serving cert signed by the cluster CA via a CSR; fall
    // back to the device-local CA only when CSR bootstrap is unavailable.
    let (cert_chain, key_der) = if let Some((certs, key)) = preissued {
        tracing::info!("kubelet serving cert issued by cluster CA (CSR)");
        (certs, key)
    } else {
        // Sign the kubelet serving cert with our CA.
        // The IP must be a SAN — k3s connects to the InternalIP we advertise.
        // A distinct CN keeps the leaf from looking self-signed to verifiers.
        let mut server_params = CertificateParams::new(sans.to_vec())
            .map_err(|e| crate::error::DroidError::Config(format!("cert params: {e}")))?;
        server_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "droidnode-kubelet");
        // Standard server-certificate extensions. Without these the cert is a bare
        // public key + SAN, which OpenSSL accepts but Go (and therefore
        // kube-apiserver) refuses during the handshake.
        server_params.is_ca = rcgen::IsCa::NoCa;
        server_params.key_usages = vec![
            rcgen::KeyUsagePurpose::DigitalSignature,
            rcgen::KeyUsagePurpose::KeyEncipherment,
        ];
        server_params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        let server_key = KeyPair::generate()
            .map_err(|e| crate::error::DroidError::Config(format!("server key gen: {e}")))?;
        let signed = server_params
            .signed_by(&server_key, &ca_cert, &ca_key)
            .map_err(|e| crate::error::DroidError::Config(format!("sign: {e}")))?;
        let cert_der = rustls::pki_types::CertificateDer::from(signed.der().to_vec());
        let key_der =
            rustls::pki_types::PrivateKeyDer::Pkcs8(server_key.serialize_der().into());
        (vec![cert_der], key_der)
    };

    tracing::info!(sans = ?sans, ca = %ca_cert_path.display(), "kubelet TLS ready");

    // A10: optionally require a client certificate signed by the cluster's
    // client CA (k3s: /var/lib/rancher/k3s/server/tls/client-ca.crt). It is
    // copied to <dataDir>/client-ca.crt (or $DROIDNODE_KUBELET_CLIENT_CA).
    // `allow_unauthenticated` keeps the local Android UI (no client cert)
    // working; the exec/attach/port-forward handlers enforce the cert.
    let client_ca = load_client_ca(&data_dir);
    let builder = rustls::ServerConfig::builder();
    let builder = if let Some(roots) = client_ca {
        let verifier = rustls::server::WebPkiClientVerifier::builder(roots)
            .allow_unauthenticated()
            .build()
            .map_err(|e| crate::error::DroidError::Config(format!("client CA: {e}")))?;
        tracing::info!("kubelet client-certificate verification enabled");
        builder.with_client_cert_verifier(verifier)
    } else {
        tracing::warn!(
            "no client CA configured - exec/attach/port-forward will refuse remote clients; \
             copy the cluster client-ca.crt to <dataDir>/client-ca.crt"
        );
        builder.with_no_client_auth()
    };
    builder
        .with_single_cert(cert_chain, key_der)
        .map_err(|e| crate::error::DroidError::Config(format!("tls: {e}")))
}

/// A10: load an optional client CA bundle for kubelet mTLS.
fn load_client_ca(
    data_dir: &std::path::Path,
) -> Option<std::sync::Arc<tokio_rustls::rustls::RootCertStore>> {
    use base64::Engine;
    use tokio_rustls::rustls::pki_types::CertificateDer;
    use tokio_rustls::rustls::RootCertStore;

    let path = std::env::var("DROIDNODE_KUBELET_CLIENT_CA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| data_dir.join("client-ca.crt"));
    let pem = std::fs::read_to_string(&path).ok()?;
    let mut roots = RootCertStore::empty();
    let mut rest = pem.as_str();
    while let Some(begin) = rest.find("-----BEGIN CERTIFICATE-----") {
        let after = &rest[begin + "-----BEGIN CERTIFICATE-----".len()..];
        let Some(end) = after.find("-----END CERTIFICATE-----") else {
            break;
        };
        let body: String = after[..end].lines().map(str::trim).collect();
        rest = &after[end + "-----END CERTIFICATE-----".len()..];
        let Ok(der) = base64::engine::general_purpose::STANDARD.decode(body) else {
            continue;
        };
        if roots.add(CertificateDer::from(der)).is_err() {
            continue;
        }
    }
    if roots.is_empty() {
        tracing::warn!(path = %path.display(), "client CA file has no usable certificates");
        return None;
    }
    tracing::info!(path = %path.display(), certs = roots.len(), "loaded kubelet client CA");
    Some(std::sync::Arc::new(roots))
}

// ─── Router ───────────────────────────────────────────────────────────────────

fn build_router(state: KubeletState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/pods", get(list_pods))
        .route("/images", get(list_images))
        .route("/images/delete", axum::routing::post(delete_image))
        .route(
            "/containerLogs/:namespace/:pod_id/:container_name",
            get(container_logs),
        )
        .route("/metrics", get(metrics))
        .route("/metrics/resource", get(metrics_resource))
        .route("/stats/summary", get(stats_summary))
        .route("/drain", axum::routing::post(node_drain))
        .route("/undrain", axum::routing::post(node_undrain))
        .route("/maintenance", axum::routing::post(node_maintenance))
        .route(
            "/exec/:namespace/:pod/:container",
            get(exec_handler).post(exec_handler),
        )
        .route(
            "/attach/:namespace/:pod/:container",
            get(attach_handler).post(attach_handler),
        )
        .route(
            "/portForward/:namespace/:pod",
            get(port_forward_handler).post(port_forward_handler),
        )
        .with_state(state)
}

/// A1: percent-decode one query component (`kubectl` sends `%20` for spaces).
fn decode_query_component(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = |b: u8| -> Option<u8> {
                    match b {
                        b'0'..=b'9' => Some(b - b'0'),
                        b'a'..=b'f' => Some(b - b'a' + 10),
                        b'A'..=b'F' => Some(b - b'A' + 10),
                        _ => None,
                    }
                };
                match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    (Some(hi), Some(lo)) => {
                        out.push(hi * 16 + lo);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A1: parse the RemoteCommand query (`command` may repeat, one arg each).
fn parse_exec_query(raw: Option<String>) -> (Vec<String>, bool, bool, bool, bool) {
    let mut command = Vec::new();
    let mut stdin = false;
    let mut stdout = false;
    let mut stderr = false;
    let mut tty = false;
    for pair in raw.unwrap_or_default().split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = decode_query_component(value);
        match key {
            "command" => command.push(value),
            "stdin" => stdin = value == "1" || value == "true",
            "stdout" => stdout = value == "1" || value == "true",
            "stderr" => stderr = value == "1" || value == "true",
            "tty" => tty = value == "1" || value == "true",
            _ => {}
        }
    }
    (command, stdin, stdout, stderr, tty)
}

/// A1: `kubectl exec` transport.
///
/// The apiserver uses `Upgrade: SPDY/3.1` (POST) for remote command. Rust owns
/// TLS and the 101 response, then hands the raw upgraded byte stream to the Go
/// sidecar, which speaks SPDY/3.1 (moby/spdystream) and bridges the streams to
/// a proot process.
async fn exec_handler(
    State(state): State<KubeletState>,
    Extension(peer): Extension<PeerCert>,
    Path((namespace, pod, container)): Path<(String, String, String)>,
    RawQuery(raw): RawQuery,
    mut req: axum::extract::Request,
) -> Response {
    // A10: only the apiserver (verified client cert) may exec. The local UI
    // uses the maintenance endpoints and never presents a client cert.
    if peer.0.is_none() {
        tracing::warn!(
            namespace = %namespace,
            pod = %pod,
            container = %container,
            "exec rejected: no client certificate"
        );
        return (
            StatusCode::FORBIDDEN,
            "exec requires a client certificate signed by the cluster client CA",
        )
            .into_response();
    }

    let Some(target) = state.exec_target(&namespace, &pod, &container).await else {
        tracing::warn!(
            namespace = %namespace,
            pod = %pod,
            container = %container,
            "exec rejected: container not registered"
        );
        return (StatusCode::NOT_FOUND, "container not running").into_response();
    };

    let (command, stdin, stdout, stderr, tty) = parse_exec_query(raw);
    if command.is_empty() {
        return (StatusCode::BAD_REQUEST, "command is required").into_response();
    }

    let upgrade = req
        .headers()
        .get(axum::http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !upgrade.eq_ignore_ascii_case("SPDY/3.1") {
        return (
            StatusCode::BAD_REQUEST,
            "exec requires Upgrade: SPDY/3.1 (WebSocket-to-kubelet is not enabled)",
        )
            .into_response();
    }

    let protocol = req
        .headers()
        .get("x-stream-protocol-version")
        .and_then(|v| v.to_str().ok())
        .filter(|p| {
            *p == "v4.channel.k8s.io" || *p == "v3.channel.k8s.io" || *p == "v2.channel.k8s.io"
        })
        .unwrap_or("v4.channel.k8s.io")
        .to_string();

    let proot_path = match std::env::var("DROIDNODE_PROOT_PATH") {
        Ok(p) => p,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "DROIDNODE_PROOT_PATH is not set",
            )
                .into_response()
        }
    };
    let helper = std::env::var("DROIDNODE_SPDY_HELPER").unwrap_or_else(|_| {
        std::path::Path::new(&proot_path)
            .with_file_name("libdroidnode_spdy.so")
            .to_string_lossy()
            .to_string()
    });
    if !std::path::Path::new(&helper).exists() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("SPDY sidecar not found at {helper}"),
        )
            .into_response();
    }

    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/data/local/tmp".to_string());
    let socket_path = std::path::Path::new(&tmpdir).join(format!(
        "droidnode-spdy-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_file(&socket_path);
    let listener = match tokio::net::UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("spdy socket: {e}"),
            )
                .into_response()
        }
    };

    let mut child = match tokio::process::Command::new(&helper)
        .arg(&socket_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&socket_path);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("spdy sidecar spawn: {e}"),
            )
                .into_response();
        }
    };

    let (mut stream, _) = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        listener.accept(),
    )
    .await
    {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => {
            let _ = child.kill().await;
            let _ = std::fs::remove_file(&socket_path);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("spdy socket accept: {e}"),
            )
                .into_response();
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = std::fs::remove_file(&socket_path);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "spdy sidecar did not connect",
            )
                .into_response();
        }
    };

    let loader = std::path::Path::new(&proot_path)
        .with_file_name("libproot_loader.so")
        .to_string_lossy()
        .to_string();
    let metadata = serde_json::json!({
        "rootfs": target.rootfs.to_string_lossy(),
        "proot": proot_path,
        "loader": loader,
        "tmpdir": tmpdir,
        "command": command,
        "tty": tty,
        "env": {},
    });
    let mut line = match serde_json::to_vec(&metadata) {
        Ok(v) => v,
        Err(e) => {
            let _ = child.kill().await;
            let _ = std::fs::remove_file(&socket_path);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("spdy metadata: {e}"),
            )
                .into_response();
        }
    };
    line.push(b'\n');
    if let Err(e) = stream.write_all(&line).await {
        let _ = child.kill().await;
        let _ = std::fs::remove_file(&socket_path);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("spdy metadata write: {e}"),
        )
            .into_response();
    }

    let on_upgrade = hyper::upgrade::on(&mut req);
    info!(
        namespace = %namespace,
        pod = %pod,
        container = %container,
        tty,
        stdin,
        stdout,
        stderr,
        protocol = %protocol,
        "exec SPDY handoff"
    );

    tokio::spawn(async move {
        match on_upgrade.await {
            Ok(upgraded) => {
                let mut upgraded = hyper_util::rt::TokioIo::new(upgraded);
                let _ = tokio::io::copy_bidirectional(&mut upgraded, &mut stream).await;
            }
            Err(e) => tracing::warn!(error = %e, "exec upgrade failed"),
        }
        let _ = child.kill().await;
        let _ = child.wait().await;
        let _ = std::fs::remove_file(&socket_path);
    });

    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(axum::http::header::CONNECTION, "Upgrade")
        .header(axum::http::header::UPGRADE, "SPDY/3.1")
        .header("X-Stream-Protocol-Version", protocol)
        .body(Body::empty())
        .unwrap()
}

/// A3: `kubectl port-forward` transport. Each client-opened SPDY stream has a
/// `port` header; the sidecar dials `127.0.0.1:<port>` inside the device netns.
async fn port_forward_handler(
    Extension(peer): Extension<PeerCert>,
    Path((namespace, pod)): Path<(String, String)>,
    mut req: axum::extract::Request,
) -> Response {
    if peer.0.is_none() {
        return (StatusCode::FORBIDDEN, "client certificate required").into_response();
    }
    let upgrade = req
        .headers()
        .get(axum::http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !upgrade.eq_ignore_ascii_case("SPDY/3.1") {
        return (StatusCode::BAD_REQUEST, "port-forward requires SPDY/3.1").into_response();
    }
    let protocol = req
        .headers()
        .get("x-stream-protocol-version")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("portforward.k8s.io")
        .to_string();

    let proot_path = match std::env::var("DROIDNODE_PROOT_PATH") {
        Ok(p) => p,
        Err(_) => {
            return (StatusCode::SERVICE_UNAVAILABLE, "DROIDNODE_PROOT_PATH is not set")
                .into_response()
        }
    };
    let helper = std::env::var("DROIDNODE_SPDY_HELPER").unwrap_or_else(|_| {
        std::path::Path::new(&proot_path)
            .with_file_name("libdroidnode_spdy.so")
            .to_string_lossy()
            .to_string()
    });
    if !std::path::Path::new(&helper).exists() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("SPDY sidecar not found at {helper}"),
        )
            .into_response();
    }
    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/data/local/tmp".to_string());
    let socket_path = std::path::Path::new(&tmpdir).join(format!(
        "droidnode-pf-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_file(&socket_path);
    let listener = match tokio::net::UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("spdy socket: {e}"))
                .into_response()
        }
    };
    let mut child = match tokio::process::Command::new(&helper)
        .arg(&socket_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&socket_path);
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("spdy spawn: {e}"))
                .into_response();
        }
    };
    let (mut stream, _) = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        listener.accept(),
    )
    .await
    {
        Ok(Ok(pair)) => pair,
        _ => {
            let _ = child.kill().await;
            let _ = std::fs::remove_file(&socket_path);
            return (StatusCode::INTERNAL_SERVER_ERROR, "spdy sidecar did not connect")
                .into_response();
        }
    };
    let metadata = serde_json::json!({
        "mode": "portforward",
        "namespace": namespace,
        "pod": pod,
        "tmpdir": tmpdir,
        "env": {},
    });
    let mut line = serde_json::to_vec(&metadata).unwrap_or_default();
    line.push(b'\n');
    if let Err(e) = stream.write_all(&line).await {
        let _ = child.kill().await;
        let _ = std::fs::remove_file(&socket_path);
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("spdy metadata: {e}"))
            .into_response();
    }
    let on_upgrade = hyper::upgrade::on(&mut req);
    tracing::info!(namespace = %namespace, pod = %pod, protocol = %protocol, "port-forward SPDY handoff");
    tokio::spawn(async move {
        match on_upgrade.await {
            Ok(upgraded) => {
                let mut upgraded = hyper_util::rt::TokioIo::new(upgraded);
                let _ = tokio::io::copy_bidirectional(&mut upgraded, &mut stream).await;
            }
            Err(e) => tracing::warn!(error = %e, "port-forward upgrade failed"),
        }
        let _ = child.kill().await;
        let _ = child.wait().await;
        let _ = std::fs::remove_file(&socket_path);
    });
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(axum::http::header::CONNECTION, "Upgrade")
        .header(axum::http::header::UPGRADE, "SPDY/3.1")
        .header("X-Stream-Protocol-Version", protocol)
        .body(Body::empty())
        .unwrap()
}

/// A2: `kubectl attach` over SPDY.
///
/// Rust exposes the running container's stdin/stdout/stderr as three Unix
/// sockets; the sidecar connects each SPDY stream to the matching socket.
async fn attach_handler(
    State(state): State<KubeletState>,
    Extension(peer): Extension<PeerCert>,
    Path((namespace, pod, container)): Path<(String, String, String)>,
    mut req: axum::extract::Request,
) -> Response {
    if peer.0.is_none() {
        return (StatusCode::FORBIDDEN, "client certificate required").into_response();
    }
    let Some(target) = state.attach_target(&namespace, &pod, &container).await else {
        return (StatusCode::NOT_FOUND, "container not running").into_response();
    };
    let upgrade = req
        .headers()
        .get(axum::http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !upgrade.eq_ignore_ascii_case("SPDY/3.1") {
        return (StatusCode::BAD_REQUEST, "attach requires SPDY/3.1").into_response();
    }
    let protocol = req
        .headers()
        .get("x-stream-protocol-version")
        .and_then(|v| v.to_str().ok())
        .filter(|p| p.starts_with("v3.") || p.starts_with("v4."))
        .unwrap_or("v4.channel.k8s.io")
        .to_string();

    let proot_path = match std::env::var("DROIDNODE_PROOT_PATH") {
        Ok(p) => p,
        Err(_) => {
            return (StatusCode::SERVICE_UNAVAILABLE, "DROIDNODE_PROOT_PATH is not set")
                .into_response()
        }
    };
    let helper = std::env::var("DROIDNODE_SPDY_HELPER").unwrap_or_else(|_| {
        std::path::Path::new(&proot_path)
            .with_file_name("libdroidnode_spdy.so")
            .to_string_lossy()
            .to_string()
    });
    if !std::path::Path::new(&helper).exists() {
        return (StatusCode::SERVICE_UNAVAILABLE, "SPDY sidecar missing").into_response();
    }
    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/data/local/tmp".to_string());
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base = format!("droidnode-attach-{}-{stamp}", std::process::id());
    let stdin_path = std::path::Path::new(&tmpdir).join(format!("{base}-in.sock"));
    let stdout_path = std::path::Path::new(&tmpdir).join(format!("{base}-out.sock"));
    let stderr_path = std::path::Path::new(&tmpdir).join(format!("{base}-err.sock"));
    let ctl_path = std::path::Path::new(&tmpdir).join(format!("{base}-ctl.sock"));
    for p in [&stdin_path, &stdout_path, &stderr_path, &ctl_path] {
        let _ = std::fs::remove_file(p);
    }
    let stdin_listener = match tokio::net::UnixListener::bind(&stdin_path) {
        Ok(l) => l,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("stdin sock: {e}")).into_response(),
    };
    let stdout_listener = match tokio::net::UnixListener::bind(&stdout_path) {
        Ok(l) => l,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("stdout sock: {e}")).into_response(),
    };
    let stderr_listener = match tokio::net::UnixListener::bind(&stderr_path) {
        Ok(l) => l,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("stderr sock: {e}")).into_response(),
    };
    let ctl_listener = match tokio::net::UnixListener::bind(&ctl_path) {
        Ok(l) => l,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("ctl sock: {e}")).into_response(),
    };
    let mut child = match tokio::process::Command::new(&helper)
        .arg(&ctl_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("attach sidecar: {e}")).into_response(),
    };
    let (mut ctl_stream, _) = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        ctl_listener.accept(),
    )
    .await
    {
        Ok(Ok(pair)) => pair,
        _ => {
            let _ = child.kill().await;
            return (StatusCode::INTERNAL_SERVER_ERROR, "attach sidecar did not connect").into_response();
        }
    };
    let metadata = serde_json::json!({
        "mode": "attach",
        "namespace": namespace,
        "pod": pod,
        "container": container,
        "stdin_path": stdin_path.to_string_lossy(),
        "stdout_path": stdout_path.to_string_lossy(),
        "stderr_path": stderr_path.to_string_lossy(),
        "tmpdir": tmpdir,
        "env": {},
    });
    let mut line = serde_json::to_vec(&metadata).unwrap_or_default();
    line.push(b'\n');
    if let Err(e) = ctl_stream.write_all(&line).await {
        let _ = child.kill().await;
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("attach metadata: {e}")).into_response();
    }

    let on_upgrade = hyper::upgrade::on(&mut req);
    let paths = (stdin_path.clone(), stdout_path.clone(), stderr_path.clone(), ctl_path.clone());
    tokio::spawn(async move {
        let out_target = target.clone();
        let err_target = target.clone();
        let in_target = target;
        let out_task = tokio::spawn(async move {
            if let Ok((mut sock, _)) = stdout_listener.accept().await {
                let mut rx = out_target.output.subscribe();
                loop {
                    match rx.recv().await {
                        Ok(frame) if frame.channel == 1 => {
                            if sock.write_all(&frame.data).await.is_err() {
                                break;
                            }
                        }
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
                let _ = sock.shutdown().await;
            }
        });
        let err_task = tokio::spawn(async move {
            if let Ok((mut sock, _)) = stderr_listener.accept().await {
                let mut rx = err_target.output.subscribe();
                loop {
                    match rx.recv().await {
                        Ok(frame) if frame.channel == 2 => {
                            if sock.write_all(&frame.data).await.is_err() {
                                break;
                            }
                        }
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
                let _ = sock.shutdown().await;
            }
        });
        let in_task = tokio::spawn(async move {
            if let Ok((mut sock, _)) = stdin_listener.accept().await {
                let mut buf = [0u8; 8192];
                loop {
                    match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if in_target.stdin.send(buf[..n].to_vec()).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        });

        match on_upgrade.await {
            Ok(upgraded) => {
                let mut upgraded = hyper_util::rt::TokioIo::new(upgraded);
                let _ = tokio::io::copy_bidirectional(&mut upgraded, &mut ctl_stream).await;
            }
            Err(e) => tracing::warn!(error = %e, "attach upgrade failed"),
        }
        out_task.abort();
        err_task.abort();
        in_task.abort();
        let _ = child.kill().await;
        let _ = child.wait().await;
        for p in [paths.0, paths.1, paths.2, paths.3] {
            let _ = std::fs::remove_file(p);
        }
    });

    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(axum::http::header::CONNECTION, "Upgrade")
        .header(axum::http::header::UPGRADE, "SPDY/3.1")
        .header("X-Stream-Protocol-Version", protocol)
        .body(Body::empty())
        .unwrap()
}

/// E7: ask the agent to stop its pods gracefully but keep running.
async fn node_drain(State(state): State<KubeletState>) -> impl IntoResponse {
    state
        .drained
        .store(true, std::sync::atomic::Ordering::Relaxed);
    tracing::warn!("node drain requested - pods will be stopped gracefully, agent keeps running");
    (
        StatusCode::OK,
        Json(serde_json::json!({ "drained": true })),
    )
}

async fn node_undrain(State(state): State<KubeletState>) -> impl IntoResponse {
    // Battery recovery must not undo an operator's maintenance mode.
    if state.is_maintenance() {
        tracing::info!("undrain ignored while maintenance mode is active");
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "drained": true,
                "maintenance": true,
                "message": "maintenance mode is active; use /maintenance?on=false"
            })),
        );
    }
    state
        .drained
        .store(false, std::sync::atomic::Ordering::Relaxed);
    tracing::info!("node undrain requested - resuming pod management");
    (
        StatusCode::OK,
        Json(serde_json::json!({ "drained": false, "maintenance": false })),
    )
}

#[derive(serde::Deserialize)]
struct MaintenanceQuery {
    on: bool,
}

/// G2: operator maintenance mode.
///
/// `on=true` cordons the Node (`spec.unschedulable=true`) and drains locally;
/// `on=false` resumes scheduling and lets the reconciler restart containers.
/// This is the only path that changes `spec.unschedulable`; `/undrain` is the
/// battery-recovery path and is refused while maintenance is active.
async fn node_maintenance(
    State(state): State<KubeletState>,
    Query(query): Query<MaintenanceQuery>,
) -> impl IntoResponse {
    let on = query.on;
    state
        .maintenance
        .store(on, std::sync::atomic::Ordering::Relaxed);
    state
        .drained
        .store(on, std::sync::atomic::Ordering::Relaxed);

    let (mut applied, mut message) = (false, String::new());
    match state.control_plane() {
        Some(cp) => match cp.set_unschedulable(on).await {
            Ok(()) => {
                state
                    .cordoned
                    .store(on, std::sync::atomic::Ordering::Relaxed);
                applied = true;
            }
            Err(e) => {
                message = format!("cordon failed: {e}");
                tracing::warn!(error = %e, on, "maintenance: could not update spec.unschedulable");
            }
        },
        None => message = "control plane handle not attached".into(),
    }

    tracing::warn!(on, applied, %message, "maintenance mode changed");
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "maintenance": on,
            "drained": on,
            "cordoned": state.cordoned.load(std::sync::atomic::Ordering::Relaxed),
            "applied": applied,
            "message": message,
        })),
    )
}

/// Kubelet Summary API (`/stats/summary`).
///
/// This is what the Kubernetes dashboard and Headlamp use for the node's
/// CPU / Memory / Disk / Pods charts; metrics-server's API alone is not enough,
/// so the page showed "Metrics not available at the moment".
async fn stats_summary(State(state): State<KubeletState>) -> impl IntoResponse {
    use chrono::Utc;
    use serde_json::json;

    let now = Utc::now();
    let rfc3339 = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    // CPU rate from two samples of the cumulative counter.
    let cpu_total = crate::proc_metrics::node_cpu_seconds().unwrap_or(0.0);
    let nano_cores = {
        let mut cache = state.summary_cpu.write().await;
        let rate = match *cache {
            Some((prev_at, prev_total)) => {
                let dt = (now - prev_at).num_milliseconds() as f64 / 1000.0;
                if dt >= 0.5 {
                    ((cpu_total - prev_total).max(0.0) / dt * 1e9) as u64
                } else {
                    0
                }
            }
            None => 0,
        };
        *cache = Some((now, cpu_total));
        rate
    };

    let (mem_total, mem_available) = crate::proc_metrics::node_memory().unwrap_or((0, 0));
    let mem_used = mem_total.saturating_sub(mem_available);

    // Disk usage for the app data dir (the only filesystem we can statvfs).
    let (fs_capacity, fs_available) = {
        let path = state.ensure_log_dir();
        statvfs_pair(&path).unwrap_or((0, 0))
    };
    let fs_used = fs_capacity.saturating_sub(fs_available);

    let fs_obj = |ts: &str| {
        json!({
            "time": ts,
            "availableBytes": fs_available,
            "capacityBytes": fs_capacity,
            "usedBytes": fs_used,
            "inodesFree": 0, "inodes": 0, "inodesUsed": 0,
        })
    };
    let cpu_obj = |rate: u64, total: u64| json!({ "usageNanoCores": rate, "usageCoreNanoSeconds": total });

    let procs: Vec<(String, String, String, i32)> =
        state.pod_procs.read().await.values().cloned().collect();
    let statuses = state.pod_statuses.read().await;

    let mut pods_json = Vec::new();
    for (namespace, pod, container, pgid) in &procs {
        let (cpu_secs, rss) = crate::proc_metrics::process_group_usage(*pgid);
        let uid = statuses
            .values()
            .find(|s| &s.pod_name == pod && &s.namespace == namespace)
            .map(|s| s.pod_uid.clone())
            .unwrap_or_default();
        let start = statuses
            .values()
            .find(|s| &s.pod_name == pod && &s.namespace == namespace)
            .and_then(|s| s.containers.first().and_then(|c| c.started_at.clone()))
            .unwrap_or_else(|| rfc3339.clone());

        pods_json.push(json!({
            "podRef": { "name": pod, "namespace": namespace, "uid": uid },
            "startTime": start,
            "containers": [{
                "name": container,
                "startTime": start,
                "cpu": cpu_obj(0, (cpu_secs * 1e9) as u64),
                "memory": {
                    "time": rfc3339,
                    "usageBytes": rss,
                    "workingSetBytes": rss,
                    "rssBytes": rss,
                    "availableBytes": 0,
                    "pageFaults": 0,
                    "majorPageFaults": 0
                },
                "rootfs": fs_obj(&rfc3339),
                "logs": fs_obj(&rfc3339),
            }],
            "network": { "time": rfc3339, "name": "default", "rxBytes": 0, "rxErrors": 0, "txBytes": 0, "txErrors": 0, "interfaces": [] },
            "volume": [],
            "cpu": cpu_obj(0, (cpu_secs * 1e9) as u64),
            "memory": {
                "time": rfc3339,
                "usageBytes": rss,
                "workingSetBytes": rss,
                "rssBytes": rss,
                "availableBytes": 0,
                "pageFaults": 0,
                "majorPageFaults": 0
            }
        }));
    }

    let summary = json!({
        "node": {
            "nodeName": state.node_name_hint().await,
            "systemContainers": [],
            "startTime": rfc3339,
            "machine": {
                "timestamp": rfc3339,
                "cpu": cpu_obj(nano_cores, (cpu_total * 1e9) as u64),
                "memory": {
                    "time": rfc3339,
                    "availableBytes": mem_available,
                    "usageBytes": mem_used,
                    "workingSetBytes": mem_used,
                    "rssBytes": mem_used,
                    "pageFaults": 0,
                    "majorPageFaults": 0
                },
                "network": {
                    "time": rfc3339, "name": "default",
                    "rxBytes": 0, "rxErrors": 0, "txBytes": 0, "txErrors": 0, "interfaces": []
                },
                "fs": fs_obj(&rfc3339)
            },
            "network": {
                "time": rfc3339, "name": "default",
                "rxBytes": 0, "rxErrors": 0, "txBytes": 0, "txErrors": 0, "interfaces": []
            },
            "fs": fs_obj(&rfc3339),
            "runtime": {
                "imageFs": fs_obj(&rfc3339),
                "imageFsInodesFree": 0, "imageFsInodes": 0, "imageFsInodesUsed": 0,
                "nodeFs": fs_obj(&rfc3339),
                "nodeFsInodesFree": 0, "nodeFsInodes": 0, "nodeFsInodesUsed": 0,
                "containerFs": fs_obj(&rfc3339)
            },
            "rctl": { "cpuCFSQuotaPeriod": 100000000 }
        },
        "pods": pods_json
    });

    Json(summary)
}

/// (capacity, available) of the filesystem holding `path`.
fn statvfs_pair(path: &std::path::Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        let c = CString::new(path.to_string_lossy().to_string()).ok()?;
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
            return None;
        }
        let cap = st.f_frsize as u64 * st.f_blocks as u64;
        let avail = st.f_frsize as u64 * st.f_bavail as u64;
        Some((cap, avail))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Kubelet resource metrics (`/metrics/resource`), scraped by metrics-server to
/// power `kubectl top node/pod` and the CPU/memory panels in dashboards such as
/// Headlamp. Without this endpoint both show `<unknown>` (A11).
async fn metrics_resource(State(state): State<KubeletState>) -> impl IntoResponse {
    let mut out = String::new();

    // ── node ────────────────────────────────────────────────────────────────
    if let Some(cpu) = crate::proc_metrics::node_cpu_seconds() {
        out.push_str("# HELP node_cpu_usage_seconds_total [ALPHA] Cumulative cpu time consumed by the node in seconds\n");
        out.push_str("# TYPE node_cpu_usage_seconds_total counter\n");
        out.push_str(&format!("node_cpu_usage_seconds_total {cpu}\n"));
    }
    if let Some((total, available)) = crate::proc_metrics::node_memory() {
        let working_set = total.saturating_sub(available);
        out.push_str("# HELP node_memory_working_set_bytes [ALPHA] Current working set of the node in bytes\n");
        out.push_str("# TYPE node_memory_working_set_bytes gauge\n");
        out.push_str(&format!("node_memory_working_set_bytes {working_set}\n"));
    }

    // ── containers ──────────────────────────────────────────────────────────
    let procs: Vec<(String, String, String, i32)> =
        state.pod_procs.read().await.values().cloned().collect();

    if !procs.is_empty() {
        out.push_str("# HELP container_cpu_usage_seconds_total [ALPHA] Cumulative cpu time consumed by the container in seconds\n");
        out.push_str("# TYPE container_cpu_usage_seconds_total counter\n");
    }
    let mut mem_lines = String::new();
    for (namespace, pod, container, pgid) in &procs {
        let (cpu, rss) = crate::proc_metrics::process_group_usage(*pgid);
        out.push_str(&format!(
            "container_cpu_usage_seconds_total{{container=\"{container}\",namespace=\"{namespace}\",pod=\"{pod}\"}} {cpu}\n"
        ));
        mem_lines.push_str(&format!(
            "container_memory_working_set_bytes{{container=\"{container}\",namespace=\"{namespace}\",pod=\"{pod}\"}} {rss}\n"
        ));
    }
    if !procs.is_empty() {
        out.push_str("# HELP container_memory_working_set_bytes [ALPHA] Current working set of the container in bytes\n");
        out.push_str("# TYPE container_memory_working_set_bytes gauge\n");
        out.push_str(&mem_lines);
    }

    (
        [(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        out,
    )
}

#[derive(serde::Deserialize)]
struct DeleteImageQuery {
    /// Absolute rootfs directory as returned by `/images`.
    id: String,
}

async fn list_images(State(state): State<KubeletState>) -> impl IntoResponse {
    let base = (*state.rootfs_base).clone();
    let images = tokio::task::spawn_blocking(move || {
        crate::services::foundation::image_store::list(&base)
    })
    .await
    .unwrap_or_default();

    let total: u64 = images.iter().map(|i| i.total_size()).sum();
    Json(serde_json::json!({
        "images": images,
        "total_size": total,
        "count": images.len(),
    }))
}

async fn delete_image(
    State(state): State<KubeletState>,
    Query(q): Query<DeleteImageQuery>,
) -> impl IntoResponse {
    if state.is_in_use(&q.id).await {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "镜像正在被运行中的 Pod 使用，请先删除该 Pod"
            })),
        );
    }

    let base = (*state.rootfs_base).clone();
    let layers = (*state.layers_base).clone();
    let id = q.id.clone();

    let result = tokio::task::spawn_blocking(move || {
        crate::services::foundation::image_store::delete(&base, &layers, &id)
    })
    .await;

    match result {
        Ok(Ok(bytes)) => (
            StatusCode::OK,
            Json(serde_json::json!({ "deleted": q.id, "freed_bytes": bytes })),
        ),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("join error: {e}") })),
        ),
    }
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

/// `/pods` returns a standard `v1.PodList` (A19). Tools that re-use the kubelet
/// API expect `kind: PodList` + `items[]`, not a custom envelope.
async fn list_pods(State(state): State<KubeletState>) -> impl IntoResponse {
    use serde_json::json;

    let statuses = state.pod_statuses.read().await;
    let node_name = state.node_name_hint().await;
    let node_ip = crate::net_util::primary_ipv4();

    let items: Vec<serde_json::Value> = statuses
        .values()
        .map(|s| {
            let containers: Vec<serde_json::Value> = s
                .containers
                .iter()
                .map(|c| {
                    let state_json = if c.running {
                        json!({ "running": { "startedAt": c.started_at } })
                    } else if let Some(w) = &c.waiting_reason {
                        json!({ "waiting": { "reason": w, "message": s.message } })
                    } else {
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
                        "state": state_json
                    })
                })
                .collect();

            json!({
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": {
                    "name": s.pod_name,
                    "namespace": s.namespace,
                    "uid": s.pod_uid,
                },
                "spec": { "nodeName": node_name },
                "status": {
                    "phase": s.phase.as_str(),
                    "podIP": node_ip,
                    "podIPs": [ { "ip": node_ip } ],
                    "hostIP": node_ip,
                    "message": s.message,
                    "containerStatuses": containers,
                }
            })
        })
        .collect();

    Json(json!({
        "kind": "PodList",
        "apiVersion": "v1",
        "metadata": {},
        "items": items,
    }))
}

#[derive(serde::Deserialize)]
struct LogQuery {
    follow: Option<bool>,
    #[serde(rename = "tailLines")]
    tail_lines: Option<i64>,
    #[serde(rename = "sinceSeconds")]
    since_seconds: Option<i64>,
    #[serde(rename = "sinceTime")]
    since_time: Option<String>,
    timestamps: Option<bool>,
    /// Dashboards send this for crash-looping containers.
    previous: Option<bool>,
    /// Accepted and ignored (we do not truncate mid-line).
    #[serde(rename = "limitBytes")]
    limit_bytes: Option<i64>,
}

async fn container_logs(
    State(state): State<KubeletState>,
    Path((namespace, pod_id, container_name)): Path<(String, String, String)>,
    Query(q): Query<LogQuery>,
) -> Response {
    let key = format!("{}/{}/{}", namespace, pod_id, container_name);
    let tail = q.tail_lines.unwrap_or(200).clamp(0, 10_000) as usize;
    let timestamps = q.timestamps.unwrap_or(false);
    let _ = q.limit_bytes;

    // `--previous` now reads the rotated file (the container instance that ran
    // before the current one).
    if q.previous.unwrap_or(false) {
        let prev = state.logs.tail_previous(&key, tail).await;
        if prev.is_empty() {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header("Content-Type", "text/plain")
                .body(Body::from(
                    "previous terminated container \"\".log not found: \
                     no earlier instance of this container was recorded\n",
                ))
                .unwrap();
        }
        let body = prev
            .iter()
            .map(|l| l.render(timestamps) + "\n")
            .collect::<String>();
        return Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/plain")
            .body(Body::from(body))
            .unwrap();
    }

    // sinceSeconds / sinceTime are honoured so dashboards that ask for "last 5
    // minutes" do not get the whole buffer.
    let now = chrono::Utc::now();
    let since = if let Some(secs) = q.since_seconds {
        Some(now - chrono::Duration::seconds(secs.max(0)))
    } else if let Some(ref ts) = q.since_time {
        chrono::DateTime::parse_from_rfc3339(ts)
            .ok()
            .map(|t| t.with_timezone(&chrono::Utc))
    } else {
        None
    };

    let initial = state.logs.tail(&key, tail, since).await;
    // After an agent restart the in-memory buffer is empty but the file still
    // has the output - read it so `kubectl logs` is not blank.
    let initial = if initial.is_empty() {
        let from_file = state.logs.tail_file(&key, tail).await;
        if since.is_none() {
            from_file
        } else {
            from_file
                .into_iter()
                .filter(|l| since.is_some_and(|s| l.at >= s))
                .collect()
        }
    } else {
        initial
    };

    if q.follow.unwrap_or(false) {
        let rx = state.logs.subscribe(&key).await;
        let head = initial
            .iter()
            .map(|l| l.render(timestamps) + "\n")
            .collect::<String>();
        let head_stream = futures::stream::once(async move {
            Ok::<_, std::io::Error>(bytes::Bytes::from(head))
        });
        // Ends the stream on EOF so dashboards stop "tailing forever".
        let live = BroadcastStream::new(rx)
            .filter_map(move |item| async move {
                match item {
                    Ok(line) => Some((line.eof, line.render(timestamps))),
                    // Lagged: the follower missed lines, keep the connection open.
                    Err(_) => None,
                }
            })
            .take_while(|(eof, _)| futures::future::ready(!*eof))
            .map(|(_, text)| Ok::<_, std::io::Error>(bytes::Bytes::from(text + "\n")));
        return Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/plain")
            .body(Body::from_stream(head_stream.chain(live)))
            .unwrap();
    }

    let output = initial
        .iter()
        .map(|l| l.render(timestamps) + "\n")
        .collect::<String>();
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/plain")
        .body(Body::from(output))
        .unwrap()
}

async fn metrics(State(state): State<KubeletState>) -> impl IntoResponse {
    let mut out = String::new();

    // E1/E12: real device telemetry pushed by the app (battery + temperature +
    // network). Before this the endpoint emitted nothing at all because
    // `node_profile` was never populated.
    if let Some(device) = state.device_state() {
        let telemetry = device.snapshot().await;
        if telemetry.is_fresh() {
            if let Some(percent) = telemetry.battery_percent {
                out.push_str(&format!(
                    "# HELP droidnode_battery_percent Battery charge percentage\n\
                     # TYPE droidnode_battery_percent gauge\n\
                     droidnode_battery_percent {percent}\n"
                ));
            }
            if let Some(charging) = telemetry.battery_charging {
                out.push_str(&format!(
                    "# HELP droidnode_battery_charging 1 when the device is charging\n\
                     # TYPE droidnode_battery_charging gauge\n\
                     droidnode_battery_charging {}\n",
                    if charging { 1 } else { 0 }
                ));
            }
            if let Some(celsius) = telemetry.battery_temperature_c {
                out.push_str(&format!(
                    "# HELP droidnode_battery_temperature_celsius Battery temperature\n\
                     # TYPE droidnode_battery_temperature_celsius gauge\n\
                     droidnode_battery_temperature_celsius {celsius}\n"
                ));
            }
            if let Some(kind) = telemetry.network_type.as_deref() {
                out.push_str(&format!(
                    "# HELP droidnode_network_up 1 when a network is available\n\
                     # TYPE droidnode_network_up gauge\n\
                     droidnode_network_up {}\n",
                    if kind == "none" || kind.is_empty() { 0 } else { 1 }
                ));
            }
        }
    }

    let memory = crate::services::foundation::node_capability_service::read_memory_info();
    out.push_str(&format!(
        "# HELP droidnode_memory_available_bytes Available memory in bytes\n\
         # TYPE droidnode_memory_available_bytes gauge\n\
         droidnode_memory_available_bytes {}\n",
        memory.available_bytes
    ));
    out.push_str(&format!(
        "# HELP droidnode_memory_total_bytes Total memory in bytes\n\
         # TYPE droidnode_memory_total_bytes gauge\n\
         droidnode_memory_total_bytes {}\n",
        memory.total_bytes
    ));

    let storage = crate::services::foundation::node_capability_service::read_storage_info(
        &state.rootfs_base,
    );
    out.push_str(&format!(
        "# HELP droidnode_storage_available_bytes Available storage in bytes\n\
         # TYPE droidnode_storage_available_bytes gauge\n\
         droidnode_storage_available_bytes {}\n",
        storage.available_bytes
    ));
    out.push_str(&format!(
        "# HELP droidnode_storage_total_bytes Total storage in bytes\n\
         # TYPE droidnode_storage_total_bytes gauge\n\
         droidnode_storage_total_bytes {}\n",
        storage.total_bytes
    ));

    let pods = crate::status_ipc::running_pods(&state).await;
    out.push_str(&format!(
        "# HELP droidnode_pods_running Pods with at least one running container\n\
         # TYPE droidnode_pods_running gauge\n\
         droidnode_pods_running {pods}\n"
    ));

    (
        StatusCode::OK,
        [("Content-Type", "text/plain; version=0.0.4")],
        out,
    )
}
