use axum::{body::Body, http::StatusCode, response::Response, routing::get, Router};
use std::net::SocketAddr;
use tracing::{info, instrument};

use crate::error::Result;

pub use crate::exposers::state::*;
use crate::exposers::tls::*;

// ─── Shared state ─────────────────────────────────────────────────────────────

// ─── Exposer ──────────────────────────────────────────────────────────────────

pub struct VirtualKubeletExposer {
    state: KubeletState,
    bind_addr: SocketAddr,
}

impl VirtualKubeletExposer {
    pub fn new(bind_addr: SocketAddr) -> Self {
        Self {
            state: KubeletState::new(),
            bind_addr,
        }
    }

    /// Constructor used by the agent: gives the local API access to the image
    /// store so the on-device page can list and delete images.
    pub fn with_paths(
        bind_addr: SocketAddr,
        rootfs_base: std::path::PathBuf,
        layers_base: std::path::PathBuf,
    ) -> Self {
        Self {
            state: KubeletState::with_paths(rootfs_base, layers_base),
            bind_addr,
        }
    }

    pub fn state(&self) -> KubeletState {
        self.state.clone()
    }

    #[instrument(skip(self), fields(addr = %self.bind_addr))]
    pub async fn serve(&self) -> Result<()> {
        use hyper::service::service_fn;
        use hyper_util::rt::{TokioExecutor, TokioIo};
        use hyper_util::server::conn::auto::Builder as ConnBuilder;
        use std::sync::Arc;
        use tokio_rustls::TlsAcceptor;

        let app = build_router(self.state.clone());

        // Enable on-disk logs before serving so restarts keep the history.
        self.state.logs.set_dir(self.state.ensure_log_dir()).await;

        // Every local IPv4 goes into the cert's SAN: the cluster may reach us
        // over Wi-Fi while the UDP default-route trick reports a cellular IP.
        let local_ip = crate::net_util::primary_ipv4();
        let mut sans = vec!["droidnode".to_string()];
        for ip in crate::net_util::local_ipv4_addresses() {
            if !sans.contains(&ip) {
                sans.push(ip);
            }
        }
        if !sans.contains(&local_ip) {
            sans.push(local_ip.clone());
        }

        let preissued = try_csr_serving_cert(&sans, &self.state.node_name_hint().await).await;
        let tls_cfg = make_tls_config(&sans, preissued)?;
        let acceptor = TlsAcceptor::from(Arc::new(tls_cfg));

        // A failed bind must not take the whole agent down (that used to exit
        // the process and left the node flapping). Retry with backoff instead.
        let listener = loop {
            match tokio::net::TcpListener::bind(self.bind_addr).await {
                Ok(l) => break l,
                Err(e) => {
                    tracing::error!(
                        addr = %self.bind_addr,
                        error = %e,
                        "kubelet bind failed - is another agent instance running? retrying in 5s"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        };

        info!(addr = %self.bind_addr, "kubelet HTTPS server listening");

        loop {
            let (tcp, _) = match listener.accept().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("accept error: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    continue;
                }
            };
            let acceptor = acceptor.clone();
            let app = app.clone();
            let peer = tcp.peer_addr().ok();

            tokio::spawn(async move {
                // Log the handshake outcome: without this a rejection (wrong CA,
                // no shared TLS version, missing SNI) looks exactly like the
                // peer never connecting, and dashboards just show 502.
                let tls = match acceptor.accept(tcp).await {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!(?peer, error = %e, "kubelet TLS handshake failed");
                        return;
                    }
                };
                let peer_cert = tls
                    .get_ref()
                    .1
                    .peer_certificates()
                    .and_then(|certs| certs.first())
                    .map(|c| c.as_ref().to_vec());
                info!(
                    ?peer,
                    client_cert = peer_cert.is_some(),
                    "kubelet TLS connection accepted"
                );
                let io = TokioIo::new(tls);
                let svc = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                    let mut app = app.clone();
                    let peer_cert = peer_cert.clone();
                    let peer_addr = peer;
                    async move {
                        use tower::Service;
                        let mut req = req.map(axum::body::Body::new);
                        // F1: the local Android UI talks over loopback and has no
                        // client cert. Everything else must be the apiserver (or
                        // another client-CA-signed component). Metrics and
                        // healthz stay public for metrics-server / probes.
                        let path = req.uri().path().to_string();
                        let public =
                            path == "/healthz" || path == "/metrics" || path == "/metrics/resource";
                        let remote = peer_addr
                            .map(|addr| !addr.ip().is_loopback())
                            .unwrap_or(true);
                        if remote && peer_cert.is_none() && !public {
                            return Ok::<_, std::convert::Infallible>(
                                Response::builder()
                                    .status(StatusCode::FORBIDDEN)
                                    .header("content-type", "text/plain")
                                    .body(Body::from(
                                        "client certificate required by DroidNode kubelet",
                                    ))
                                    .unwrap(),
                            );
                        }
                        req.extensions_mut().insert(PeerCert(peer_cert));
                        req.extensions_mut().insert(PeerAddr(peer_addr));
                        app.call(req).await
                    }
                });
                ConnBuilder::new(TokioExecutor::new())
                    .serve_connection_with_upgrades(io, svc)
                    .await
                    .ok();
            });
        }
    }
}

// ─── Router ───────────────────────────────────────────────────────────────────

fn build_router(state: KubeletState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/pods", get(list_pods))
        .route("/images", get(list_images))
        .route("/images/delete", axum::routing::post(delete_image))
        .route(
            "/containerLogs/:namespace/:pod_id/:container_name",
            get(container_logs),
        )
        .route("/metrics", get(metrics))
        .route("/metrics/resource", get(metrics_resource))
        .route("/stats/summary", get(stats_summary))
        .route("/drain", axum::routing::post(node_drain))
        .route("/undrain", axum::routing::post(node_undrain))
        .route("/maintenance", axum::routing::post(node_maintenance))
        .route(
            "/exec/:namespace/:pod/:container",
            get(exec_handler).post(exec_handler),
        )
        .route(
            "/attach/:namespace/:pod/:container",
            get(attach_handler).post(attach_handler),
        )
        .route(
            "/portForward/:namespace/:pod",
            get(port_forward_handler).post(port_forward_handler),
        )
        .with_state(state)
}
use crate::exposers::handlers::*;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_returns_ok() {
        let response = build_router(KubeletState::new())
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"ok");
    }

    #[tokio::test]
    async fn unknown_route_is_404() {
        let response = build_router(KubeletState::new())
            .oneshot(Request::builder().uri("/nope").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    #[tokio::test]
    async fn pods_endpoint_returns_pod_list() {
        let response = build_router(KubeletState::new())
            .oneshot(Request::builder().uri("/pods").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["kind"], "PodList");
        assert_eq!(json["items"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn images_endpoint_returns_items() {
        let response = build_router(KubeletState::new())
            .oneshot(
                Request::builder()
                    .uri("/images")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["items"].is_array());
    }
}
