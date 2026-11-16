use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};

use crate::exposers::state::*;

/// E7: ask the agent to stop its pods gracefully but keep running.
pub(crate) async fn node_drain(State(state): State<KubeletState>) -> impl IntoResponse {
    state
        .drained
        .store(true, std::sync::atomic::Ordering::Relaxed);
    tracing::warn!("node drain requested - pods will be stopped gracefully, agent keeps running");
    (StatusCode::OK, Json(serde_json::json!({ "drained": true })))
}

pub(crate) async fn node_undrain(State(state): State<KubeletState>) -> impl IntoResponse {
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
pub(crate) struct MaintenanceQuery {
    on: bool,
}

/// G2: operator maintenance mode.
///
/// `on=true` cordons the Node (`spec.unschedulable=true`) and drains locally;
/// `on=false` resumes scheduling and lets the reconciler restart containers.
/// This is the only path that changes `spec.unschedulable`; `/undrain` is the
/// battery-recovery path and is refused while maintenance is active.
pub(crate) async fn node_maintenance(
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
pub(crate) async fn stats_summary(State(state): State<KubeletState>) -> impl IntoResponse {
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
    let cpu_obj =
        |rate: u64, total: u64| json!({ "usageNanoCores": rate, "usageCoreNanoSeconds": total });

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
            // kubelet v1.29 expects node.cpu/node.memory at this level; the
            // machine block below is an extra convenience for dashboards.
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
pub(crate) fn statvfs_pair(path: &std::path::Path) -> Option<(u64, u64)> {
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
pub(crate) async fn metrics_resource(State(state): State<KubeletState>) -> impl IntoResponse {
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
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        out,
    )
}
