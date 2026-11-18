use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// Re-export k8s types used throughout
pub use k8s_openapi::api::core::v1::Pod;

// ─── OCI types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ImageRef {
    pub registry: String,
    pub repository: String,
    pub reference: String, // tag or digest
}

impl ImageRef {
    pub fn parse(s: &str) -> crate::error::Result<Self> {
        let mut image = Self::parse_unmirrored(s)?;
        // Rewrite well-known upstream registries to a reachable mirror
        // (docker.io is unreachable from CN mobile networks).
        crate::registry_mirror::apply(&mut image);
        Ok(image)
    }

    /// Same as [`Self::parse`] but keeps the registry the Pod asked for.
    ///
    /// `imagePullSecrets` are keyed by the *upstream* registry
    /// (`https://index.docker.io/v1/`, `registry.example.com:5000`), while the
    /// pull itself may go through the mirror. Auth lookup therefore needs the
    /// original host.
    pub fn parse_unmirrored(s: &str) -> crate::error::Result<Self> {
        let s = s.trim();

        // Split off digest or tag from the rightmost ':' or '@'
        let (name_part, reference) = if let Some(at) = s.find('@') {
            (&s[..at], s[at + 1..].to_string())
        } else {
            // Find the last colon that isn't part of a host:port
            let slash_pos = s.find('/').unwrap_or(0);
            if let Some(colon) = s[slash_pos..].rfind(':') {
                let abs = slash_pos + colon;
                (&s[..abs], s[abs + 1..].to_string())
            } else {
                (s, "latest".to_string())
            }
        };

        // Determine registry vs repository
        let first_slash = name_part.find('/');
        let (registry, repository) = match first_slash {
            Some(idx) => {
                let host = &name_part[..idx];
                // A host contains a dot or colon (e.g. "docker.io", "localhost:5000")
                if host.contains('.') || host.contains(':') {
                    (host.to_string(), name_part[idx + 1..].to_string())
                } else {
                    // Docker Hub short form: "library/ubuntu" → docker.io
                    ("registry-1.docker.io".to_string(), name_part.to_string())
                }
            }
            None => {
                // No slash → official image like "ubuntu"
                (
                    "registry-1.docker.io".to_string(),
                    format!("library/{}", name_part),
                )
            }
        };

        Ok(ImageRef {
            registry,
            repository,
            reference,
        })
    }

    pub fn registry_url(&self) -> String {
        // Local/self-hosted registries often run without TLS during bring-up.
        // An explicit DROIDNODE_INSECURE_REGISTRIES entry wins; a registry on a
        // private/loopback IP also defaults to HTTP because a LAN registry
        // reached from the phone almost never has a trusted certificate.
        let scheme = if crate::net_util::is_insecure_registry(&self.registry)
            || crate::net_util::is_private_registry(&self.registry)
        {
            "http"
        } else {
            "https"
        };
        format!("{scheme}://{}", self.registry)
    }
}

/// Credentials resolved from an `imagePullSecret`.
///
/// `Debug` is hand-written so a stray `{auth:?}` / `#[instrument]` can never
/// print the password into the agent log.
#[derive(Clone, PartialEq, Eq)]
pub struct RegistryAuth {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for RegistryAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryAuth")
            .field("username", &self.username)
            .field("password", &"***")
            .finish()
    }
}

/// Soft resource limits applied to the proot process (and inherited by the
/// container's tracees).
///
/// This is deliberately NOT cgroup semantics: `RLIMIT_AS` caps virtual address
/// space (an over-limit allocation fails instead of being OOM-killed), and a
/// fractional CPU limit only lowers scheduling priority via `nice`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContainerLimits {
    pub memory_bytes: Option<u64>,
    /// CPU limit in millicores (e.g. `500m` → 500). Used only to pick a nice value.
    pub cpu_millis: Option<u64>,
}

impl ContainerLimits {
    pub fn is_empty(&self) -> bool {
        self.memory_bytes.is_none() && self.cpu_millis.is_none()
    }

    /// `nice` value to apply: full cores keep the default priority, fractional
    /// limits get deprioritised so the phone's foreground work wins.
    pub fn nice_value(&self) -> i32 {
        match self.cpu_millis {
            Some(m) if m < 1000 => 10,
            Some(m) if m < 2000 => 5,
            _ => 0,
        }
    }
}

/// Parse a Kubernetes memory quantity into bytes.
///
/// Supports binary suffixes (`Ki/Mi/Gi/Ti`), decimal suffixes (`K/M/G/T`) and
/// plain bytes. Fractional values (`1.5Gi`) are rounded down.
pub fn parse_memory_bytes(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let split = value
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(value.len());
    let (number, suffix) = value.split_at(split);
    let number: f64 = number.parse().ok()?;
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    let multiplier: f64 = match suffix.trim() {
        "" => 1.0,
        "Ki" => 1024.0,
        "Mi" => 1024.0 * 1024.0,
        "Gi" => 1024.0 * 1024.0 * 1024.0,
        "Ti" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        "K" | "k" => 1_000.0,
        "M" => 1_000_000.0,
        "G" => 1_000_000_000.0,
        "T" => 1_000_000_000_000.0,
        _ => return None,
    };
    let bytes = number * multiplier;
    if bytes > u64::MAX as f64 {
        return None;
    }
    Some(bytes as u64)
}

/// Parse a Kubernetes CPU quantity into millicores (`500m` → 500, `2` → 2000).
pub fn parse_cpu_millis(value: &str) -> Option<u64> {
    let value = value.trim();
    if let Some(millis) = value.strip_suffix('m') {
        return millis.trim().parse::<u64>().ok();
    }
    let cores: f64 = value.parse().ok()?;
    if !cores.is_finite() || cores <= 0.0 {
        return None;
    }
    Some((cores * 1000.0) as u64)
}

// ─── Digest ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Digest(pub String);

impl Digest {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn hex(&self) -> &str {
        self.0.strip_prefix("sha256:").unwrap_or(&self.0)
    }

    // Safe filesystem name: replace ':' with '-'
    pub fn as_fs_name(&self) -> String {
        self.0.replace(':', "-")
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ─── OCI Manifest ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Descriptor {
    #[serde(rename = "mediaType", default)]
    pub media_type: String,
    pub digest: String,
    pub size: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    #[serde(rename = "mediaType", default)]
    pub media_type: String,
    pub config: Descriptor,
    pub layers: Vec<Descriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImageConfig {
    pub architecture: Option<String>,
    pub os: Option<String>,
    pub config: Option<ContainerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContainerConfig {
    #[serde(rename = "Entrypoint")]
    pub entrypoint: Option<Vec<String>>,
    #[serde(rename = "Cmd")]
    pub cmd: Option<Vec<String>>,
    #[serde(rename = "Env")]
    pub env: Option<Vec<String>>,
    #[serde(rename = "WorkingDir")]
    pub working_dir: Option<String>,
    #[serde(rename = "User")]
    pub user: Option<String>,
}

// ─── Runtime types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Mount {
    pub source: PathBuf,
    pub target: PathBuf,
    pub read_only: bool,
}

// ─── Node profile ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeProfile {
    pub node_id: String,
    pub cpu: CpuInfo,
    pub memory: MemoryInfo,
    pub storage: StorageInfo,
    pub runtime: RuntimeInfo,
    pub battery: BatteryInfo,
    pub network: NetworkInfo,
    pub conditions: NodeConditions,
    /// Images present on the device, reported in `Node.status.images`.
    #[serde(default)]
    pub images: Vec<NodeImageInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuInfo {
    pub arch: String,
    pub cores: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryInfo {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageInfo {
    /// Total size of the filesystem backing the image/pod data directory.
    /// `default` keeps older serialized profiles (only `available_bytes`)
    /// deserializable; a missing total is reported as 0 rather than failing.
    #[serde(default)]
    pub total_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BatteryInfo {
    pub percent: u8,
    pub charging: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NetworkInfo {
    pub network_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConditions {
    pub ready: bool,
    pub battery_pressure: bool,
    pub memory_pressure: bool,
    pub network_available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeStatus {
    pub node_id: String,
    pub conditions: NodeConditions,
    pub memory: MemoryInfo,
    pub storage: StorageInfo,
    pub battery: BatteryInfo,
    pub network: NetworkInfo,
    /// Mirrors `Node.status.images` (`names` + `sizeBytes`).
    #[serde(default)]
    pub images: Vec<NodeImageInfo>,
}

/// One entry of `Node.status.images` (core/v1 `ContainerImage`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NodeImageInfo {
    /// e.g. `alpine:latest` plus its digest-pinned form when known.
    pub names: Vec<String>,
    pub size_bytes: u64,
}

/// `spec.containers[].imagePullPolicy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullPolicy {
    Always,
    IfNotPresent,
    Never,
}

impl PullPolicy {
    /// Kubernetes default: `Always` for `:latest` (or no tag), else `IfNotPresent`.
    pub fn resolve(policy: Option<&str>, reference: &str) -> Self {
        match policy {
            Some("Always") => Self::Always,
            Some("Never") => Self::Never,
            Some("IfNotPresent") => Self::IfNotPresent,
            _ => {
                if reference.is_empty() || reference == "latest" {
                    Self::Always
                } else {
                    Self::IfNotPresent
                }
            }
        }
    }
}

/// The Kubernetes API version this kubelet implementation is compiled against.
///
/// Single source of truth for everything we report. It is **pinned on purpose**:
/// `k8s-openapi` 0.21 only ships schemas up to `v1_29`, so 1.29 is both the
/// newest schema available and a real stable release.
///
/// Compatibility policy: we only use long-stable `core/v1` resources
/// (`nodes`, `pods`, `pods/status`, `events`) plus our own kubelet HTTP surface,
/// so a newer control plane (e.g. 1.36) still works - but fields added after
/// 1.29 are intentionally NOT surfaced. Newer-version features are out of scope.
pub const KUBELET_VERSION: &str = "v1.29.0";

/// Minor version of [`KUBELET_VERSION`], used for the startup skew check.
pub const KUBELET_MINOR: u32 = 29;

// ─── Events ───────────────────────────────────────────────────────────────────
/// Identifies the object an event is about. kubelet always sets
/// `involvedObject`; without it `kubectl get events --field-selector
/// involvedObject.name=...` and `kubectl describe pod` show nothing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PodRef {
    pub name: String,
    pub namespace: String,
    pub uid: Option<String>,
}

impl PodRef {
    pub fn from_pod(pod: &Pod) -> Self {
        Self {
            name: pod.metadata.name.clone().unwrap_or_default(),
            namespace: pod
                .metadata
                .namespace
                .clone()
                .unwrap_or_else(|| "default".to_string()),
            uid: pod.metadata.uid.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeEvent {
    pub node_id: String,
    pub event_type: EventType,
    pub reason: String,
    pub message: String,
    pub pod_id: Option<String>,
    /// Object the event refers to (name + namespace + uid).
    #[serde(default)]
    pub pod: Option<PodRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventType {
    Normal,
    Warning,
}

// ─── Pod run status ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PodRunStatus {
    pub pod_uid: String,
    pub pod_name: String,
    pub namespace: String,
    pub phase: PodPhase,
    pub message: Option<String>,
    pub containers: Vec<ContainerRunStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PodPhase {
    Pending,
    Running,
    Succeeded,
    Failed,
    Unknown,
}

impl PodPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Running => "Running",
            Self::Succeeded => "Succeeded",
            Self::Failed => "Failed",
            Self::Unknown => "Unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerRunStatus {
    pub name: String,
    pub ready: bool,
    pub running: bool,
    pub exit_code: Option<i32>,
    /// How many times this container has been restarted (kubelet's `restartCount`).
    #[serde(default)]
    pub restart_count: i32,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    /// "Completed" | "Error" - what kubectl renders instead of `ExitCode:N`.
    #[serde(default)]
    pub reason: Option<String>,
    /// Set while the container is waiting to be restarted (kubelet uses
    /// "CrashLoopBackOff" here, which is what kubectl shows in STATUS).
    #[serde(default)]
    pub waiting_reason: Option<String>,
    /// Why it is waiting (image pull error, port conflict, ...). kubectl prints
    /// this under the container state; without it every waiting pod looked like
    /// a crash loop, which made slow pulls impossible to diagnose.
    #[serde(default)]
    pub waiting_message: Option<String>,
}

// ─── Health probes ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum ProbeType {
    Startup,
    Liveness,
    Readiness,
}

#[derive(Debug, Clone)]
pub struct HttpProbe {
    pub host: String,
    pub port: u16,
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct ProbeConfig {
    pub probe_type: ProbeType,
    pub exec_command: Option<Vec<String>>,
    pub http_get: Option<HttpProbe>,
    /// (host, port) for tcpSocket probes.
    pub tcp_socket: Option<(String, u16)>,
    pub initial_delay_seconds: u32,
    pub period_seconds: u32,
    pub failure_threshold: u32,
    pub success_threshold: u32,
    pub timeout_seconds: u32,
}

#[cfg(test)]
mod limit_tests {
    use super::*;

    #[test]
    fn parses_memory_quantities() {
        assert_eq!(parse_memory_bytes("256Mi"), Some(256 * 1024 * 1024));
        assert_eq!(parse_memory_bytes("1Gi"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_memory_bytes("512M"), Some(512_000_000));
        assert_eq!(parse_memory_bytes("1024"), Some(1024));
        assert_eq!(parse_memory_bytes("1.5Gi"), Some(1536 * 1024 * 1024));
        assert_eq!(parse_memory_bytes("nope"), None);
        assert_eq!(parse_memory_bytes(""), None);
    }

    #[test]
    fn parses_cpu_quantities() {
        assert_eq!(parse_cpu_millis("500m"), Some(500));
        assert_eq!(parse_cpu_millis("2"), Some(2000));
        assert_eq!(parse_cpu_millis("0.25"), Some(250));
        assert_eq!(parse_cpu_millis("abc"), None);
        assert_eq!(parse_cpu_millis("0"), None);
    }

    #[test]
    fn nice_value_tracks_fractional_cpu() {
        let half = ContainerLimits {
            memory_bytes: None,
            cpu_millis: Some(500),
        };
        assert_eq!(half.nice_value(), 10);
        let one_and_half = ContainerLimits {
            memory_bytes: None,
            cpu_millis: Some(1500),
        };
        assert_eq!(one_and_half.nice_value(), 5);
        let two = ContainerLimits {
            memory_bytes: None,
            cpu_millis: Some(2000),
        };
        assert_eq!(two.nice_value(), 0);
        assert!(ContainerLimits::default().is_empty());
    }
}
