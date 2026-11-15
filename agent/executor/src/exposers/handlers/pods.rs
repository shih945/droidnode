use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};

use crate::exposers::state::*;

// ─── Handlers ─────────────────────────────────────────────────────────────────

pub(crate) async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

/// `/pods` returns a standard `v1.PodList` (A19). Tools that re-use the kubelet
/// API expect `kind: PodList` + `items[]`, not a custom envelope.
pub(crate) async fn list_pods(State(state): State<KubeletState>) -> impl IntoResponse {
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
