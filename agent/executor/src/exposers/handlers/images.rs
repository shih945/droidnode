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

/// Standard kubelet `ImageList` (`names[]`, `sizeBytes`, `id`).
/// The APK uses `/droidnode/images`, which keeps the richer custom fields.
pub(crate) async fn list_images(State(state): State<KubeletState>) -> impl IntoResponse {
    let base = (*state.rootfs_base).clone();
    let images =
        tokio::task::spawn_blocking(move || crate::services::foundation::image_store::list(&base))
            .await
            .unwrap_or_default();

    let items: Vec<serde_json::Value> = images
        .iter()
        .map(|image| {
            serde_json::json!({
                "names": [image.name.clone()],
                "sizeBytes": image.unpacked_size,
                "id": image.manifest_digest.clone(),
            })
        })
        .collect();
    Json(serde_json::json!({ "images": items }))
}

pub(crate) async fn list_droidnode_images(State(state): State<KubeletState>) -> impl IntoResponse {
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
        return Err(DroidError::ImageInUse { id: q.id.clone() });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn deleting_an_image_used_by_a_running_pod_is_rejected() {
        let state = KubeletState::new();
        let image_rootfs = std::path::PathBuf::from("/data/rootfs/registry/repo/tag");
        state.mark_in_use(&image_rootfs).await;

        let err = delete_image(
            State(state),
            Query(DeleteImageQuery {
                id: image_rootfs.to_string_lossy().to_string(),
            }),
        )
        .await
        .expect_err("deleting an in-use image must fail");

        assert!(matches!(err, crate::error::DroidError::ImageInUse { .. }));
    }
}
