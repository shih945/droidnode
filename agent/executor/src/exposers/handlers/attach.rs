use axum::extract::Extension;
use axum::{
    body::Body,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::exposers::state::*;

/// `kubectl attach` over SPDY.
///
/// Rust exposes the running container's stdin/stdout/stderr as three Unix
/// sockets; the sidecar connects each SPDY stream to the matching socket.
pub(crate) async fn attach_handler(
    State(state): State<KubeletState>,
    Extension(peer): Extension<PeerCert>,
    Path((namespace, pod, container)): Path<(String, String, String)>,
    mut req: axum::extract::Request,
) -> Response {
    if peer.0.is_none() {
        return (StatusCode::FORBIDDEN, "client certificate required").into_response();
    }
    let Some(target) = state.attach_target(&namespace, &pod, &container).await else {
        return (StatusCode::NOT_FOUND, "container not running").into_response();
    };
    let upgrade = req
        .headers()
        .get(axum::http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !upgrade.eq_ignore_ascii_case("SPDY/3.1") {
        return (StatusCode::BAD_REQUEST, "attach requires SPDY/3.1").into_response();
    }
    let protocol = req
        .headers()
        .get("x-stream-protocol-version")
        .and_then(|v| v.to_str().ok())
        .filter(|p| p.starts_with("v3.") || p.starts_with("v4."))
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
        return (StatusCode::SERVICE_UNAVAILABLE, "SPDY sidecar missing").into_response();
    }
    let tmpdir = crate::config::get().tmpdir().to_string();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base = format!("droidnode-attach-{}-{stamp}", std::process::id());
    let stdin_path = std::path::Path::new(&tmpdir).join(format!("{base}-in.sock"));
    let stdout_path = std::path::Path::new(&tmpdir).join(format!("{base}-out.sock"));
    let stderr_path = std::path::Path::new(&tmpdir).join(format!("{base}-err.sock"));
    let ctl_path = std::path::Path::new(&tmpdir).join(format!("{base}-ctl.sock"));
    for p in [&stdin_path, &stdout_path, &stderr_path, &ctl_path] {
        let _ = std::fs::remove_file(p);
    }
    let stdin_listener = match tokio::net::UnixListener::bind(&stdin_path) {
        Ok(l) => l,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("stdin sock: {e}"),
            )
                .into_response()
        }
    };
    let stdout_listener = match tokio::net::UnixListener::bind(&stdout_path) {
        Ok(l) => l,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("stdout sock: {e}"),
            )
                .into_response()
        }
    };
    let stderr_listener = match tokio::net::UnixListener::bind(&stderr_path) {
        Ok(l) => l,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("stderr sock: {e}"),
            )
                .into_response()
        }
    };
    let ctl_listener = match tokio::net::UnixListener::bind(&ctl_path) {
        Ok(l) => l,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("ctl sock: {e}")).into_response()
        }
    };
    let mut child = match tokio::process::Command::new(&helper)
        .arg(&ctl_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("attach sidecar: {e}"),
            )
                .into_response()
        }
    };
    let (mut ctl_stream, _) = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        ctl_listener.accept(),
    )
    .await
    {
        Ok(Ok(pair)) => pair,
        _ => {
            let _ = child.kill().await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "attach sidecar did not connect",
            )
                .into_response();
        }
    };
    let metadata = serde_json::json!({
        "mode": "attach",
        "namespace": namespace,
        "pod": pod,
        "container": container,
        "stdin_path": stdin_path.to_string_lossy(),
        "stdout_path": stdout_path.to_string_lossy(),
        "stderr_path": stderr_path.to_string_lossy(),
        "tmpdir": tmpdir,
        "env": {},
    });
    let mut line = serde_json::to_vec(&metadata).unwrap_or_default();
    line.push(b'\n');
    if let Err(e) = ctl_stream.write_all(&line).await {
        let _ = child.kill().await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("attach metadata: {e}"),
        )
            .into_response();
    }

    let on_upgrade = hyper::upgrade::on(&mut req);
    let paths = (
        stdin_path.clone(),
        stdout_path.clone(),
        stderr_path.clone(),
        ctl_path.clone(),
    );
    tokio::spawn(async move {
        let out_target = target.clone();
        let err_target = target.clone();
        let in_target = target;
        let out_task = tokio::spawn(async move {
            if let Ok((mut sock, _)) = stdout_listener.accept().await {
                let mut rx = out_target.output.subscribe();
                loop {
                    match rx.recv().await {
                        Ok(frame) if frame.channel == 1 => {
                            if sock.write_all(&frame.data).await.is_err() {
                                break;
                            }
                        }
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
                let _ = sock.shutdown().await;
            }
        });
        let err_task = tokio::spawn(async move {
            if let Ok((mut sock, _)) = stderr_listener.accept().await {
                let mut rx = err_target.output.subscribe();
                loop {
                    match rx.recv().await {
                        Ok(frame) if frame.channel == 2 => {
                            if sock.write_all(&frame.data).await.is_err() {
                                break;
                            }
                        }
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
                let _ = sock.shutdown().await;
            }
        });
        let in_task = tokio::spawn(async move {
            if let Ok((mut sock, _)) = stdin_listener.accept().await {
                let mut buf = [0u8; 8192];
                loop {
                    match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if in_target.stdin.send(buf[..n].to_vec()).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        });

        match on_upgrade.await {
            Ok(upgraded) => {
                let mut upgraded = hyper_util::rt::TokioIo::new(upgraded);
                let _ = tokio::io::copy_bidirectional(&mut upgraded, &mut ctl_stream).await;
            }
            Err(e) => tracing::warn!(error = %e, "attach upgrade failed"),
        }
        out_task.abort();
        err_task.abort();
        in_task.abort();
        let _ = child.kill().await;
        let _ = child.wait().await;
        for p in [paths.0, paths.1, paths.2, paths.3] {
            let _ = std::fs::remove_file(p);
        }
    });

    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(axum::http::header::CONNECTION, "Upgrade")
        .header(axum::http::header::UPGRADE, "SPDY/3.1")
        .header("X-Stream-Protocol-Version", protocol)
        .body(Body::empty())
        .unwrap()
}
