use axum::{
    extract::{Query, State},
    response::IntoResponse,
    Json,
};

use crate::exposers::state::*;

#[derive(serde::Deserialize)]
pub(crate) struct DeleteImageQuery {
    /// Absolute rootfs directory as returned by `/images`.
    id: String,
}

pub(crate) async fn list_images(State(state): State<KubeletState>) -> impl IntoResponse {
    let base = (*state.rootfs_base).clone();
    let images =
        tokio::task::spawn_blocking(move || crate::services::foundation::image_store::list(&base))
            .await
            .unwrap_or_default();

    let total: u64 = images.iter().map(|i| i.total_size()).sum();
    Json(serde_json::json!({
        "images": images,
        "total_size": total,
        "count": images.len(),
    }))
}

pub(crate) async fn delete_image(
    State(state): State<KubeletState>,
    Query(q): Query<DeleteImageQuery>,
) -> Result<Json<serde_json::Value>, crate::error::DroidError> {
    use crate::error::DroidError;

    if state.is_in_use(&q.id).await {
        return Err(DroidError::Conflict(
            "镜像正在被运行中的 Pod 使用，请先删除该 Pod".into(),
        ));
    }

    let base = (*state.rootfs_base).clone();
    let layers = (*state.layers_base).clone();
    let id = q.id.clone();

    let bytes = tokio::task::spawn_blocking(move || {
        crate::services::foundation::image_store::delete(&base, &layers, &id)
    })
    .await
    .map_err(|e| DroidError::Workload(format!("join error: {e}")))??;

    Ok(Json(
        serde_json::json!({ "deleted": q.id, "freed_bytes": bytes }),
    ))
}
