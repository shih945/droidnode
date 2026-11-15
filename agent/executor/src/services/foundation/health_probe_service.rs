use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, instrument};

use crate::brokers::ProotBroker;
use crate::error::{DroidError, Result};
use crate::models::{HttpProbe, ProbeConfig};

// ─── Trait ────────────────────────────────────────────────────────────────────

#[async_trait]
pub trait HealthProbeService: Send + Sync {
    /// Run a single probe execution. Returns Ok(()) when the probe passes.
    async fn run_probe(
        &self,
        config: &ProbeConfig,
        rootfs: &Path,
        pid_namespace: Option<u32>,
    ) -> Result<()>;
}

// ─── Implementation ───────────────────────────────────────────────────────────

pub struct HealthProbeServiceImpl {
    proot_broker: Arc<dyn ProotBroker>,
    http_client: reqwest::Client,
}

impl HealthProbeServiceImpl {
    pub fn new(proot_broker: Arc<dyn ProotBroker>) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build probe HTTP client");
        Self {
            proot_broker,
            http_client,
        }
    }
}

#[async_trait]
impl HealthProbeService for HealthProbeServiceImpl {
    #[instrument(skip(self, config), fields(probe_type = ?std::mem::discriminant(&config.probe_type)))]
    async fn run_probe(
        &self,
        config: &ProbeConfig,
        rootfs: &Path,
        _pid_namespace: Option<u32>,
    ) -> Result<()> {
        let timeout = Duration::from_secs(config.timeout_seconds as u64);

        let result = tokio::time::timeout(timeout, self.execute_probe(config, rootfs)).await;

        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(DroidError::ProbeFailed(format!(
                "probe timed out after {}s",
                config.timeout_seconds
            ))),
        }
    }
}

impl HealthProbeServiceImpl {
    async fn execute_probe(&self, config: &ProbeConfig, rootfs: &Path) -> Result<()> {
        if let Some(cmd) = &config.exec_command {
            return self.exec_probe(cmd, rootfs).await;
        }
        if let Some(http) = &config.http_get {
            return self.http_probe(http).await;
        }
        if let Some((host, port)) = &config.tcp_socket {
            return self.tcp_probe(host, *port).await;
        }
        Err(DroidError::Config(
            "probe has no exec, httpGet or tcpSocket configured".into(),
        ))
    }

    /// tcpSocket probe: a successful connect means "healthy".
    async fn tcp_probe(&self, host: &str, port: u16) -> Result<()> {
        let addr = format!("{host}:{port}");
        debug!(%addr, "running tcpSocket probe");
        match tokio::time::timeout(
            Duration::from_secs(5),
            tokio::net::TcpStream::connect(&addr),
        )
        .await
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(DroidError::ProbeFailed(format!("tcp probe failed: {e}"))),
            Err(_) => Err(DroidError::ProbeFailed("tcp probe timed out".into())),
        }
    }

    async fn exec_probe(&self, command: &[String], rootfs: &Path) -> Result<()> {
        debug!(command = ?command, "running exec probe");

        let mut child = self
            .proot_broker
            .execute(rootfs, command, &[], &[], None)
            .await?;

        let status = child
            .wait()
            .await
            .map_err(|e| DroidError::Process(format!("probe wait: {e}")))?;

        if status.success() {
            Ok(())
        } else {
            Err(DroidError::ProbeFailed(format!(
                "exec probe exited with status {}",
                status.code().unwrap_or(-1)
            )))
        }
    }

    async fn http_probe(&self, probe: &HttpProbe) -> Result<()> {
        let url = format!("http://{}:{}{}", probe.host, probe.port, probe.path);
        debug!(%url, "running HTTP GET probe");

        let resp = self
            .http_client
            .get(&url)
            .send()
            .await
            .map_err(|e| DroidError::ProbeFailed(format!("HTTP probe error: {e}")))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(DroidError::ProbeFailed(format!(
                "HTTP probe returned status {}",
                resp.status()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ContainerLimits, HttpProbe, Mount, ProbeConfig, ProbeType};
    use std::path::Path;
    use std::sync::Arc;

    struct NoProot;

    #[async_trait]
    impl ProotBroker for NoProot {
        async fn execute(
            &self,
            _rootfs: &Path,
            _command: &[String],
            _env: &[(String, String)],
            _mounts: &[Mount],
            _cwd: Option<&str>,
        ) -> Result<tokio::process::Child> {
            Err(DroidError::Process("no proot in unit tests".into()))
        }

        async fn execute_as(
            &self,
            _rootfs: &Path,
            _command: &[String],
            _env: &[(String, String)],
            _mounts: &[Mount],
            _cwd: Option<&str>,
            _fake_root: bool,
            _limits: &ContainerLimits,
        ) -> Result<tokio::process::Child> {
            Err(DroidError::Process("no proot in unit tests".into()))
        }
        fn build_command(
            &self,
            _rootfs: &Path,
            _command: &[String],
            _env: &[(String, String)],
            _mounts: &[Mount],
            _cwd: Option<&str>,
            _fake_root: bool,
        ) -> Result<tokio::process::Command> {
            Err(DroidError::Process("no proot in unit tests".into()))
        }
    }

    fn service() -> HealthProbeServiceImpl {
        HealthProbeServiceImpl::new(Arc::new(NoProot))
    }

    fn config(
        tcp: Option<(String, u16)>,
        http: Option<HttpProbe>,
        exec: Option<Vec<String>>,
    ) -> ProbeConfig {
        ProbeConfig {
            probe_type: ProbeType::Readiness,
            exec_command: exec,
            http_get: http,
            tcp_socket: tcp,
            initial_delay_seconds: 0,
            period_seconds: 10,
            failure_threshold: 3,
            success_threshold: 1,
            timeout_seconds: 2,
        }
    }

    #[tokio::test]
    async fn tcp_probe_succeeds_when_port_is_open() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        let probe = config(Some(("127.0.0.1".into(), port)), None, None);
        service()
            .run_probe(&probe, Path::new("/tmp"), None)
            .await
            .expect("open port must pass");
    }

    #[tokio::test]
    async fn tcp_probe_fails_when_port_is_closed() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let probe = config(Some(("127.0.0.1".into(), port)), None, None);
        assert!(service()
            .run_probe(&probe, Path::new("/tmp"), None)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn http_probe_accepts_2xx_and_rejects_5xx() {
        let app = axum::Router::new()
            .route("/ok", axum::routing::get(|| async { "ok" }))
            .route(
                "/bad",
                axum::routing::get(|| async {
                    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "no")
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let ok = config(
            None,
            Some(HttpProbe {
                host: "127.0.0.1".into(),
                port,
                path: "/ok".into(),
            }),
            None,
        );
        service()
            .run_probe(&ok, Path::new("/tmp"), None)
            .await
            .unwrap();

        let bad = config(
            None,
            Some(HttpProbe {
                host: "127.0.0.1".into(),
                port,
                path: "/bad".into(),
            }),
            None,
        );
        assert!(service()
            .run_probe(&bad, Path::new("/tmp"), None)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn probe_without_a_source_is_a_config_error() {
        let probe = config(None, None, None);
        let err = service()
            .run_probe(&probe, Path::new("/tmp"), None)
            .await
            .unwrap_err();
        assert!(matches!(err, DroidError::Config(_)), "unexpected: {err:?}");
    }

    #[tokio::test]
    async fn exec_probe_propagates_broker_failure() {
        let probe = config(None, None, Some(vec!["/bin/true".into()]));
        let err = service()
            .run_probe(&probe, Path::new("/tmp"), None)
            .await
            .unwrap_err();
        assert!(matches!(err, DroidError::Process(_)), "unexpected: {err:?}");
    }
}
