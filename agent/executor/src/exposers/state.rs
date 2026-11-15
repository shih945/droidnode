use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::brokers::ControlPlaneBroker;
use crate::models::{NodeProfile, PodRunStatus};
use crate::status_ipc::DeviceState;

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
        self.maintenance.load(std::sync::atomic::Ordering::Relaxed)
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
