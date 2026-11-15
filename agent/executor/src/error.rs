use thiserror::Error;

#[derive(Error, Debug)]
pub enum DroidError {
    #[error("OCI registry error: {0}")]
    OciRegistry(String),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Filesystem error: {0}")]
    Filesystem(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Kubernetes API error: {0}")]
    KubeApi(#[from] kube::Error),
    #[error("Kubernetes watch error: {0}")]
    Watch(String),
    #[error("Process execution error: {0}")]
    Process(String),
    #[error("Image not found: {0}")]
    ImageNotFound(String),
    #[error("Layer digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch { expected: String, actual: String },
    #[error("Workload error: {0}")]
    Workload(String),
    #[error("Health probe failed: {0}")]
    ProbeFailed(String),
    #[error("Node registration error: {0}")]
    NodeRegistration(String),
    #[error("Configuration error: {0}")]
    Config(String),
    #[error("Unsupported operation: {0}")]
    Unsupported(String),
    #[error("{0}")]
    Conflict(String),
    #[error("image is in use: {id}")]
    ImageInUse { id: String },
}

pub type Result<T> = std::result::Result<T, DroidError>;

impl axum::response::IntoResponse for DroidError {
    fn into_response(self) -> axum::response::Response {
        use axum::http::StatusCode;
        use axum::Json;

        let status = match &self {
            DroidError::ImageNotFound(_) => StatusCode::NOT_FOUND,
            DroidError::OciRegistry(_) | DroidError::DigestMismatch { .. } => {
                StatusCode::BAD_GATEWAY
            }
            DroidError::Config(_) | DroidError::Unsupported(_) => StatusCode::BAD_REQUEST,
            DroidError::Conflict(_) | DroidError::ImageInUse { .. } => StatusCode::CONFLICT,
            DroidError::ProbeFailed(_) => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (
            status,
            Json(serde_json::json!({ "error": self.to_string() })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    #[test]
    fn maps_error_kinds_to_http_status() {
        assert_eq!(
            DroidError::ImageNotFound("x".into())
                .into_response()
                .status(),
            axum::http::StatusCode::NOT_FOUND
        );
        assert_eq!(
            DroidError::Config("x".into()).into_response().status(),
            axum::http::StatusCode::BAD_REQUEST
        );
        assert_eq!(
            DroidError::ProbeFailed("x".into()).into_response().status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            DroidError::Workload("x".into()).into_response().status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            DroidError::Conflict("x".into()).into_response().status(),
            axum::http::StatusCode::CONFLICT
        );
    }
}
