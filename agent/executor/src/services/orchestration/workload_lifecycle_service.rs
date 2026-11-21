use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use tokio::process::Child;
use tokio::sync::Mutex;
use tracing::{info, instrument, warn};

use crate::error::Result;
use crate::exposers::virtual_kubelet_exposer::{AttachFrame, KubeletState};
use crate::models::{ContainerRunStatus, Pod, PodPhase, PodRunStatus, ProbeConfig};
use crate::services::foundation::{
    health_probe_service::HealthProbeService, workload_execution_service::WorkloadExecutionService,
};

// ─── Running workload handle ──────────────────────────────────────────────────

/// One running container of a pod; a pod may have several.
#[path = "workload_lifecycle/probes.rs"]
mod probes;
#[path = "workload_lifecycle/trait_impl.rs"]
mod trait_impl;

pub struct RunningContainer {
    pub name: String,
    pub rootfs: PathBuf,
    /// Shared, read-only image rootfs this container was created from. The
    /// per-pod rootfs is a throwaway copy, so image deletion must check this
    /// path to know whether the image is still needed.
    pub image_rootfs: PathBuf,
    /// RFC3339 timestamp of the *first* start of this container instance chain.
    pub started_at: String,
    /// Kubelet's `restartCount`, owned by the reconciler.
    pub restart_count: AtomicI32,
    /// Readiness. Shared with the readiness probe task.
    pub ready: Arc<AtomicBool>,
    /// False once the container stops, so probe loops exit.
    pub probe_alive: Arc<AtomicBool>,
    pub child: Mutex<Child>,
}

impl RunningContainer {
    pub fn pgid(&self) -> Option<i32> {
        // try_lock is enough: nobody holds it while the process is running.
        self.child
            .try_lock()
            .ok()
            .and_then(|c| c.id())
            .map(|p| p as i32)
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }
}

pub struct RunningWorkload {
    pub pod_uid: String,
    pub pod_name: String,
    pub namespace: String,
    /// Seconds to wait between SIGTERM and SIGKILL
    /// (`pod.spec.terminationGracePeriodSeconds`, default 30).
    pub grace_seconds: u64,
    pub containers: Vec<Arc<RunningContainer>>,
}

impl RunningWorkload {
    /// Kubelet's `restartCount` is a pod-level number in our model.
    pub fn restart_count(&self) -> i32 {
        self.containers
            .iter()
            .map(|c| c.restart_count.load(Ordering::Relaxed))
            .max()
            .unwrap_or(0)
    }

    pub fn container(&self, name: &str) -> Option<&Arc<RunningContainer>> {
        self.containers.iter().find(|c| c.name == name)
    }

    /// Ready only when every container is ready.
    pub fn all_ready(&self) -> bool {
        !self.containers.is_empty() && self.containers.iter().all(|c| c.is_ready())
    }
}

/// Cap a single log line so one noisy container cannot blow up the buffer.
const MAX_LINE_BYTES: usize = 64 * 1024;

/// Default grace period when the pod does not set one.
const DEFAULT_GRACE_SECONDS: u64 = 30;

/// How long a single init container may run before we kill it.
const INIT_CONTAINER_TIMEOUT_SECS: u64 = 600;

/// Recursively copy `src` into `dst`, preserving symlinks and permissions.
///
/// Used to give every pod its own writable copy of the image rootfs.
/// Symlinks are recreated as links, never followed, so a rootfs that links to
/// an absolute path cannot escape the copy.
fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ft.is_symlink() {
            #[cfg(unix)]
            {
                let target = std::fs::read_link(&from)?;
                let _ = std::fs::remove_file(&to);
                std::os::unix::fs::symlink(target, &to)?;
            }
        } else if ft.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Translate a Kubernetes probe into our internal `ProbeConfig`.
fn to_probe_config(
    probe: &k8s_openapi::api::core::v1::Probe,
    probe_type: crate::models::ProbeType,
    container_ports: &[(String, u16)],
) -> ProbeConfig {
    let resolve_port = |p: &k8s_openapi::apimachinery::pkg::util::intstr::IntOrString| -> u16 {
        match p {
            k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(n) => {
                if *n > 0 && *n <= u16::MAX as i32 {
                    *n as u16
                } else {
                    0
                }
            }
            k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::String(name) => {
                container_ports
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, p)| *p)
                    .unwrap_or(0)
            }
        }
    };

    let http_get = probe.http_get.as_ref().map(|h| crate::models::HttpProbe {
        host: h.host.clone().unwrap_or_else(|| "127.0.0.1".to_string()),
        port: resolve_port(&h.port),
        path: h.path.clone().unwrap_or_else(|| "/".to_string()),
    });
    let tcp_socket = probe.tcp_socket.as_ref().map(|t| {
        (
            t.host.clone().unwrap_or_else(|| "127.0.0.1".to_string()),
            resolve_port(&t.port),
        )
    });
    let exec_command = probe
        .exec
        .as_ref()
        .and_then(|e| e.command.clone())
        .filter(|c| !c.is_empty());

    ProbeConfig {
        probe_type,
        exec_command,
        http_get,
        tcp_socket,
        initial_delay_seconds: probe.initial_delay_seconds.unwrap_or(0).max(0) as u32,
        period_seconds: probe.period_seconds.unwrap_or(10).max(1) as u32,
        failure_threshold: probe.failure_threshold.unwrap_or(3).max(1) as u32,
        success_threshold: probe.success_threshold.unwrap_or(1).max(1) as u32,
        timeout_seconds: probe.timeout_seconds.unwrap_or(1).max(1) as u32,
    }
}

/// Repeatedly run one probe, flipping `ready` (readiness) or killing the
/// container's process group once `failure_threshold` is reached (liveness).
#[allow(clippy::too_many_arguments)]
async fn run_probe_loop(
    svc: Arc<dyn HealthProbeService>,
    cfg: ProbeConfig,
    rootfs: PathBuf,
    pgid: i32,
    ready: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    pod_name: String,
    container: String,
) {
    use crate::models::ProbeType;
    use std::time::Duration;

    if cfg.initial_delay_seconds > 0 {
        tokio::time::sleep(Duration::from_secs(cfg.initial_delay_seconds as u64)).await;
    }

    let mut failures = 0u32;
    let mut successes = 0u32;
    while alive.load(Ordering::Relaxed) {
        let ok = svc.run_probe(&cfg, &rootfs, None).await.is_ok();
        match cfg.probe_type {
            ProbeType::Readiness => {
                if ok {
                    successes += 1;
                    if successes >= cfg.success_threshold && !ready.swap(true, Ordering::Relaxed) {
                        info!(pod = %pod_name, container = %container, "readiness probe passed");
                    }
                    failures = 0;
                } else {
                    successes = 0;
                    failures += 1;
                    if failures >= cfg.failure_threshold && ready.swap(false, Ordering::Relaxed) {
                        warn!(pod = %pod_name, container = %container, "readiness probe failed - pod becomes NotReady");
                    }
                }
            }
            ProbeType::Liveness | ProbeType::Startup => {
                if ok {
                    failures = 0;
                    if matches!(cfg.probe_type, ProbeType::Startup) {
                        info!(pod = %pod_name, container = %container, "startup probe passed");
                        return;
                    }
                } else {
                    failures += 1;
                    if failures >= cfg.failure_threshold {
                        warn!(
                            pod = %pod_name,
                            container = %container,
                            failures,
                            "liveness probe failed - killing container so it restarts"
                        );
                        #[cfg(unix)]
                        unsafe {
                            libc::kill(-pgid, libc::SIGKILL);
                            libc::kill(pgid, libc::SIGKILL);
                        }
                        return;
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(cfg.period_seconds as u64)).await;
    }
}

/// Drain a container stream into the kubelet log store.
///
/// Reads raw bytes and decodes with `from_utf8_lossy` instead of using
/// `BufRead::lines()`: `lines()` requires valid UTF-8 and would silently drop
/// the whole line (and every following one) as soon as a container emits a
/// non-UTF-8 byte - gbk output, progress bars or binary junk.
fn spawn_log_drain<R>(
    reader: R,
    state: KubeletState,
    namespace: String,
    pod: String,
    container: String,
    channel: u8,
    attach: Option<tokio::sync::broadcast::Sender<AttachFrame>>,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    use tokio::io::{AsyncBufReadExt, BufReader};

    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        let mut buf: Vec<u8> = Vec::with_capacity(256);
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) => break, // EOF
                Ok(_) => {
                    // Mirror the raw chunk before stripping the newline.
                    if let Some(tx) = &attach {
                        let _ = tx.send(AttachFrame {
                            channel,
                            data: buf.clone(),
                        });
                    }
                    while matches!(buf.last(), Some(b'\n') | Some(b'\r')) {
                        buf.pop();
                    }
                    if buf.len() > MAX_LINE_BYTES {
                        buf.truncate(MAX_LINE_BYTES);
                    }
                    let line = String::from_utf8_lossy(&buf).into_owned();
                    state.append_log(&namespace, &pod, &container, line).await;
                }
                Err(_) => break,
            }
        }
        // Let any open `kubectl logs -f` / dashboard stream finish instead of
        // hanging on a container that no longer exists.
        state.finish_log(&namespace, &pod, &container).await;
    });
}

// ─── Trait ────────────────────────────────────────────────────────────────────

#[async_trait]
pub trait WorkloadLifecycleService: Send + Sync {
    /// Start **every** container of the pod. `roots` maps container name to
    /// its unpacked rootfs (each image unpacks separately).
    async fn start(
        &self,
        pod: &Pod,
        roots: std::collections::HashMap<String, PathBuf>,
    ) -> Result<Arc<RunningWorkload>>;

    /// Graceful stop: SIGTERM, wait `grace_seconds`, then SIGKILL.
    async fn stop(&self, workload: &RunningWorkload) -> Result<()>;

    /// Graceful stop with an explicit grace period. Used on deletion, where the
    /// API server tells us how much of the window is left via
    /// `metadata.deletionGracePeriodSeconds`.
    async fn stop_with_grace(&self, workload: &RunningWorkload, grace: u64) -> Result<()>;

    /// Immediate stop: SIGKILL right away. Used when replacing a crashed
    /// container, where waiting out the grace period would only slow restarts.
    async fn stop_immediately(&self, workload: &RunningWorkload) -> Result<()>;

    async fn status(&self, workload: &RunningWorkload) -> Result<PodRunStatus>;

    /// Mirror a status computed by the reconciler (e.g. CrashLoopBackOff) into
    /// the local kubelet state so the on-device page and the control plane agree.
    async fn publish_status(&self, status: &PodRunStatus);

    /// Drop a pod from the local kubelet cache. Used when a pod is deleted
    /// before it ever produced a running handle (e.g. a stuck image pull),
    /// otherwise the on-device page keeps showing it forever.
    async fn forget_status(&self, pod_uid: &str);

    /// Cache the Pod spec so `/pods` can return containers/images like kubelet.
    async fn store_pod(&self, pod: &Pod);

    /// Drop the cached Pod spec when the pod is gone.
    async fn forget_pod(&self, pod_uid: &str);
}

// ─── Implementation ───────────────────────────────────────────────────────────

pub struct WorkloadLifecycleServiceImpl {
    execution_service: Arc<dyn WorkloadExecutionService>,
    probe_service: Arc<dyn HealthProbeService>,
    kubelet_state: KubeletState,
}

impl WorkloadLifecycleServiceImpl {
    pub fn new(
        execution_service: Arc<dyn WorkloadExecutionService>,
        probe_service: Arc<dyn HealthProbeService>,
        kubelet_state: KubeletState,
    ) -> Self {
        Self {
            execution_service,
            probe_service,
            kubelet_state,
        }
    }

    /// Per-pod writable copy of an image rootfs.
    ///
    /// Every pod used to run directly on the shared image rootfs, so two pods of
    /// the same image wrote into the same directory. Now each (pod, container)
    /// gets its own copy under `<files>/pods/<uid>/<container>/rootfs`, created on
    /// first use.
    async fn pod_rootfs(
        &self,
        pod_uid: &str,
        container: &str,
        image_rootfs: &Path,
    ) -> Result<PathBuf> {
        let base = image_rootfs
            .parent()
            .and_then(|_| {
                // image rootfs is <files>/rootfs/<registry>/<repo>/<tag>
                image_rootfs
                    .ancestors()
                    .nth(4)
                    .map(|files| files.join("pods"))
            })
            .unwrap_or_else(|| image_rootfs.join("pods"));
        let target = base.join(pod_uid).join(container).join("rootfs");

        if target.join(".droidnode_pod_rootfs_ready").exists() {
            return Ok(target);
        }

        let src = image_rootfs.to_path_buf();
        let dst = target.clone();
        let copied = tokio::task::spawn_blocking(move || {
            let _ = std::fs::remove_dir_all(&dst);
            copy_tree(&src, &dst)
        })
        .await;

        match copied {
            Ok(Ok(())) => {
                let _ = tokio::fs::write(target.join(".droidnode_pod_rootfs_ready"), b"1").await;
                info!(rootfs = %target.display(), "created per-pod writable rootfs");
                Ok(target)
            }
            Ok(Err(e)) => Err(crate::error::DroidError::Filesystem(e)),
            Err(e) => Err(crate::error::DroidError::Process(format!(
                "rootfs copy task failed: {e}"
            ))),
        }
    }

    /// Stop one container: SIGTERM, wait up to `grace`, then SIGKILL.
    async fn stop_container(&self, pod_name: &str, c: &RunningContainer, grace: u64) {
        c.probe_alive.store(false, Ordering::Relaxed);
        c.ready.store(false, Ordering::Relaxed);

        let mut child = c.child.lock().await;
        let Some(pid) = child.id() else {
            let _ = child.wait().await;
            return;
        };
        let pid = pid as i32;

        #[cfg(unix)]
        {
            // Terminate the whole process group so proot's children see it too.
            unsafe {
                libc::kill(-pid, libc::SIGTERM);
                libc::kill(pid, libc::SIGTERM);
            }
            if grace > 0 {
                info!(pod = %pod_name, container = %c.name, grace, "sent SIGTERM, waiting for graceful stop");
                let waited =
                    tokio::time::timeout(std::time::Duration::from_secs(grace), child.wait()).await;
                if waited.is_ok() {
                    info!(pod = %pod_name, container = %c.name, "container exited gracefully");
                    return;
                }
                warn!(pod = %pod_name, container = %c.name, grace, "grace period expired - sending SIGKILL");
            }
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
                libc::kill(pid, libc::SIGKILL);
            }
        }

        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}
