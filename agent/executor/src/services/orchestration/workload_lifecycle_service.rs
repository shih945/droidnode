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

/// One running container of a pod (B1: pods may have several).
pub struct RunningContainer {
    pub name: String,
    pub rootfs: PathBuf,
    /// RFC3339 timestamp of the *first* start of this container instance chain.
    pub started_at: String,
    /// Kubelet's `restartCount`, owned by the reconciler.
    pub restart_count: AtomicI32,
    /// Readiness (A9). Shared with the readiness probe task.
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
    /// (`pod.spec.terminationGracePeriodSeconds`, default 30) - B5.
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

/// How long a single init container may run before we kill it (B2).
const INIT_CONTAINER_TIMEOUT_SECS: u64 = 600;

/// Recursively copy `src` into `dst`, preserving symlinks and permissions.
///
/// Used to give every pod its own writable copy of the image rootfs (B17).
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

/// Translate a Kubernetes probe into our internal `ProbeConfig` (A9).
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
                    // A2: mirror the raw chunk before stripping the newline.
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
    /// Start **every** container of the pod (B1). `roots` maps container name to
    /// its unpacked rootfs (each image unpacks separately).
    async fn start(
        &self,
        pod: &Pod,
        roots: std::collections::HashMap<String, PathBuf>,
    ) -> Result<Arc<RunningWorkload>>;

    /// Graceful stop (B5): SIGTERM, wait `grace_seconds`, then SIGKILL.
    async fn stop(&self, workload: &RunningWorkload) -> Result<()>;

    /// Graceful stop with an explicit grace period. Used on deletion, where the
    /// API server tells us how much of the window is left via
    /// `metadata.deletionGracePeriodSeconds` (B6).
    async fn stop_with_grace(&self, workload: &RunningWorkload, grace: u64) -> Result<()>;

    /// Immediate stop: SIGKILL right away. Used when replacing a crashed
    /// container, where waiting out the grace period would only slow restarts.
    async fn stop_immediately(&self, workload: &RunningWorkload) -> Result<()>;

    async fn status(&self, workload: &RunningWorkload) -> Result<PodRunStatus>;

    /// Mirror a status computed by the reconciler (e.g. CrashLoopBackOff) into
    /// the local kubelet state so the on-device page and the control plane agree.
    async fn publish_status(&self, status: &PodRunStatus);
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

    /// Per-pod writable copy of an image rootfs (B17).
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

    /// Spawn the probe tasks declared by one container (A9).
    async fn spawn_probes(
        &self,
        pod: &Pod,
        container: &k8s_openapi::api::core::v1::Container,
        rootfs: &Path,
        pgid: i32,
        ready: Arc<AtomicBool>,
        alive: Arc<AtomicBool>,
    ) {
        let pod_name = pod.metadata.name.clone().unwrap_or_default();
        let container_name = container.name.clone();
        let ports: Vec<(String, u16)> = container
            .ports
            .clone()
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.container_port > 0)
            .map(|p| (p.name.unwrap_or_default(), p.container_port as u16))
            .collect();

        let startup = container
            .startup_probe
            .as_ref()
            .map(|p| to_probe_config(p, crate::models::ProbeType::Startup, &ports));
        let liveness = container
            .liveness_probe
            .as_ref()
            .map(|p| to_probe_config(p, crate::models::ProbeType::Liveness, &ports));
        let readiness = container
            .readiness_probe
            .as_ref()
            .map(|p| to_probe_config(p, crate::models::ProbeType::Readiness, &ports));

        if startup.is_none() && liveness.is_none() && readiness.is_none() {
            return;
        }

        let svc = Arc::clone(&self.probe_service);
        let rootfs = rootfs.to_path_buf();
        info!(
            pod = %pod_name,
            container = %container_name,
            liveness = liveness.is_some(),
            readiness = readiness.is_some(),
            startup = startup.is_some(),
            "health probes enabled"
        );

        tokio::spawn(async move {
            if let Some(cfg) = readiness {
                let svc = Arc::clone(&svc);
                let rootfs = rootfs.clone();
                let ready = Arc::clone(&ready);
                let alive = Arc::clone(&alive);
                let pod_name = pod_name.clone();
                let container_name = container_name.clone();
                tokio::spawn(async move {
                    run_probe_loop(
                        svc,
                        cfg,
                        rootfs,
                        pgid,
                        ready,
                        alive,
                        pod_name,
                        container_name,
                    )
                    .await
                });
            }

            if let Some(cfg) = startup {
                run_probe_loop(
                    Arc::clone(&svc),
                    cfg,
                    rootfs.clone(),
                    pgid,
                    Arc::clone(&ready),
                    Arc::clone(&alive),
                    pod_name.clone(),
                    container_name.clone(),
                )
                .await;
            }
            if alive.load(Ordering::Relaxed) {
                if let Some(cfg) = liveness {
                    run_probe_loop(
                        svc,
                        cfg,
                        rootfs,
                        pgid,
                        ready,
                        alive,
                        pod_name,
                        container_name,
                    )
                    .await;
                }
            }
        });
    }

    /// Stop one container: SIGTERM, wait up to `grace`, then SIGKILL (B5).
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

#[async_trait]
impl WorkloadLifecycleService for WorkloadLifecycleServiceImpl {
    #[instrument(skip(self, pod, roots), fields(
        pod = pod.metadata.name.as_deref().unwrap_or("?"),
        containers = roots.len()
    ))]
    async fn start(
        &self,
        pod: &Pod,
        roots: std::collections::HashMap<String, PathBuf>,
    ) -> Result<Arc<RunningWorkload>> {
        let pod_uid = pod.metadata.uid.clone().unwrap_or_default();
        let pod_name = pod.metadata.name.clone().unwrap_or_default();
        let namespace = pod
            .metadata
            .namespace
            .clone()
            .unwrap_or_else(|| "default".into());
        let spec = pod
            .spec
            .as_ref()
            .ok_or_else(|| crate::error::DroidError::Workload("pod has no spec".into()))?;
        let grace_seconds = spec
            .termination_grace_period_seconds
            .unwrap_or(DEFAULT_GRACE_SECONDS as i64)
            .max(0) as u64;

        let mut containers: Vec<Arc<RunningContainer>> = Vec::new();

        // B2: init containers run first, strictly in order, and must succeed.
        // A non-zero exit is a pod-level failure; the reconciler then applies the
        // restartPolicy/backoff just like for a crashed main container.
        if let Some(init_containers) = spec.init_containers.as_ref() {
            let total = init_containers.len();
            for (idx, init) in init_containers.iter().enumerate() {
                let Some(image_rootfs) = roots.get(&init.name).cloned() else {
                    warn!(pod = %pod_name, container = %init.name, "no rootfs for init container");
                    continue;
                };
                let rootfs = match self.pod_rootfs(&pod_uid, &init.name, &image_rootfs).await {
                    Ok(p) => p,
                    Err(_) => image_rootfs,
                };

                info!(
                    pod = %pod_name,
                    container = %init.name,
                    step = idx + 1,
                    total,
                    "running init container"
                );
                eprintln!(
                    "POD_EVENT init pod={} container={} step={}/{}",
                    pod_name,
                    init.name,
                    idx + 1,
                    total
                );

                let mut child = self
                    .execution_service
                    .execute_container(pod, init, &rootfs)
                    .await?;

                self.kubelet_state
                    .begin_log(&namespace, &pod_name, &init.name)
                    .await;
                if let Some(stdout) = child.stdout.take() {
                    spawn_log_drain(
                        stdout,
                        self.kubelet_state.clone(),
                        namespace.clone(),
                        pod_name.clone(),
                        init.name.clone(),
                        1,
                        None,
                    );
                }
                if let Some(stderr) = child.stderr.take() {
                    spawn_log_drain(
                        stderr,
                        self.kubelet_state.clone(),
                        namespace.clone(),
                        pod_name.clone(),
                        init.name.clone(),
                        2,
                        None,
                    );
                }

                let waited = tokio::time::timeout(
                    std::time::Duration::from_secs(INIT_CONTAINER_TIMEOUT_SECS),
                    child.wait(),
                )
                .await;

                match waited {
                    Ok(Ok(status)) if status.success() => {
                        info!(pod = %pod_name, container = %init.name, "init container completed");
                    }
                    Ok(Ok(status)) => {
                        return Err(crate::error::DroidError::Workload(format!(
                            "init container \"{}\" failed with exit code {:?}",
                            init.name,
                            status.code()
                        )));
                    }
                    Ok(Err(e)) => {
                        return Err(crate::error::DroidError::Workload(format!(
                            "init container \"{}\" could not be waited on: {e}",
                            init.name
                        )));
                    }
                    Err(_) => {
                        let _ = child.start_kill();
                        let _ = child.wait().await;
                        return Err(crate::error::DroidError::Workload(format!(
                            "init container \"{}\" timed out after {INIT_CONTAINER_TIMEOUT_SECS}s",
                            init.name
                        )));
                    }
                }
            }
        }

        for container in &spec.containers {
            let Some(image_rootfs) = roots.get(&container.name).cloned() else {
                warn!(pod = %pod_name, container = %container.name, "no rootfs prepared - skipping container");
                continue;
            };

            // B17: writable copy per (pod, container) instead of the shared image.
            let rootfs = match self
                .pod_rootfs(&pod_uid, &container.name, &image_rootfs)
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    warn!(pod = %pod_name, container = %container.name, error = %e, "could not create per-pod rootfs; falling back to the shared image");
                    image_rootfs.clone()
                }
            };

            let mut child = match self
                .execution_service
                .execute_container(pod, container, &rootfs)
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    // Roll back whatever already started so we do not leave a
                    // half-created pod behind.
                    for started in &containers {
                        let _ = self.stop_container(&pod_name, started, 0).await;
                    }
                    return Err(e);
                }
            };

            self.kubelet_state
                .begin_log(&namespace, &pod_name, &container.name)
                .await;
            // A2: keep stdin open and mirror output so `kubectl attach` can
            // join the running process.
            let (attach_in_tx, mut attach_in_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
            let (attach_out_tx, _) = tokio::sync::broadcast::channel::<AttachFrame>(256);
            if let Some(mut stdin) = child.stdin.take() {
                tokio::spawn(async move {
                    use tokio::io::AsyncWriteExt;
                    while let Some(data) = attach_in_rx.recv().await {
                        if stdin.write_all(&data).await.is_err() {
                            break;
                        }
                        let _ = stdin.flush().await;
                    }
                });
            }
            if let Some(stdout) = child.stdout.take() {
                spawn_log_drain(
                    stdout,
                    self.kubelet_state.clone(),
                    namespace.clone(),
                    pod_name.clone(),
                    container.name.clone(),
                    1,
                    Some(attach_out_tx.clone()),
                );
            }
            if let Some(stderr) = child.stderr.take() {
                spawn_log_drain(
                    stderr,
                    self.kubelet_state.clone(),
                    namespace.clone(),
                    pod_name.clone(),
                    container.name.clone(),
                    2,
                    Some(attach_out_tx.clone()),
                );
            }

            let started_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let ready = Arc::new(AtomicBool::new(true));
            let probe_alive = Arc::new(AtomicBool::new(true));
            let pgid = child.id().map(|p| p as i32);

            let handle = Arc::new(RunningContainer {
                name: container.name.clone(),
                rootfs: rootfs.clone(),
                started_at: started_at.clone(),
                restart_count: AtomicI32::new(0),
                ready: Arc::clone(&ready),
                probe_alive: Arc::clone(&probe_alive),
                child: Mutex::new(child),
            });

            if let Some(pgid) = pgid {
                self.spawn_probes(pod, container, &rootfs, pgid, ready, probe_alive)
                    .await;
                self.kubelet_state.mark_in_use(&rootfs).await;
                self.kubelet_state
                    .register_pod_proc(
                        &format!("{pod_uid}/{}", container.name),
                        &namespace,
                        &pod_name,
                        &container.name,
                        pgid,
                    )
                    .await;
                // A1: exec needs the same per-pod rootfs (and the fake-root
                // flag) as the running container.
                self.kubelet_state
                    .register_exec_target(&namespace, &pod_name, &container.name, rootfs.clone())
                    .await;
                self.kubelet_state
                    .register_attach_target(
                        &namespace,
                        &pod_name,
                        &container.name,
                        attach_in_tx.clone(),
                        attach_out_tx.clone(),
                    )
                    .await;
            }

            info!(pod = %pod_name, container = %container.name, "container started");
            containers.push(handle);
        }

        if containers.is_empty() {
            return Err(crate::error::DroidError::Workload(
                "no container could be started".into(),
            ));
        }

        eprintln!("POD_EVENT started pod={pod_name}");

        let workload = Arc::new(RunningWorkload {
            pod_uid: pod_uid.clone(),
            pod_name: pod_name.clone(),
            namespace: namespace.clone(),
            grace_seconds,
            containers,
        });

        // Publish immediately so the pod does not vanish from the on-device page
        // between start() and the next reconcile pass.
        let snapshot = PodRunStatus {
            pod_uid,
            pod_name,
            namespace,
            phase: PodPhase::Running,
            message: None,
            containers: workload
                .containers
                .iter()
                .map(|c| ContainerRunStatus {
                    name: c.name.clone(),
                    ready: true,
                    running: true,
                    exit_code: None,
                    restart_count: 0,
                    started_at: Some(c.started_at.clone()),
                    finished_at: None,
                    reason: None,
                    waiting_reason: None,
                })
                .collect(),
        };
        self.kubelet_state.update_status(snapshot).await;

        Ok(workload)
    }

    #[instrument(skip(self, workload), fields(pod = %workload.pod_name))]
    async fn stop(&self, workload: &RunningWorkload) -> Result<()> {
        self.stop_with_grace(workload, workload.grace_seconds).await
    }

    #[instrument(skip(self, workload), fields(pod = %workload.pod_name, grace))]
    async fn stop_with_grace(&self, workload: &RunningWorkload, grace: u64) -> Result<()> {
        for c in &workload.containers {
            self.stop_container(&workload.pod_name, c, grace).await;
            self.kubelet_state.unmark_in_use(&c.rootfs).await;
            self.kubelet_state
                .unregister_pod_proc(&format!("{}/{}", workload.pod_uid, c.name))
                .await;
        }
        self.kubelet_state.remove_status(&workload.pod_uid).await;
        self.kubelet_state
            .remove_exec_targets(&workload.namespace, &workload.pod_name)
            .await;
        self.kubelet_state
            .remove_attach_targets(&workload.namespace, &workload.pod_name)
            .await;
        info!(pod = %workload.pod_name, grace, "workload stopped");
        Ok(())
    }

    #[instrument(skip(self, workload), fields(pod = %workload.pod_name))]
    async fn stop_immediately(&self, workload: &RunningWorkload) -> Result<()> {
        for c in &workload.containers {
            self.stop_container(&workload.pod_name, c, 0).await;
            self.kubelet_state.unmark_in_use(&c.rootfs).await;
            self.kubelet_state
                .unregister_pod_proc(&format!("{}/{}", workload.pod_uid, c.name))
                .await;
        }
        self.kubelet_state.remove_status(&workload.pod_uid).await;
        self.kubelet_state
            .remove_exec_targets(&workload.namespace, &workload.pod_name)
            .await;
        self.kubelet_state
            .remove_attach_targets(&workload.namespace, &workload.pod_name)
            .await;
        Ok(())
    }

    #[instrument(skip(self, workload), fields(pod = %workload.pod_name))]
    async fn status(&self, workload: &RunningWorkload) -> Result<PodRunStatus> {
        let mut statuses = Vec::with_capacity(workload.containers.len());
        let mut any_running = false;
        let mut any_failed = false;
        let mut all_done = true;

        for c in &workload.containers {
            let mut child = c.child.lock().await;
            let (running, exit_code) = match child.try_wait() {
                Ok(Some(status)) => (false, status.code()),
                Ok(None) => (true, None),
                Err(e) => {
                    warn!(pod = %workload.pod_name, container = %c.name, error = %e, "could not check child status");
                    (false, None)
                }
            };
            if running {
                any_running = true;
                all_done = false;
            } else {
                if exit_code != Some(0) {
                    any_failed = true;
                }
            }

            let reason = if running {
                None
            } else if exit_code == Some(0) {
                Some("Completed".to_string())
            } else {
                Some("Error".to_string())
            };

            statuses.push(ContainerRunStatus {
                name: c.name.clone(),
                ready: running && c.is_ready(),
                running,
                exit_code,
                restart_count: c.restart_count.load(Ordering::Relaxed),
                started_at: Some(c.started_at.clone()),
                finished_at: if running {
                    None
                } else {
                    Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                },
                reason,
                waiting_reason: None,
            });
        }

        let phase = if any_failed {
            PodPhase::Failed
        } else if any_running {
            PodPhase::Running
        } else if all_done {
            PodPhase::Succeeded
        } else {
            PodPhase::Unknown
        };

        let snapshot = PodRunStatus {
            pod_uid: workload.pod_uid.clone(),
            pod_name: workload.pod_name.clone(),
            namespace: workload.namespace.clone(),
            phase,
            message: None,
            containers: statuses,
        };

        self.kubelet_state.update_status(snapshot.clone()).await;
        Ok(snapshot)
    }

    async fn publish_status(&self, status: &PodRunStatus) {
        self.kubelet_state.update_status(status.clone()).await;
    }
}
