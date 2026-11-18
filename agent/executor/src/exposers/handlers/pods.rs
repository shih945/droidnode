use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};

use crate::exposers::state::*;

// ─── Handlers ─────────────────────────────────────────────────────────────────

pub(crate) async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

fn qos_class(spec: &k8s_openapi::api::core::v1::PodSpec) -> &'static str {
    let mut any = false;
    let mut guaranteed = true;
    for container in &spec.containers {
        let Some(resources) = container.resources.as_ref() else {
            guaranteed = false;
            continue;
        };
        for (name, request) in resources.requests.iter().flatten() {
            any = true;
            let limit = resources
                .limits
                .as_ref()
                .and_then(|limits| limits.get(name));
            if limit != Some(request) {
                guaranteed = false;
            }
        }
        if let Some(limits) = resources.limits.as_ref() {
            for name in limits.keys() {
                any = true;
                if !resources
                    .requests
                    .as_ref()
                    .is_some_and(|requests| requests.contains_key(name))
                {
                    guaranteed = false;
                }
            }
        }
    }
    if !any {
        "BestEffort"
    } else if guaranteed {
        "Guaranteed"
    } else {
        "Burstable"
    }
}

/// `/pods` returns a standard `v1.PodList`. Tools that re-use the kubelet
/// API expect `kind: PodList` + `items[]`, not a custom envelope. The Pod spec
/// is cached by the reconciler so containers/images are present here too.
pub(crate) async fn list_pods(State(state): State<KubeletState>) -> impl IntoResponse {
    use serde_json::json;

    let statuses = state.pod_statuses.read().await;
    let node_name = state.node_name_hint().await;
    let node_ip = crate::net_util::primary_ipv4();

    let mut items: Vec<serde_json::Value> = Vec::new();
    for s in statuses.values() {
        let spec = state.pod_spec(&s.pod_uid).await;
        let spec_json = spec
            .as_ref()
            .and_then(|pod| pod.spec.as_ref())
            .and_then(|spec| serde_json::to_value(spec).ok())
            .unwrap_or_else(|| json!({ "nodeName": node_name }));

        let mut images: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        if let Some(pod) = &spec {
            if let Some(spec) = &pod.spec {
                for container in &spec.containers {
                    if let Some(image) = &container.image {
                        images.insert(container.name.clone(), image.clone());
                    }
                }
            }
        }

        let mut started: Vec<String> = Vec::new();
        let containers: Vec<serde_json::Value> = s
            .containers
            .iter()
            .map(|c| {
                if let Some(at) = &c.started_at {
                    started.push(at.clone());
                }
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
                let image = images.get(&c.name).cloned().unwrap_or_default();
                json!({
                    "name": c.name,
                    "image": image,
                    "imageID": if image.is_empty() { String::new() } else { format!("proot://{image}") },
                    "ready": c.ready,
                    "started": c.running,
                    "restartCount": c.restart_count,
                    "state": state_json
                })
            })
            .collect();

        started.sort();
        // A running pod adopted after an agent restart may not have a
        // container start timestamp yet; kubelet always reports startTime.
        let start_time = started
            .first()
            .cloned()
            .unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string());

        let all_ready = !s.containers.is_empty() && s.containers.iter().all(|c| c.ready);
        let condition = |kind: &str, ok: bool| {
            json!({
                "type": kind,
                "status": if ok { "True" } else { "False" },
                "lastTransitionTime": start_time.clone(),
            })
        };

        items.push(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": s.pod_name,
                "namespace": s.namespace,
                "uid": s.pod_uid,
            },
            "spec": spec_json,
            "status": {
                "phase": s.phase.as_str(),
                "podIP": node_ip,
                "podIPs": [ { "ip": node_ip } ],
                "hostIP": node_ip,
                "message": s.message,
                "startTime": start_time,
                "qosClass": spec.as_ref().and_then(|p| p.spec.as_ref()).map(qos_class).unwrap_or("BestEffort"),
                "conditions": [
                    condition("PodScheduled", true),
                    condition("Initialized", true),
                    condition("ContainersReady", all_ready),
                    condition("Ready", all_ready)
                ],
                "containerStatuses": containers,
            }
        }));
    }

    Json(json!({
        "kind": "PodList",
        "apiVersion": "v1",
        "metadata": {},
        "items": items,
    }))
}
