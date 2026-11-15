use super::*;

impl WorkloadLifecycleServiceImpl {
    /// Spawn the probe tasks declared by one container (A9).
    pub(super) async fn spawn_probes(
        &self,
        pod: &Pod,
        container: &k8s_openapi::api::core::v1::Container,
        rootfs: &Path,
        pgid: i32,
        ready: Arc<AtomicBool>,
        alive: Arc<AtomicBool>,
    ) {
        let pod_name = pod.metadata.name.clone().unwrap_or_default();
        let container_name = container.name.clone();
        let ports: Vec<(String, u16)> = container
            .ports
            .clone()
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.container_port > 0)
            .map(|p| (p.name.unwrap_or_default(), p.container_port as u16))
            .collect();

        let startup = container
            .startup_probe
            .as_ref()
            .map(|p| to_probe_config(p, crate::models::ProbeType::Startup, &ports));
        let liveness = container
            .liveness_probe
            .as_ref()
            .map(|p| to_probe_config(p, crate::models::ProbeType::Liveness, &ports));
        let readiness = container
            .readiness_probe
            .as_ref()
            .map(|p| to_probe_config(p, crate::models::ProbeType::Readiness, &ports));

        if startup.is_none() && liveness.is_none() && readiness.is_none() {
            return;
        }

        let svc = Arc::clone(&self.probe_service);
        let rootfs = rootfs.to_path_buf();
        info!(
            pod = %pod_name,
            container = %container_name,
            liveness = liveness.is_some(),
            readiness = readiness.is_some(),
            startup = startup.is_some(),
            "health probes enabled"
        );

        tokio::spawn(async move {
            if let Some(cfg) = readiness {
                let svc = Arc::clone(&svc);
                let rootfs = rootfs.clone();
                let ready = Arc::clone(&ready);
                let alive = Arc::clone(&alive);
                let pod_name = pod_name.clone();
                let container_name = container_name.clone();
                tokio::spawn(async move {
                    run_probe_loop(
                        svc,
                        cfg,
                        rootfs,
                        pgid,
                        ready,
                        alive,
                        pod_name,
                        container_name,
                    )
                    .await
                });
            }

            if let Some(cfg) = startup {
                run_probe_loop(
                    Arc::clone(&svc),
                    cfg,
                    rootfs.clone(),
                    pgid,
                    Arc::clone(&ready),
                    Arc::clone(&alive),
                    pod_name.clone(),
                    container_name.clone(),
                )
                .await;
            }
            if alive.load(Ordering::Relaxed) {
                if let Some(cfg) = liveness {
                    run_probe_loop(
                        svc,
                        cfg,
                        rootfs,
                        pgid,
                        ready,
                        alive,
                        pod_name,
                        container_name,
                    )
                    .await;
                }
            }
        });
    }
}
