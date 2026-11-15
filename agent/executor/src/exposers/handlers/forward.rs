use axum::extract::Extension;
use axum::{
    body::Body,
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use tokio::io::AsyncWriteExt;

use crate::exposers::state::*;

/// A3: `kubectl port-forward` transport. Each client-opened SPDY stream has a
/// `port` header; the sidecar dials `127.0.0.1:<port>` inside the device netns.
pub(crate) async fn port_forward_handler(
    Extension(peer): Extension<PeerCert>,
    Path((namespace, pod)): Path<(String, String)>,
    mut req: axum::extract::Request,
) -> Response {
    if peer.0.is_none() {
        return (StatusCode::FORBIDDEN, "client certificate required").into_response();
    }
    let upgrade = req
        .headers()
        .get(axum::http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !upgrade.eq_ignore_ascii_case("SPDY/3.1") {
        return (StatusCode::BAD_REQUEST, "port-forward requires SPDY/3.1").into_response();
    }
    let protocol = req
        .headers()
        .get("x-stream-protocol-version")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("portforward.k8s.io")
        .to_string();

    let proot_path = match std::env::var("DROIDNODE_PROOT_PATH") {
        Ok(p) => p,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "DROIDNODE_PROOT_PATH is not set",
            )
                .into_response()
        }
    };
    let helper = std::env::var("DROIDNODE_SPDY_HELPER").unwrap_or_else(|_| {
        std::path::Path::new(&proot_path)
            .with_file_name("libdroidnode_spdy.so")
            .to_string_lossy()
            .to_string()
    });
    if !std::path::Path::new(&helper).exists() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("SPDY sidecar not found at {helper}"),
        )
            .into_response();
    }
    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/data/local/tmp".to_string());
    let socket_path = std::path::Path::new(&tmpdir).join(format!(
        "droidnode-pf-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_file(&socket_path);
    let listener = match tokio::net::UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("spdy socket: {e}"),
            )
                .into_response()
        }
    };
    let mut child = match tokio::process::Command::new(&helper)
        .arg(&socket_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&socket_path);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("spdy spawn: {e}"),
            )
                .into_response();
        }
    };
    let (mut stream, _) =
        match tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept()).await {
            Ok(Ok(pair)) => pair,
            _ => {
                let _ = child.kill().await;
                let _ = std::fs::remove_file(&socket_path);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "spdy sidecar did not connect",
                )
                    .into_response();
            }
        };
    let metadata = serde_json::json!({
        "mode": "portforward",
        "namespace": namespace,
        "pod": pod,
        "tmpdir": tmpdir,
        "env": {},
    });
    let mut line = serde_json::to_vec(&metadata).unwrap_or_default();
    line.push(b'\n');
    if let Err(e) = stream.write_all(&line).await {
        let _ = child.kill().await;
        let _ = std::fs::remove_file(&socket_path);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("spdy metadata: {e}"),
        )
            .into_response();
    }
    let on_upgrade = hyper::upgrade::on(&mut req);
    tracing::info!(namespace = %namespace, pod = %pod, protocol = %protocol, "port-forward SPDY handoff");
    tokio::spawn(async move {
        match on_upgrade.await {
            Ok(upgraded) => {
                let mut upgraded = hyper_util::rt::TokioIo::new(upgraded);
                let _ = tokio::io::copy_bidirectional(&mut upgraded, &mut stream).await;
            }
            Err(e) => tracing::warn!(error = %e, "port-forward upgrade failed"),
        }
        let _ = child.kill().await;
        let _ = child.wait().await;
        let _ = std::fs::remove_file(&socket_path);
    });
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(axum::http::header::CONNECTION, "Upgrade")
        .header(axum::http::header::UPGRADE, "SPDY/3.1")
        .header("X-Stream-Protocol-Version", protocol)
        .body(Body::empty())
        .unwrap()
}
