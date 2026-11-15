use anyhow::Context;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use executor::{
    brokers::{
        ControlPlaneBrokerImpl, FilesystemBrokerImpl, OciRegistryBrokerImpl, ProotBroker,
        ProotBrokerImpl,
    },
    exposers::VirtualKubeletExposer,
    models::ImageRef,
    services::{
        foundation::{
            EventRecordingServiceImpl, HealthProbeServiceImpl, ImagePullServiceImpl,
            ImageUnpackServiceImpl, NodeCapabilityServiceImpl, WorkloadExecutionServiceImpl,
        },
        orchestration::{
            ImageOrchestrationService, ImageOrchestrationServiceImpl, NodeRegistrationService,
            NodeRegistrationServiceImpl, ReconciliationService, ReconciliationServiceImpl,
            WorkloadLifecycleServiceImpl,
        },
    },
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // rustls 0.23 requires an explicit CryptoProvider before any TLS operations.
    // With both kube and reqwest using rustls, the provider must be set once here.
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();

    // No ANSI: our stdout is piped into the Android log and shown in the app,
    // so colour escapes would arrive as literal "[2m"/"[32m" noise. Android's
    // logcat also strips the ESC byte, leaving those fragments behind.
    // `RUST_LOG` is set per-launch by the Android settings page (E14), so honour
    // it; only fall back to `info` when the variable is absent. Appending an
    // `info` directive unconditionally used to silently override `RUST_LOG=debug`.
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_ansi(false)
        .init();

    let config = Config::from_env();
    info!(
        node_id = %config.node_id,
        proot_path = %config.proot_path.display(),
        layers_root = %config.layers_root.display(),
        kubelet_addr = %config.kubelet_addr,
        "droidnode agent starting"
    );

    // Mirror config is resolved once per process; log the effective table so the
    // settings page can show the real value instead of inferring it from pulls.
    executor::registry_mirror::log_summary();

    // G3: reconcile on-disk state before starting anything. A crash during
    // unpack leaves a rootfs tree without its READY sentinel; if we reuse it the
    // next unpack writes layers on top of a half-applied tree.
    {
        let rootfs = config.rootfs_base.clone();
        match tokio::task::spawn_blocking(move || {
            executor::services::foundation::image_store::cleanup_incomplete(&rootfs)
        })
        .await
        {
            Ok((images, pods, bytes)) if images + pods > 0 => warn!(
                images,
                pods, bytes, "removed incomplete rootfs trees left by a previous crash"
            ),
            Ok(_) => info!("no incomplete rootfs trees found"),
            Err(e) => warn!(error = %e, "rootfs startup reconciliation failed"),
        }
    }

    // ─── Brokers ─────────────────────────────────────────────────────────────

    let oci_broker = Arc::new(OciRegistryBrokerImpl::new());

    let fs_broker = Arc::new(FilesystemBrokerImpl::new(config.layers_root.clone()));

    let proot_broker = Arc::new(ProotBrokerImpl::new(config.proot_path.clone()));

    let cp_broker = Arc::new(
        ControlPlaneBrokerImpl::new(config.node_id.clone(), config.kubelet_addr.port())
            .await
            .context("failed to create k8s client — is KUBECONFIG set?")?,
    );

    // ─── Foundation services ─────────────────────────────────────────────────

    let pull_service = Arc::new(ImagePullServiceImpl::new(
        Arc::clone(&oci_broker) as Arc<dyn executor::brokers::OciRegistryBroker>,
        Arc::clone(&fs_broker) as Arc<dyn executor::brokers::FilesystemBroker>,
    ));

    let unpack_service = Arc::new(ImageUnpackServiceImpl::new(
        Arc::clone(&fs_broker) as Arc<dyn executor::brokers::FilesystemBroker>
    ));

    let execution_service = Arc::new(WorkloadExecutionServiceImpl::new(
        Arc::clone(&proot_broker) as Arc<dyn executor::brokers::ProotBroker>,
        Arc::clone(&cp_broker) as Arc<dyn executor::brokers::ControlPlaneBroker>,
    ));

    let probe_service = Arc::new(HealthProbeServiceImpl::new(
        Arc::clone(&proot_broker) as Arc<dyn executor::brokers::ProotBroker>
    ));

    // E1: telemetry pushed by the Android app over `<dataDir>/agent.sock`.
    let device_state = executor::status_ipc::DeviceState::new();

    let capability_service = Arc::new(NodeCapabilityServiceImpl::new(
        config.node_id.clone(),
        config.layers_root.clone(),
        config.rootfs_base.clone(),
        device_state.clone(),
    ));

    let event_service = Arc::new(EventRecordingServiceImpl::new(
        Arc::clone(&cp_broker) as Arc<dyn executor::brokers::ControlPlaneBroker>,
        config.node_id.clone(),
    ));

    // ─── Orchestration services ───────────────────────────────────────────────

    let image_orch = Arc::new(ImageOrchestrationServiceImpl::new(
        Arc::clone(&pull_service) as Arc<dyn executor::services::foundation::ImagePullService>,
        Arc::clone(&unpack_service) as Arc<dyn executor::services::foundation::ImageUnpackService>,
        config.rootfs_base.clone(),
    ));

    // ─── Exposer (created early so its state can be shared with the lifecycle service) ───

    let exposer = VirtualKubeletExposer::with_paths(
        config.kubelet_addr,
        config.rootfs_base.clone(),
        config.layers_root.clone(),
    );
    exposer.state().set_node_name(config.node_id.clone()).await;
    let kubelet_state = exposer.state();
    kubelet_state.attach_device(device_state.clone());
    // G2: `/maintenance` patches `spec.unschedulable` through the same broker
    // the heartbeat uses.
    kubelet_state.attach_control_plane(
        Arc::clone(&cp_broker) as Arc<dyn executor::brokers::ControlPlaneBroker>
    );
    // A1: `kubectl exec` starts a helper proot process in the container rootfs.
    kubelet_state
        .attach_proot(Arc::clone(&proot_broker) as Arc<dyn ProotBroker>)
        .await;
    // Keep a handle for the telemetry socket; the original value is moved into
    // the workload lifecycle service below.
    let ipc_state = kubelet_state.clone();
    // D3: loopback ClusterIP proxies + per-pod /etc/hosts entries.
    tokio::spawn(executor::services::foundation::cluster_ip_proxy::run(
        Arc::clone(&cp_broker) as Arc<dyn executor::brokers::ControlPlaneBroker>,
        ipc_state.clone(),
    ));
    // B10: create local hostPath PVs for `droidnode-local` PVCs.
    {
        let cp = Arc::clone(&cp_broker) as Arc<dyn executor::brokers::ControlPlaneBroker>;
        let base = std::env::var("DROIDNODE_DATA_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("/tmp/droidnode"))
            .join("volumes")
            .join("pv");
        tokio::spawn(async move {
            loop {
                if let Err(e) = cp.reconcile_local_pvcs(&base).await {
                    tracing::warn!(error = %e, "B10: PVC reconcile failed");
                }
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
            }
        });
    }
    // E7: shared with the reconciler; `/drain` flips it when the phone is low on
    // battery so pods stop gracefully instead of the process being killed.
    let drain_flag = kubelet_state.drain_flag();

    let workload_lifecycle = Arc::new(WorkloadLifecycleServiceImpl::new(
        Arc::clone(&execution_service)
            as Arc<dyn executor::services::foundation::WorkloadExecutionService>,
        Arc::clone(&probe_service) as Arc<dyn executor::services::foundation::HealthProbeService>,
        kubelet_state,
    ));

    let reconciler = Arc::new(ReconciliationServiceImpl::new(
        Arc::clone(&cp_broker) as Arc<dyn executor::brokers::ControlPlaneBroker>,
        Arc::clone(&image_orch)
            as Arc<dyn executor::services::orchestration::ImageOrchestrationService>,
        Arc::clone(&workload_lifecycle)
            as Arc<dyn executor::services::orchestration::WorkloadLifecycleService>,
        Arc::clone(&event_service)
            as Arc<dyn executor::services::foundation::EventRecordingService>,
        config.node_id.clone(),
        drain_flag,
    ));

    let registration = Arc::new(NodeRegistrationServiceImpl::new(
        Arc::clone(&cp_broker) as Arc<dyn executor::brokers::ControlPlaneBroker>,
        Arc::clone(&capability_service)
            as Arc<dyn executor::services::foundation::NodeCapabilityService>,
    ));

    // ─── Proot smoke test (independent of Kubernetes) ────────────────────────
    // Runs at startup in the background; logs PASSED/FAILED so we know immediately
    // whether proot can actually run musl ELFs before any pod is scheduled.

    tokio::spawn({
        let proot = Arc::clone(&proot_broker) as Arc<dyn ProotBroker>;
        let orch = Arc::clone(&image_orch) as Arc<dyn ImageOrchestrationService>;
        async move { proot_smoke_test(proot, orch).await }
    });

    // ─── Orphan cleanup (B7) ─────────────────────────────────────────────────
    // A previous agent run may have been killed (force-stop, OOM, battery
    // manager) leaving proot containers behind. They still hold their ports and
    // would make the restarted containers fail with EADDRINUSE, so take them
    // down before starting anything.

    cleanup_orphan_proot(&config.proot_path);

    // ─── Launch all tasks ────────────────────────────────────────────────────
    // Every task is wrapped in a restart loop: a task ending (bind failure,
    // transient API error) must never take the whole agent down. Only SIGINT
    // stops the process.

    let reg_handle = {
        let reg = Arc::clone(&registration);
        tokio::spawn(async move {
            loop {
                if let Err(e) = reg.run().await {
                    error!(error = %e, "node registration task failed; retrying in 5s");
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        })
    };

    let reconcile_handle = {
        let rec = Arc::clone(&reconciler);
        tokio::spawn(async move {
            loop {
                if let Err(e) = rec.run_loop().await {
                    error!(error = %e, "reconciliation loop failed; retrying in 5s");
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        })
    };

    let kubelet_handle = {
        let exp = Arc::new(exposer);
        tokio::spawn(async move {
            loop {
                if let Err(e) = exp.serve().await {
                    error!(error = %e, "kubelet server failed; retrying in 5s");
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        })
    };

    // E1/E2: device telemetry + structured status socket. Supervised like the
    // other long-running tasks so a client bug never takes the agent down.
    let ipc_path = executor::status_ipc::default_socket_path();
    let ipc_apiserver = executor::status_ipc::apiserver_from_env();
    let ipc_handle = {
        let device = device_state.clone();
        let state = ipc_state.clone();
        let path = ipc_path.clone();
        let port = config.kubelet_addr.port();
        let apiserver = ipc_apiserver.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) = executor::status_ipc::serve(
                    path.clone(),
                    device.clone(),
                    state.clone(),
                    port,
                    apiserver.clone(),
                )
                .await
                {
                    error!(error = %e, "device status IPC failed; retrying in 5s");
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        })
    };

    // One signal is enough: Android's service manager sends SIGTERM, a terminal
    // sends SIGINT. Both must reach the deregistration path exactly once.
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => info!("SIGINT received — shutting down"),
        _ = sigterm.recv() => info!("SIGTERM received — shutting down"),
        // If a supervisor loop somehow exits, keep the remaining ones alive.
        _ = reg_handle => error!("registration supervisor exited"),
        _ = reconcile_handle => error!("reconcile supervisor exited"),
        _ = kubelet_handle => error!("kubelet supervisor exited"),
        _ = ipc_handle => error!("device status IPC supervisor exited"),
    }

    executor::status_ipc::cleanup(&ipc_path);

    // Deregister node on clean shutdown, then stop the background tasks.
    if let Err(e) = registration.deregister().await {
        error!(error = %e, "deregistration failed");
    }

    info!("droidnode agent stopped");
    Ok(())
}

// ─── Configuration ────────────────────────────────────────────────────────────

struct Config {
    node_id: String,
    proot_path: PathBuf,
    layers_root: PathBuf,
    rootfs_base: PathBuf,
    kubelet_addr: SocketAddr,
}

impl Config {
    fn from_env() -> Self {
        let base_dir = std::env::var("DROIDNODE_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| dirs_or_home().join(".droidnode"));

        let node_id = std::env::var("DROIDNODE_NODE_ID")
            .unwrap_or_else(|_| format!("droidnode-{}", hostname_or_uuid()));

        let proot_path = std::env::var("DROIDNODE_PROOT_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| base_dir.join("proot"));

        let layers_root = std::env::var("DROIDNODE_LAYERS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| base_dir.join("layers"));

        let rootfs_base = std::env::var("DROIDNODE_ROOTFS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| base_dir.join("rootfs"));

        let kubelet_port: u16 = std::env::var("DROIDNODE_KUBELET_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(10250);

        let kubelet_addr: SocketAddr = format!("0.0.0.0:{kubelet_port}").parse().unwrap();

        Self {
            node_id,
            proot_path,
            layers_root,
            rootfs_base,
            kubelet_addr,
        }
    }
}

// ─── Proot smoke test ─────────────────────────────────────────────────────────

async fn proot_smoke_test(
    proot: Arc<dyn ProotBroker>,
    image_orch: Arc<dyn ImageOrchestrationService>,
) {
    info!("proot smoke test: starting");

    let image_ref = match ImageRef::parse("alpine:latest") {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, "proot smoke test: bad image ref");
            return;
        }
    };

    let rootfs = match image_orch
        .prepare_image(&image_ref, executor::models::PullPolicy::IfNotPresent, None)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, "proot smoke test: failed to prepare Alpine rootfs");
            return;
        }
    };

    let cmd = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "echo proot-smoke-ok; uname -m".to_string(),
    ];

    let child = match proot.execute(&rootfs, &cmd, &[], &[], None).await {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, "proot smoke test: failed to spawn proot");
            return;
        }
    };

    match child.wait_with_output().await {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stdout.contains("proot-smoke-ok") {
                info!(stdout = %stdout.trim(), "proot smoke test PASSED — proot + musl ELF execution working");
            } else {
                error!(
                    exit_code = ?out.status.code(),
                    stdout = %stdout.trim(),
                    stderr = %stderr.trim(),
                    "proot smoke test FAILED"
                );
            }
        }
        Err(e) => error!(error = %e, "proot smoke test: wait_with_output failed"),
    }
}

/// Kill proot processes left over from a previous agent run.
///
/// When the agent is force-stopped or OOM-killed, its proot children are
/// reparented to init and keep running (still holding their ports). Any new
/// container for the same pod then dies with `bind: address already in use`.
/// At startup we own no containers we want to keep, so it is safe to take down
/// every process whose executable is our proot binary.
#[cfg(unix)]
fn cleanup_orphan_proot(proot_path: &PathBuf) {
    let target = std::fs::canonicalize(proot_path).unwrap_or_else(|_| proot_path.clone());
    let mut killed = 0usize;
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Ok(pid) = name.to_string_lossy().parse::<i32>() else {
            continue;
        };
        if pid <= 1 || pid == std::process::id() as i32 {
            continue;
        }
        let exe_link = PathBuf::from(format!("/proc/{pid}/exe"));
        let Ok(exe) = std::fs::read_link(&exe_link) else {
            continue;
        };
        if exe != target {
            continue;
        }
        // SAFETY: plain kill(2) on a pid we own.
        let rc = unsafe { libc::kill(pid, libc::SIGKILL) };
        if rc == 0 {
            killed += 1;
        }
    }
    if killed > 0 {
        warn!(killed, "killed orphan proot processes from a previous run");
    } else {
        info!("no orphan proot processes found");
    }
}

#[cfg(not(unix))]
fn cleanup_orphan_proot(_proot_path: &PathBuf) {}

fn dirs_or_home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

fn hostname_or_uuid() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}
