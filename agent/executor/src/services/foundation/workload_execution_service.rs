use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::process::Child;
use tracing::{info, instrument};

use crate::brokers::ProotBroker;
use crate::error::{DroidError, Result};
use crate::models::{
    parse_cpu_millis, parse_memory_bytes, ContainerConfig, ContainerLimits, Mount, Pod,
};

// ─── Trait ────────────────────────────────────────────────────────────────────

#[async_trait]
pub trait WorkloadExecutionService: Send + Sync {
    /// Launch the first container in `pod` using the merged rootfs at `rootfs_path`.
    /// Kept for callers that only deal with single-container pods.
    async fn execute_workload(&self, pod: &Pod, rootfs_path: &Path) -> Result<Child>;

    /// Launch one specific container of a multi-container pod (B1). Each container
    /// has its own rootfs because each image unpacks separately.
    async fn execute_container(
        &self,
        pod: &Pod,
        container: &k8s_openapi::api::core::v1::Container,
        rootfs_path: &Path,
    ) -> Result<Child>;
}

// ─── Implementation ───────────────────────────────────────────────────────────

pub struct WorkloadExecutionServiceImpl {
    proot_broker: Arc<dyn ProotBroker>,
    cp_broker: Arc<dyn crate::brokers::ControlPlaneBroker>,
}

#[path = "workload_execution/env.rs"]
mod env;
#[path = "workload_execution/volumes.rs"]
mod volumes;

impl WorkloadExecutionServiceImpl {
    pub fn new(
        proot_broker: Arc<dyn ProotBroker>,
        cp_broker: Arc<dyn crate::brokers::ControlPlaneBroker>,
    ) -> Self {
        Self {
            proot_broker,
            cp_broker,
        }
    }
}

#[async_trait]
impl WorkloadExecutionService for WorkloadExecutionServiceImpl {
    #[instrument(skip(self, pod), fields(
        pod = pod.metadata.name.as_deref().unwrap_or("?"),
        rootfs = %rootfs_path.display()
    ))]
    async fn execute_workload(&self, pod: &Pod, rootfs_path: &Path) -> Result<Child> {
        let container = pod
            .spec
            .as_ref()
            .and_then(|s| s.containers.first())
            .ok_or_else(|| DroidError::Workload("pod has no containers".into()))?;
        self.execute_container(pod, container, rootfs_path).await
    }

    #[instrument(skip(self, pod, container), fields(
        pod = pod.metadata.name.as_deref().unwrap_or("?"),
        container = %container.name,
        rootfs = %rootfs_path.display()
    ))]
    async fn execute_container(
        &self,
        pod: &Pod,
        container: &k8s_openapi::api::core::v1::Container,
        rootfs_path: &Path,
    ) -> Result<Child> {
        // Load the image's ContainerConfig written by ImageOrchestrationService.
        // This gives us ENTRYPOINT/CMD/ENV from the image when the pod spec omits them.
        let image_config: Option<ContainerConfig> = {
            let cfg_path = rootfs_path.join(".droidnode_image_config.json");
            match tokio::fs::read(&cfg_path).await {
                Ok(bytes) => serde_json::from_slice(&bytes).ok(),
                Err(_) => None,
            }
        };

        let command = Self::resolve_command(container, image_config.as_ref());
        if command.is_empty() {
            return Err(DroidError::Workload(format!(
                "pod {}: container {} has no command or entrypoint defined",
                pod.metadata.name.as_deref().unwrap_or("?"),
                container.name
            )));
        }

        let env = Self::resolve_env_with_pod(container, image_config.as_ref(), pod);
        // B13: envFrom (configMapRef / secretRef) is merged after explicit env so
        // explicit entries win, matching kubelet behaviour.
        let mut env = env;
        if let Some(from_list) = container.env_from.as_ref() {
            let namespace = pod
                .metadata
                .namespace
                .clone()
                .unwrap_or_else(|| "default".into());
            for from in from_list {
                if let Some(cm) = &from.config_map_ref {
                    let name = cm.name.clone().unwrap_or_default();
                    if let Ok(data) = self.cp_broker.get_config_map(&namespace, &name).await {
                        for (k, v) in data {
                            if !env.iter().any(|(existing, _)| existing == &k) {
                                env.push((k, v));
                            }
                        }
                    }
                } else if let Some(sec) = &from.secret_ref {
                    let name = sec.name.clone().unwrap_or_default();
                    if let Ok(data) = self.cp_broker.get_secret(&namespace, &name).await {
                        for (k, v) in data {
                            if !env.iter().any(|(existing, _)| existing == &k) {
                                env.push((k, String::from_utf8_lossy(&v).into_owned()));
                            }
                        }
                    }
                }
            }
        }

        // B9: volumes -> host dirs, then map the container's volumeMounts.
        let volumes = self.materialize_volumes(pod, rootfs_path).await?;
        let mounts = Self::resolve_mounts(container, &volumes);

        // B14: service-account token + CA + namespace for API clients.
        self.materialize_service_account(rootfs_path).await;

        // B15/B16: write /etc/resolv.conf and /etc/hosts into the rootfs.
        Self::prepare_rootfs_files(pod, rootfs_path).await;

        // Working directory: container spec → image WORKDIR → default "/"
        let cwd_owned = container
            .working_dir
            .clone()
            .or_else(|| image_config.as_ref().and_then(|c| c.working_dir.clone()))
            .filter(|s| !s.is_empty());
        let cwd = cwd_owned.as_deref();

        info!(
            pod = pod.metadata.name.as_deref().unwrap_or("?"),
            container = %container.name,
            command = ?command,
            cwd = cwd.unwrap_or("/"),
            "executing container"
        );

        // B8: Kubernetes containers run as uid 0 unless securityContext says
        // otherwise; proot only has a binary "fake root" switch, so honour it
        // whenever the pod does not explicitly ask for a different user.
        let fake_root = {
            let sc = container.security_context.as_ref();
            let non_root = sc.and_then(|s| s.run_as_non_root).unwrap_or(false);
            let uid = sc.and_then(|s| s.run_as_user);
            !non_root && uid.unwrap_or(0) == 0
        };

        // B26: soft limits (RLIMIT_AS for memory, nice for fractional CPU).
        // Only `limits` count; `requests` are scheduling hints, not caps.
        let limits = ContainerLimits {
            memory_bytes: Self::resource_limit(container, "memory")
                .and_then(|v| parse_memory_bytes(&v)),
            cpu_millis: Self::resource_limit(container, "cpu").and_then(|v| parse_cpu_millis(&v)),
        };

        self.proot_broker
            .execute_as(
                rootfs_path,
                &command,
                &env,
                &mounts,
                cwd,
                fake_root,
                &limits,
            )
            .await
    }
}

/// Replace a volume directory with the configMap/secret files.
pub(crate) async fn write_config_dir(
    dir: &Path,
    data: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let _ = tokio::fs::remove_dir_all(dir).await;
    tokio::fs::create_dir_all(dir).await?;
    for (k, v) in data {
        let _ = tokio::fs::write(dir.join(k), v).await;
    }
    Ok(())
}

pub(crate) async fn write_secret_dir(
    dir: &Path,
    data: &std::collections::BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    let _ = tokio::fs::remove_dir_all(dir).await;
    tokio::fs::create_dir_all(dir).await?;
    for (k, v) in data {
        let _ = tokio::fs::write(dir.join(k), v).await;
    }
    Ok(())
}

/// hostPath sources must exist; otherwise the pod starts with a surprise.
pub(crate) fn resolve_host_path(name: &str, raw: &str) -> Result<PathBuf> {
    let path = PathBuf::from(raw);
    if !path.exists() {
        return Err(DroidError::Workload(format!(
            "hostPath volume \"{name}\" source {} does not exist",
            path.display()
        )));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn scratch() -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "droidnode-vol-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn config_map_files_replace_stale_content() {
        let dir = scratch().join("config");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stale"), b"old").unwrap();

        let mut data = BTreeMap::new();
        data.insert("app.conf".to_string(), "port=8080".to_string());
        data.insert("mode".to_string(), "prod".to_string());
        write_config_dir(&dir, &data).await.unwrap();

        assert!(!dir.join("stale").exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("app.conf")).unwrap(),
            "port=8080"
        );
        assert_eq!(std::fs::read_to_string(dir.join("mode")).unwrap(), "prod");
    }

    #[tokio::test]
    async fn secret_files_keep_binary_bytes() {
        let dir = scratch().join("secret");
        let mut data = BTreeMap::new();
        data.insert("tls.key".to_string(), vec![0u8, 1, 2, 255]);
        write_secret_dir(&dir, &data).await.unwrap();
        assert_eq!(
            std::fs::read(dir.join("tls.key")).unwrap(),
            vec![0u8, 1, 2, 255]
        );
    }

    #[test]
    fn host_path_must_exist() {
        let dir = scratch();
        let path = resolve_host_path("data", dir.to_str().unwrap()).unwrap();
        assert_eq!(path, dir);
        let err = resolve_host_path("data", dir.join("missing").to_str().unwrap()).unwrap_err();
        assert!(matches!(err, DroidError::Workload(_)));
    }
}
