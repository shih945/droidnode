use super::query::parse_exec_query;
use axum::extract::Extension;
use axum::{
    body::Body,
    extract::{Path, RawQuery, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use tokio::io::AsyncWriteExt;
use tracing::info;

use crate::exposers::state::*;

/// A1: `kubectl exec` transport.
///
/// The apiserver uses `Upgrade: SPDY/3.1` (POST) for remote command. Rust owns
/// TLS and the 101 response, then hands the raw upgraded byte stream to the Go
/// sidecar, which speaks SPDY/3.1 (moby/spdystream) and bridges the streams to
/// a proot process.
pub(crate) async fn exec_handler(
    State(state): State<KubeletState>,
    Extension(peer): Extension<PeerCert>,
    Path((namespace, pod, container)): Path<(String, String, String)>,
    RawQuery(raw): RawQuery,
    mut req: axum::extract::Request,
) -> Response {
    // A10: only the apiserver (verified client cert) may exec. The local UI
    // uses the maintenance endpoints and never presents a client cert.
    if peer.0.is_none() {
        tracing::warn!(
            namespace = %namespace,
            pod = %pod,
            container = %container,
            "exec rejected: no client certificate"
        );
        return (
            StatusCode::FORBIDDEN,
            "exec requires a client certificate signed by the cluster client CA",
        )
            .into_response();
    }

    let Some(target) = state.exec_target(&namespace, &pod, &container).await else {
        tracing::warn!(
            namespace = %namespace,
            pod = %pod,
            container = %container,
            "exec rejected: container not registered"
        );
        return (StatusCode::NOT_FOUND, "container not running").into_response();
    };

    let (command, stdin, stdout, stderr, tty) = parse_exec_query(raw);
    if command.is_empty() {
        return (StatusCode::BAD_REQUEST, "command is required").into_response();
    }

    let upgrade = req
        .headers()
        .get(axum::http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !upgrade.eq_ignore_ascii_case("SPDY/3.1") {
        return (
            StatusCode::BAD_REQUEST,
            "exec requires Upgrade: SPDY/3.1 (WebSocket-to-kubelet is not enabled)",
        )
            .into_response();
    }

    let protocol = req
        .headers()
        .get("x-stream-protocol-version")
        .and_then(|v| v.to_str().ok())
        .filter(|p| {
            *p == "v4.channel.k8s.io" || *p == "v3.channel.k8s.io" || *p == "v2.channel.k8s.io"
        })
        .unwrap_or("v4.channel.k8s.io")
        .to_string();

    let proot_path = match crate::config::get().proot_path() {
        Some(p) => p.to_string_lossy().to_string(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "DROIDNODE_PROOT_PATH is not set",
            )
                .into_response()
        }
    };
    let helper = crate::config::get()
        .spdy_helper()
        .map(str::to_string)
        .unwrap_or_else(|| {
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

    let tmpdir = crate::config::get().tmpdir().to_string();
    let socket_path = std::path::Path::new(&tmpdir).join(format!(
        "droidnode-spdy-{}-{}.sock",
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
                format!("spdy sidecar spawn: {e}"),
            )
                .into_response();
        }
    };

    let (mut stream, _) =
        match tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept()).await {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => {
                let _ = child.kill().await;
                let _ = std::fs::remove_file(&socket_path);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("spdy socket accept: {e}"),
                )
                    .into_response();
            }
            Err(_) => {
                let _ = child.kill().await;
                let _ = std::fs::remove_file(&socket_path);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "spdy sidecar did not connect",
                )
                    .into_response();
            }
        };

    let loader = std::path::Path::new(&proot_path)
        .with_file_name("libproot_loader.so")
        .to_string_lossy()
        .to_string();
    let metadata = serde_json::json!({
        "rootfs": target.rootfs.to_string_lossy(),
        "proot": proot_path,
        "loader": loader,
        "tmpdir": tmpdir,
        "command": command,
        "tty": tty,
        "env": {},
    });
    let mut line = match serde_json::to_vec(&metadata) {
        Ok(v) => v,
        Err(e) => {
            let _ = child.kill().await;
            let _ = std::fs::remove_file(&socket_path);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("spdy metadata: {e}"),
            )
                .into_response();
        }
    };
    line.push(b'\n');
    if let Err(e) = stream.write_all(&line).await {
        let _ = child.kill().await;
        let _ = std::fs::remove_file(&socket_path);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("spdy metadata write: {e}"),
        )
            .into_response();
    }

    let on_upgrade = hyper::upgrade::on(&mut req);
    info!(
        namespace = %namespace,
        pod = %pod,
        container = %container,
        tty,
        stdin,
        stdout,
        stderr,
        protocol = %protocol,
        "exec SPDY handoff"
    );

    tokio::spawn(async move {
        match on_upgrade.await {
            Ok(upgraded) => {
                let mut upgraded = hyper_util::rt::TokioIo::new(upgraded);
                let _ = tokio::io::copy_bidirectional(&mut upgraded, &mut stream).await;
            }
            Err(e) => tracing::warn!(error = %e, "exec upgrade failed"),
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
