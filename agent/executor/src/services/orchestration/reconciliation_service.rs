use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{debug, error, info, instrument, warn};

use crate::brokers::{ControlPlaneBroker, PodWatchEvent};
use crate::error::Result;
use crate::models::{ContainerRunStatus, Pod, PodPhase, PodRunStatus};
use crate::services::foundation::event_recording_service::EventRecordingService;
use crate::services::orchestration::{
    image_orchestration_service::ImageOrchestrationService,
    workload_lifecycle_service::{RunningWorkload, WorkloadLifecycleService},
};

const RECONCILE_INTERVAL_SECS: u64 = 15;
/// A18: even with a live watch, do a full LIST this often as a backstop.
const FULL_RESYNC_SECS: u64 = 60;
/// kubelet-style exponential backoff: 10s, 20s, 40s ... capped at 5 min.
const BACKOFF_BASE_SECS: u64 = 10;
const BACKOFF_MAX_SECS: u64 = 300;

fn backoff_delay(attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(6);
    let secs = (BACKOFF_BASE_SECS << shift).min(BACKOFF_MAX_SECS);
    Duration::from_secs(secs)
}

#[derive(Default)]
struct RestartState {
    attempts: u32,
    next_at: Option<Instant>,
}

/// Start-failure bookkeeping (B21): back off instead of retrying every 15 s and
/// flooding the event stream.
#[derive(Default)]
struct StartFailure {
    attempts: u32,
    next_at: Option<Instant>,
    message: String,
}

/// Why a declared container port cannot be used.
enum PortProblem {
    /// Already owned by another pod or a host process.
    InUse(u16, String),
    /// <1024: the agent runs as an Android app uid, so bind() gets EACCES.
    Privileged(u16),
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

#[async_trait]
pub trait ReconciliationService: Send + Sync {
    /// Run the reconciliation loop forever. Call this on a dedicated task.
    async fn run_loop(&self) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

pub struct ReconciliationServiceImpl {
    cp_broker: Arc<dyn ControlPlaneBroker>,
    image_orch: Arc<dyn ImageOrchestrationService>,
    workload_lifecycle: Arc<dyn WorkloadLifecycleService>,
    event_service: Arc<dyn EventRecordingService>,
    node_name: String,
    /// pod_uid -> running workload
    running: Mutex<HashMap<String, Arc<RunningWorkload>>>,
    /// pod_uid -> restart backoff state
    backoff: Mutex<HashMap<String, RestartState>>,
    /// container port -> pod name. Every container shares the device's network
    /// namespace, so a port can only be claimed by one pod at a time (B23).
    ports: Mutex<HashMap<u16, String>>,
    /// image reference -> lock. Two pods asking for the same image must not
    /// unpack into the same rootfs concurrently; different images still pull in
    /// parallel (which is what splitting the reconcile lock buys us, B20).
    image_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// pod_uid -> start-failure backoff (B21)
    start_failures: Mutex<HashMap<String, StartFailure>>,
    /// "<uid>/<reason>" -> (last message, when) so identical events are not
    /// re-emitted every reconcile pass.
    last_events: Mutex<HashMap<String, (String, Instant)>>,
    /// E7: while set, running pods are stopped gracefully and no new pod starts
    /// (low battery). The agent process itself keeps running.
    drained: Arc<std::sync::atomic::AtomicBool>,
    /// A18: Pods maintained incrementally by the watch task (UID -> Pod).
    pod_cache: Arc<Mutex<HashMap<String, Pod>>>,
    /// True once the watcher delivered its first full snapshot. Until then the
    /// reconcile loop falls back to a 15 s LIST.
    watch_ready: Arc<AtomicBool>,
    /// Last successful full LIST (watch backstop + drift correction).
    last_full_list: Mutex<Instant>,
}

impl ReconciliationServiceImpl {
    pub fn new(
        cp_broker: Arc<dyn ControlPlaneBroker>,
        image_orch: Arc<dyn ImageOrchestrationService>,
        workload_lifecycle: Arc<dyn WorkloadLifecycleService>,
        event_service: Arc<dyn EventRecordingService>,
        node_name: String,
        drained: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            cp_broker,
            image_orch,
            workload_lifecycle,
            event_service,
            node_name,
            running: Mutex::new(HashMap::new()),
            backoff: Mutex::new(HashMap::new()),
            ports: Mutex::new(HashMap::new()),
            image_locks: Mutex::new(HashMap::new()),
            start_failures: Mutex::new(HashMap::new()),
            last_events: Mutex::new(HashMap::new()),
            drained,
            pod_cache: Arc::new(Mutex::new(HashMap::new())),
            watch_ready: Arc::new(AtomicBool::new(false)),
            last_full_list: Mutex::new(Instant::now()),
        }
    }

    /// Emit an event, but not the same message twice within a couple of minutes.
    async fn emit_event(&self, pod: &Pod, reason: &str, message: &str, warning: bool) {
        let uid = pod.metadata.uid.clone().unwrap_or_default();
        let key = format!("{uid}/{reason}");
        {
            let mut seen = self.last_events.lock().await;
            if let Some((last_message, at)) = seen.get(&key) {
                if last_message == message && at.elapsed() < Duration::from_secs(120) {
                    return;
                }
            }
            seen.insert(key, (message.to_string(), Instant::now()));
        }
        let pod_ref = Some(crate::models::PodRef::from_pod(pod));
        let _ = if warning {
            self.event_service.record_warning(reason, message, pod_ref).await
        } else {
            self.event_service.record_normal(reason, message, pod_ref).await
        };
    }

    /// Report a non-terminal "waiting" state (ContainerCreating / *ImagePull*).
    async fn report_waiting(&self, pod: &Pod, uid: &str, reason: &str, message: &str) {
        let status = Self::status_with_waiting(pod, uid, reason, message);
        let namespace = status.namespace.clone();
        let pod_name = status.pod_name.clone();
        let _ = self
            .cp_broker
            .report_pod_status(&pod_name, &namespace, &status)
            .await;
        self.workload_lifecycle.publish_status(&status).await;
    }

    async fn image_lock(&self, image: &str) -> Arc<Mutex<()>> {
        let mut locks = self.image_locks.lock().await;
        locks
            .entry(image.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Container ports declared by the pod's containers.
    fn declared_ports(pod: &Pod) -> Vec<u16> {
        pod.spec
            .as_ref()
            .map(|spec| {
                spec.containers
                    .iter()
                    .flat_map(|c| c.ports.clone().unwrap_or_default())
                    .filter(|p| p.container_port > 0)
                    .map(|p| p.container_port as u16)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Returns (port, owner) when `ports` cannot be bound by this pod.
    ///
    /// Two checks: (1) the port is already claimed by another pod we started,
    /// (2) an actual bind() on the wildcard address fails, which catches ports
    /// held by the Android system or by a leftover process from a previous run.
    async fn port_conflict(&self, pod_name: &str, ports: &[u16]) -> Option<PortProblem> {
        if ports.is_empty() {
            return None;
        }
        let claims = self.ports.lock().await;
        for port in ports {
            if let Some(owner) = claims.get(port) {
                if owner != pod_name {
                    return Some(PortProblem::InUse(*port, owner.clone()));
                }
            }
        }
        for port in ports {
            if claims.contains_key(port) {
                continue; // ours already
            }
            match std::net::TcpListener::bind(("0.0.0.0", *port)) {
                Ok(listener) => drop(listener),
                // EACCES on <1024 is not a conflict: report it accurately.
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    return Some(PortProblem::Privileged(*port));
                }
                Err(_) => {
                    return Some(PortProblem::InUse(*port, "another process".to_string()))
                }
            }
        }
        None
    }

    async fn claim_ports(&self, pod_name: &str, ports: &[u16]) {
        if ports.is_empty() {
            return;
        }
        let mut claims = self.ports.lock().await;
        for port in ports {
            claims.insert(*port, pod_name.to_string());
        }
    }

    async fn release_ports(&self, pod_name: &str) {
        let mut claims = self.ports.lock().await;
        claims.retain(|_, owner| owner != pod_name);
    }

    fn status_with_waiting(pod: &Pod, uid: &str, reason: &str, message: &str) -> PodRunStatus {
        let container_name = pod
            .spec
            .as_ref()
            .and_then(|s| s.containers.first())
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "main".to_string());
        PodRunStatus {
            pod_uid: uid.to_string(),
            pod_name: pod.metadata.name.clone().unwrap_or_default(),
            namespace: pod
                .metadata
                .namespace
                .clone()
                .unwrap_or_else(|| "default".to_string()),
            phase: PodPhase::Running,
            message: Some(message.to_string()),
            containers: vec![ContainerRunStatus {
                name: container_name,
                ready: false,
                running: false,
                exit_code: None,
                restart_count: 0,
                started_at: None,
                finished_at: None,
                reason: None,
                waiting_reason: Some(reason.to_string()),
            }],
        }
    }

    /// Start (or restart) a pod. Returns true when a handle was stored.
    ///
    /// This is the slow path (image pull + unpack + proot spawn). It is called
    /// WITHOUT holding the `running` lock, so one slow pull no longer blocks
    /// every other pod on the node (B20).
    #[instrument(skip(self, pod), fields(pod = %pod.metadata.name.as_deref().unwrap_or("?")))]
    async fn start_pod(&self, pod: &Pod, uid: &str, previous_restarts: i32) -> bool {
        let pod_name = pod.metadata.name.clone().unwrap_or_default();
        // B1: a pod may have several containers, each with its own image/rootfs.
        // B2: init containers are prepared here too - they run before the main
        // containers and may use a completely different image.
        let containers: Vec<(String, String)> = pod
            .spec
            .as_ref()
            .map(|s| {
                s.init_containers
                    .iter()
                    .flatten()
                    .chain(s.containers.iter())
                    .map(|c| (c.name.clone(), c.image.clone().unwrap_or_default()))
                    .collect()
            })
            .unwrap_or_default();

        if containers.is_empty() || containers.iter().any(|(_, i)| i.is_empty()) {
            warn!(pod = %pod_name, "pod has no image; skipping");
            return false;
        }
        let images_label = containers
            .iter()
            .map(|(_, i)| i.as_str())
            .collect::<Vec<_>>()
            .join(", ");

        // C5: fetch the docker config payloads referenced by imagePullSecrets
        // once per pod. A missing/broken secret is a warning (like kubelet's
        // FailedToRetrieveImagePullSecret); anonymous pull may still work.
        let namespace = pod
            .metadata
            .namespace
            .clone()
            .unwrap_or_else(|| "default".into());
        let mut pull_secret_payloads: Vec<Vec<u8>> = Vec::new();
        if let Some(spec) = pod.spec.as_ref() {
            for secret_ref in spec.image_pull_secrets.iter().flatten() {
                let secret_name = secret_ref.name.clone().unwrap_or_default();
                if secret_name.is_empty() {
                    continue;
                }
                match self.cp_broker.get_secret(&namespace, &secret_name).await {
                    Ok(data) => {
                        if let Some(payload) = data
                            .get(".dockerconfigjson")
                            .or_else(|| data.get(".dockercfg"))
                        {
                            pull_secret_payloads.push(payload.clone());
                        } else {
                            warn!(
                                pod = %pod_name,
                                secret = %secret_name,
                                "imagePullSecret has no .dockerconfigjson/.dockercfg entry"
                            );
                        }
                    }
                    Err(e) => {
                        warn!(
                            pod = %pod_name,
                            secret = %secret_name,
                            error = %e,
                            "could not read imagePullSecret"
                        );
                        self.emit_event(
                            pod,
                            "FailedToRetrieveImagePullSecret",
                            &format!(
                                "Unable to retrieve imagePullSecret \"{secret_name}\": {e}"
                            ),
                            true,
                        )
                        .await;
                    }
                }
            }
        }

        // Port conflict (B23): never start something that cannot bind.
        let ports = Self::declared_ports(pod);
        if let Some(problem) = self.port_conflict(&pod_name, &ports).await {
            let (reason, msg) = match problem {
                PortProblem::InUse(port, owner) => (
                    "PortConflict",
                    format!("port {port} is already in use by {owner}"),
                ),
                PortProblem::Privileged(port) => (
                    "PrivilegedPort",
                    format!(
                        "containerPort {port} is below 1024; the non-root agent cannot bind it, use a port >= 1024"
                    ),
                ),
            };
            warn!(pod = %pod_name, reason, message = %msg, "container port unusable - not starting");
            eprintln!("POD_EVENT {} pod={} message={}", reason.to_lowercase(), pod_name, msg);
            self.emit_event(pod, reason, &msg, true).await;
            self.report_waiting(pod, uid, reason, &msg).await;
            return false;
        }

        // B21: if the last attempt failed, wait out the backoff window instead of
        // hammering the registry every 15 s (and spamming events).
        {
            let failures = self.start_failures.lock().await;
            if let Some(st) = failures.get(uid) {
                if st.next_at.is_some_and(|t| Instant::now() < t) {
                    let msg = if st.message.is_empty() {
                        "back-off pulling image".to_string()
                    } else {
                        st.message.clone()
                    };
                    drop(failures);
                    self.report_waiting(pod, uid, "ImagePullBackOff", &msg).await;
                    return false;
                }
            }
        }

        // A20: show ContainerCreating while the images are being pulled, the way
        // a real kubelet does, instead of leaving the pod in Pending with no status.
        self.report_waiting(
            pod,
            uid,
            "ContainerCreating",
            &format!("pulling image(s) {images_label}"),
        )
        .await;
        self.emit_event(pod, "Pulling", &format!("Pulling image(s) {images_label}"), false)
            .await;

        // Prepare every container's image; any failure fails the whole start.
        let pull_started = Instant::now();
        let mut roots: std::collections::HashMap<String, std::path::PathBuf> =
            std::collections::HashMap::new();
        for (container_name, image_name) in &containers {
            let image_ref = match crate::models::ImageRef::parse(image_name) {
                Ok(r) => r,
                Err(e) => {
                    error!(pod = %pod_name, container = %container_name, error = %e, "invalid image ref");
                    self.emit_event(
                        pod,
                        "Failed",
                        &format!("Invalid image reference \"{image_name}\": {e}"),
                        true,
                    )
                    .await;
                    return false;
                }
            };

            info!(pod = %pod_name, container = %container_name, image = %image_name, "preparing image");
            // C5: resolve credentials against the image's upstream registry
            // (the pull itself may go through a mirror host).
            let auth = crate::services::foundation::pull_secret_service::resolve(
                &pull_secret_payloads,
                image_name,
            );
            if auth.is_some() {
                info!(
                    pod = %pod_name,
                    container = %container_name,
                    image = %image_name,
                    "using imagePullSecret credentials"
                );
            }
            // Serialise per image, not per node.
            let policy = crate::models::PullPolicy::resolve(
                pod.spec
                    .as_ref()
                    .and_then(|s| s.containers.iter().find(|c| &c.name == container_name))
                    .and_then(|c| c.image_pull_policy.as_deref()),
                &image_ref.reference,
            );
            let image_guard = self.image_lock(image_name).await;
            let prepared = {
                let _held = image_guard.lock().await;
                self.image_orch.prepare_image(&image_ref, policy, auth.as_ref()).await
            };

            match prepared {
                Ok(p) => {
                    roots.insert(container_name.clone(), p);
                }
                Err(e) => {
                    error!(pod = %pod_name, container = %container_name, error = %e, "image preparation failed");
                    self.emit_event(
                        pod,
                        "Failed",
                        &format!("Failed to pull image \"{image_name}\": {e}"),
                        true,
                    )
                    .await;
                    self.report_waiting(
                        pod,
                        uid,
                        "ErrImagePull",
                        &format!("image pull failed: {e}"),
                    )
                    .await;
                    // B21: exponential backoff for the next attempt.
                    let delay = {
                        let mut failures = self.start_failures.lock().await;
                        let st = failures.entry(uid.to_string()).or_default();
                        st.attempts += 1;
                        st.message = format!("back-off pulling image \"{image_name}\": {e}");
                        let delay = backoff_delay(st.attempts);
                        st.next_at = Some(Instant::now() + delay);
                        delay
                    };
                    info!(pod = %pod_name, retry_in_secs = delay.as_secs(), "image pull will be retried");
                    return false;
                }
            }
        }

        self.emit_event(
            pod,
            "Pulled",
            &format!(
                "Successfully pulled image(s) {images_label} in {:.1}s",
                pull_started.elapsed().as_secs_f64()
            ),
            false,
        )
        .await;

        match self.workload_lifecycle.start(pod, roots).await {
            Ok(w) => {
                if previous_restarts > 0 {
                    for c in &w.containers {
                        c.restart_count
                            .store(previous_restarts, Ordering::Relaxed);
                    }
                }
                self.start_failures.lock().await.remove(uid);
                self.emit_event(pod, "Started", "Started container(s)", false)
                    .await;
                self.running.lock().await.insert(uid.to_string(), w);
                self.claim_ports(&pod_name, &ports).await;
                true
            }
            Err(e) => {
                error!(pod = %pod_name, error = %e, "workload start failed");
                self.emit_event(pod, "Failed", &format!("Failed to start container: {e}"), true)
                    .await;

                // B2: with restartPolicy=Never a failed init container is final.
                // Leaving the pod in ContainerCreating while we retry forever is
                // exactly what confuses people debugging a broken init step.
                let policy = pod
                    .spec
                    .as_ref()
                    .and_then(|s| s.restart_policy.clone())
                    .unwrap_or_else(|| "Always".to_string());
                if policy == "Never" {
                    let status = crate::models::PodRunStatus {
                        pod_uid: uid.to_string(),
                        pod_name: pod_name.clone(),
                        namespace: pod
                            .metadata
                            .namespace
                            .clone()
                            .unwrap_or_else(|| "default".into()),
                        phase: crate::models::PodPhase::Failed,
                        message: Some(e.to_string()),
                        containers: pod
                            .spec
                            .as_ref()
                            .map(|s| {
                                s.init_containers
                                    .iter()
                                    .flatten()
                                    .map(|c| crate::models::ContainerRunStatus {
                                        name: c.name.clone(),
                                        ready: false,
                                        running: false,
                                        exit_code: Some(1),
                                        restart_count: 0,
                                        started_at: None,
                                        finished_at: Some(
                                            chrono::Utc::now().to_rfc3339_opts(
                                                chrono::SecondsFormat::Secs,
                                                true,
                                            ),
                                        ),
                                        reason: Some("InitContainerFailed".to_string()),
                                        waiting_reason: None,
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    };
                    let _ = self
                        .cp_broker
                        .report_pod_status(&pod_name, &status.namespace, &status)
                        .await;
                    self.workload_lifecycle.publish_status(&status).await;
                }
                false
            }
        }
    }

    /// Copy a terminal pod's status from the API object into the local kubelet
    /// state, so the on-device page shows exactly what `kubectl get pods` shows
    /// even after the agent restarted (the in-memory status is gone by then).
    async fn mirror_terminal_status(&self, pod: &Pod) {
        let Some(uid) = pod.metadata.uid.clone() else {
            return;
        };
        let Some(st) = pod.status.as_ref() else {
            return;
        };
        let phase = match st.phase.as_deref() {
            Some("Succeeded") => PodPhase::Succeeded,
            Some("Failed") => PodPhase::Failed,
            _ => PodPhase::Unknown,
        };
        let containers = st
            .container_statuses
            .as_ref()
            .map(|cs| {
                cs.iter()
                    .map(|c| {
                        let term = c.state.as_ref().and_then(|s| s.terminated.as_ref());
                        ContainerRunStatus {
                            name: c.name.clone(),
                            ready: c.ready,
                            running: false,
                            exit_code: term.map(|t| t.exit_code),
                            restart_count: c.restart_count,
                            started_at: term
                                .and_then(|t| t.started_at.as_ref())
                                .map(|t| t.0.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                            finished_at: term
                                .and_then(|t| t.finished_at.as_ref())
                                .map(|t| t.0.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                            reason: term.and_then(|t| t.reason.clone()),
                            waiting_reason: None,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        self.workload_lifecycle
            .publish_status(&PodRunStatus {
                pod_uid: uid,
                pod_name: pod.metadata.name.clone().unwrap_or_default(),
                namespace: pod
                    .metadata
                    .namespace
                    .clone()
                    .unwrap_or_else(|| "default".to_string()),
                phase,
                message: None,
                containers,
            })
            .await;
    }

    /// Remove a handle from the map so subsequent phases see it as gone.
    async fn take_handle(&self, uid: &str) -> Option<Arc<RunningWorkload>> {
        self.running.lock().await.remove(uid)
    }

    /// A18: keep a Pod cache in sync from a watch stream.
    ///
    /// The stream is restarted on error/EOF. `Restarted` is the authoritative
    /// snapshot; while the watch is down, `reconcile_once` falls back to LIST.
    fn start_pod_watch(&self) {
        let broker = Arc::clone(&self.cp_broker);
        let cache = Arc::clone(&self.pod_cache);
        let ready = Arc::clone(&self.watch_ready);
        tokio::spawn(async move {
            loop {
                let (tx, mut rx) = tokio::sync::mpsc::channel(128);
                let watcher_broker = Arc::clone(&broker);
                let handle = tokio::spawn(async move {
                    if let Err(e) = watcher_broker.watch_assigned_pods(tx).await {
                        warn!(error = %e, "pod watch stream ended; reconnecting");
                    }
                });

                while let Some(event) = rx.recv().await {
                    let mut map = cache.lock().await;
                    match event {
                        PodWatchEvent::Applied(pod) => {
                            if let Some(uid) = pod.metadata.uid.clone() {
                                map.insert(uid, pod);
                            }
                        }
                        PodWatchEvent::Deleted(uid) => {
                            map.remove(&uid);
                        }
                        PodWatchEvent::Restarted(pods) => {
                            map.clear();
                            for pod in pods {
                                if let Some(uid) = pod.metadata.uid.clone() {
                                    map.insert(uid, pod);
                                }
                            }
                            ready.store(true, Ordering::Relaxed);
                            info!(pods = map.len(), "pod watch ready (full snapshot)");
                        }
                    }
                }

                ready.store(false, Ordering::Relaxed);
                let _ = handle.await;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
    }

    /// A18: desired pods from the watch cache, with a periodic full LIST as the
    /// drift/backstop path. Before the first snapshot we keep the old 15 s LIST.
    async fn desired_pods(&self) -> Result<Vec<Pod>> {
        let watch_ready = self.watch_ready.load(Ordering::Relaxed);
        let stale = self
            .last_full_list
            .lock()
            .await
            .elapsed()
            >= Duration::from_secs(FULL_RESYNC_SECS);

        if watch_ready && !stale {
            let pods: Vec<Pod> = self.pod_cache.lock().await.values().cloned().collect();
            debug!(pods = pods.len(), "desired pods from watch cache");
            return Ok(pods);
        }

        let pods = self.cp_broker.get_assigned_pods().await?;
        info!(pods = pods.len(), "pod cache full resync (LIST)");
        *self.last_full_list.lock().await = Instant::now();
        if watch_ready {
            let mut cache = self.pod_cache.lock().await;
            cache.clear();
            for pod in &pods {
                if let Some(uid) = pod.metadata.uid.clone() {
                    cache.insert(uid, pod.clone());
                }
            }
        }
        Ok(pods)
    }

    #[instrument(skip(self))]
    async fn reconcile_once(&self) -> Result<()> {
        let desired_pods = self.desired_pods().await?;
        let desired_uids: HashSet<String> = desired_pods
            .iter()
            .filter_map(|p| p.metadata.uid.clone())
            .collect();

        // ---- phase 0: drained (E7) -----------------------------------------
        // Low battery: stop everything gracefully but keep the agent alive so it
        // can report status and resume the moment power comes back.
        let drained = self.drained.load(Ordering::Relaxed);
        if drained {
            let to_stop: Vec<(String, Arc<RunningWorkload>)> = {
                let mut running = self.running.lock().await;
                let ids: Vec<String> = running.keys().cloned().collect();
                ids.into_iter()
                    .filter_map(|uid| running.remove(&uid).map(|w| (uid, w)))
                    .collect()
            };
            for (uid, workload) in to_stop {
                info!(pod = %workload.pod_name, "node drained - stopping pod gracefully");
                if let Err(e) = self.workload_lifecycle.stop(&workload).await {
                    warn!(pod = %workload.pod_name, error = %e, "stop on drain failed");
                }
                self.release_ports(&workload.pod_name).await;
                self.backoff.lock().await.remove(&uid);

                // Report the containers as not-running so Services drop this pod
                // as an endpoint instead of sending traffic to a dead port.
                if let Some(pod) = desired_pods.iter().find(|p| p.metadata.uid.as_deref() == Some(uid.as_str())) {
                    self.report_waiting(
                        pod,
                        &uid,
                        "NodeDraining",
                        "node drained (low battery): containers stopped",
                    )
                    .await;
                }
            }
            // Do not start anything while drained.
            return Ok(());
        }

        // ---- phase 1: stop pods that are no longer assigned -----------------
        // The lock is only held long enough to move handles out of the map.
        let orphaned: Vec<(String, Arc<RunningWorkload>)> = {
            let mut running = self.running.lock().await;
            let ids: Vec<String> = running
                .keys()
                .filter(|uid| !desired_uids.contains(*uid))
                .cloned()
                .collect();
            ids.into_iter()
                .filter_map(|uid| running.remove(&uid).map(|w| (uid, w)))
                .collect()
        };
        for (uid, workload) in orphaned {
            info!(pod = %workload.pod_name, "pod no longer desired - stopping");
            if let Err(e) = self.workload_lifecycle.stop(&workload).await {
                warn!(pod = %workload.pod_name, error = %e, "stop failed");
            }
            self.release_ports(&workload.pod_name).await;
            self.backoff.lock().await.remove(&uid);
        }

        // ---- phase 2: reconcile every assigned pod --------------------------
        for pod in &desired_pods {
            let uid = match &pod.metadata.uid {
                Some(u) => u.clone(),
                None => continue,
            };
            let pod_name = pod.metadata.name.clone().unwrap_or_default();
            let restart_policy = pod
                .spec
                .as_ref()
                .and_then(|s| s.restart_policy.clone())
                .unwrap_or_else(|| "Always".to_string());

            // -- being deleted? --
            if pod.metadata.deletion_timestamp.is_some() {
                let namespace = pod
                    .metadata
                    .namespace
                    .clone()
                    .unwrap_or_else(|| "default".to_string());
                info!(pod = %pod_name, "pod is terminating - stopping containers");
                if let Some(w) = self.take_handle(&uid).await {
                    // B6: honour the grace window the API server granted, not the
                    // pod's own default. `kubectl delete --grace-period=5` must
                    // finish in ~5s even if the pod asked for 30.
                    let grace = pod
                        .metadata
                        .deletion_grace_period_seconds
                        .filter(|g| *g >= 0)
                        .map(|g| g as u64)
                        .unwrap_or(w.grace_seconds);
                    if let Err(e) = self.workload_lifecycle.stop_with_grace(&w, grace).await {
                        warn!(pod = %pod_name, error = %e, "stop on delete failed");
                    }
                    self.release_ports(&pod_name).await;
                }
                self.backoff.lock().await.remove(&uid);
                if let Err(e) = self.cp_broker.delete_pod(&pod_name, &namespace).await {
                    warn!(pod = %pod_name, error = %e, "delete pod failed");
                }
                continue;
            }

            // -- already have a handle? --
            let handle = { self.running.lock().await.get(&uid).cloned() };
            if let Some(workload) = handle {
                let status = self.workload_lifecycle.status(&workload).await?;

                if status.phase == PodPhase::Running {
                    self.backoff.lock().await.remove(&uid);
                    self.cp_broker
                        .report_pod_status(&workload.pod_name, &workload.namespace, &status)
                        .await?;
                    continue;
                }

                let want_restart = match restart_policy.as_str() {
                    "Always" => true,
                    "OnFailure" => status.phase == PodPhase::Failed,
                    _ => false,
                };

                if want_restart {
                    // k8s must see Running + waiting(CrashLoopBackOff), never
                    // a terminal phase, otherwise the pod can never come back.
                    let mut waiting = status.clone();
                    waiting.phase = PodPhase::Running;
                    waiting.message = Some("Back-off restarting failed container".to_string());
                    for ct in waiting.containers.iter_mut() {
                        ct.waiting_reason = Some("CrashLoopBackOff".to_string());
                        ct.running = false;
                        ct.ready = false;
                    }
                    self.cp_broker
                        .report_pod_status(&workload.pod_name, &workload.namespace, &waiting)
                        .await?;
                    self.workload_lifecycle.publish_status(&waiting).await;

                    let due = {
                        let mut bo = self.backoff.lock().await;
                        let st = bo.entry(uid.clone()).or_default();
                        let now = Instant::now();
                        if st.next_at.is_none_or(|t| now >= t) {
                            st.attempts += 1;
                            let delay = backoff_delay(st.attempts);
                            st.next_at = Some(now + delay);
                            Some((st.attempts, delay))
                        } else {
                            None
                        }
                    };

                    if let Some((attempt, delay)) = due {
                        info!(
                            pod = %workload.pod_name,
                            phase = ?status.phase,
                            attempt,
                            retry_in_secs = delay.as_secs(),
                            policy = %restart_policy,
                            "restarting container"
                        );
                        eprintln!(
                            "POD_EVENT restarted pod={} attempt={} phase={:?}",
                            workload.pod_name, attempt, status.phase
                        );
                        // A crashed container is replaced immediately; waiting out
                        // the grace period would only slow the restart down.
                        let _ = self.workload_lifecycle.stop_immediately(&workload).await;
                        self.release_ports(&workload.pod_name).await;
                        let restarts = workload.restart_count() + 1;
                        self.take_handle(&uid).await;
                        self.start_pod(pod, &uid, restarts).await;
                    }
                    continue;
                }

                // restartPolicy Never (or OnFailure + success): report once
                info!(pod = %workload.pod_name, phase = ?status.phase, "workload finished");
                eprintln!("POD_EVENT finished pod={} phase={:?}", workload.pod_name, status.phase);
                self.cp_broker
                    .report_pod_status(&workload.pod_name, &workload.namespace, &status)
                    .await?;
                self.workload_lifecycle.publish_status(&status).await;
                self.take_handle(&uid).await;
                self.release_ports(&workload.pod_name).await;
                self.backoff.lock().await.remove(&uid);
                continue;
            }

            // -- no handle yet --
            let k8s_phase = pod
                .status
                .as_ref()
                .and_then(|s| s.phase.as_deref())
                .unwrap_or("");

            // Only Never pods stay terminal. Always/OnFailure pods that were
            // marked terminal by an older agent get re-created.
            if restart_policy == "Never" && (k8s_phase == "Succeeded" || k8s_phase == "Failed") {
                self.mirror_terminal_status(pod).await;
                continue;
            }

            eprintln!("POD_EVENT assigned pod={} image={}", pod_name,
                pod.spec.as_ref().and_then(|s| s.containers.first())
                    .and_then(|c| c.image.clone()).unwrap_or_default());

            self.start_pod(pod, &uid, 0).await;
        }

        Ok(())
    }
}

#[async_trait]
impl ReconciliationService for ReconciliationServiceImpl {
    async fn run_loop(&self) -> Result<()> {
        info!(node = %self.node_name, "reconciliation loop started");
        self.start_pod_watch();
        loop {
            if let Err(e) = self.reconcile_once().await {
                error!(error = %e, "reconcile error - will retry");
            }
            tokio::time::sleep(Duration::from_secs(RECONCILE_INTERVAL_SECS)).await;
        }
    }
}
