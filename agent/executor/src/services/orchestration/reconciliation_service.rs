use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex, Notify};
use tracing::{debug, error, info, instrument, warn};

use crate::brokers::{ControlPlaneBroker, PodWatchEvent};
use crate::error::Result;
use crate::models::{ContainerRunStatus, Pod, PodPhase, PodRunStatus};
use crate::services::foundation::event_recording_service::EventRecordingService;
use crate::services::orchestration::{
    image_orchestration_service::ImageOrchestrationService,
    workload_lifecycle_service::{RunningWorkload, WorkloadLifecycleService},
};

#[path = "reconciliation_service/policy.rs"]
mod policy;
#[path = "reconciliation_service/watch.rs"]
mod watch;

use policy::{backoff_delay, effective_grace, terminal_for_policy, want_restart};
use watch::apply_watch_event;

#[path = "reconciliation_service/reconcile.rs"]
mod reconcile;
#[path = "reconciliation_service/start.rs"]
mod start;

const RECONCILE_INTERVAL_SECS: u64 = 15;
/// A18: even with a live watch, do a full LIST this often as a backstop.
const FULL_RESYNC_SECS: u64 = 60;
/// kubelet-style exponential backoff: 10s, 20s, 40s ... capped at 5 min.
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

/// One pod waiting for the serial start worker (image pull + unpack + spawn).
///
/// Starts stay serialised (the port claim and the layer cache assume it), but
/// they no longer run inside the reconcile loop: a slow registry used to block
/// deletions for the whole duration of the pull timeout.
struct StartRequest {
    pod: Pod,
    uid: String,
    previous_restarts: i32,
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
    ///
    /// Takes `Arc<Self>` because the loop owns a background start worker and a
    /// watch task, both of which outlive a single pass.
    async fn run_loop(self: Arc<Self>) -> Result<()>;
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
    /// pod_uid -> a start is queued or running in the start worker.
    starting: Mutex<HashSet<String>>,
    /// Queue feeding the serial start worker; set once the loop is running.
    start_tx: Mutex<Option<mpsc::UnboundedSender<StartRequest>>>,
    /// Woken by the watch task so a delete is handled in milliseconds instead
    /// of waiting for the next 15 s tick.
    wake: Arc<Notify>,
    /// Bumped by the watch task on every event; lets `run_loop` notice an event
    /// that arrived while it was reconciling (and skip the sleep).
    pod_events: Arc<AtomicU64>,
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
            starting: Mutex::new(HashSet::new()),
            start_tx: Mutex::new(None),
            wake: Arc::new(Notify::new()),
            pod_events: Arc::new(AtomicU64::new(0)),
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
            self.event_service
                .record_warning(reason, message, pod_ref)
                .await
        } else {
            self.event_service
                .record_normal(reason, message, pod_ref)
                .await
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
                Err(_) => return Some(PortProblem::InUse(*port, "another process".to_string())),
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
            // Pending, not Running: the container has not started yet (image
            // pull, rootfs creation). Users read Running as "the process is
            // up", so a stuck pull must not look healthy.
            phase: PodPhase::Pending,
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
        let wake = Arc::clone(&self.wake);
        let events = Arc::clone(&self.pod_events);
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
                    // Only deletions are worth interrupting the reconcile sleep
                    // for. Waking on every event would loop forever: our own
                    // status patches come back as Applied events.
                    let urgent = match &event {
                        PodWatchEvent::Deleted(_) => true,
                        PodWatchEvent::Applied(pod) => {
                            pod.metadata.deletion_timestamp.is_some()
                        }
                        PodWatchEvent::Restarted(_) => false,
                    };

                    let mut map = cache.lock().await;
                    let snapshot = apply_watch_event(&mut map, event);
                    let len = map.len();
                    drop(map);

                    if snapshot {
                        ready.store(true, Ordering::Relaxed);
                        info!(pods = len, "pod watch ready (full snapshot)");
                    }
                    if urgent {
                        events.fetch_add(1, Ordering::Relaxed);
                        wake.notify_one();
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
        let stale =
            self.last_full_list.lock().await.elapsed() >= Duration::from_secs(FULL_RESYNC_SECS);

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
}

#[async_trait]
impl ReconciliationService for ReconciliationServiceImpl {
    async fn run_loop(self: Arc<Self>) -> Result<()> {
        info!(node = %self.node_name, "reconciliation loop started");
        self.start_pod_watch();

        // Serial start worker. Pulls stay one-at-a-time (the port claim and the
        // image layer cache assume it) but they run off the reconcile loop, so a
        // slow registry can no longer delay a deletion.
        let (tx, mut rx) = mpsc::unbounded_channel::<StartRequest>();
        *self.start_tx.lock().await = Some(tx);
        {
            let me = Arc::clone(&self);
            tokio::spawn(async move {
                while let Some(req) = rx.recv().await {
                    // The pod may have been deleted while it sat in the queue.
                    let still_assigned = {
                        let ready = me.watch_ready.load(Ordering::Relaxed);
                        let cached = me.pod_cache.lock().await.contains_key(&req.uid);
                        !ready || cached
                    };
                    if still_assigned {
                        me.start_pod(&req.pod, &req.uid, req.previous_restarts)
                            .await;
                    } else {
                        debug!(
                            pod = %req.pod.metadata.name.as_deref().unwrap_or("?"),
                            "start skipped: pod was deleted while queued"
                        );
                    }
                    me.starting.lock().await.remove(&req.uid);
                }
            });
        }

        loop {
            let seen = self.pod_events.load(Ordering::Relaxed);
            if let Err(e) = self.reconcile_once().await {
                error!(error = %e, "reconcile error - will retry");
            }
            // A deletion that arrived while this pass was running means the
            // world already moved on: reconcile again instead of sleeping.
            if self.pod_events.load(Ordering::Relaxed) != seen {
                continue;
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(RECONCILE_INTERVAL_SECS)) => {}
                _ = self.wake.notified() => {
                    debug!("reconcile woken by a pod deletion");
                }
            }
        }
    }
}
