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

    /// B9: turn `pod.spec.volumes` into host directories and return
    /// `volume name -> host path`.
    ///
    /// Directory lives under `<rootfs parent>/volumes/<uid>/<name>` and is
    /// recreated on every start (emptyDir semantics).
    async fn materialize_volumes(
        &self,
        pod: &Pod,
        rootfs: &Path,
    ) -> Result<std::collections::HashMap<String, PathBuf>> {
        let mut out = std::collections::HashMap::new();
        let Some(spec) = pod.spec.as_ref() else {
            return Ok(out);
        };
        let uid = pod.metadata.uid.clone().unwrap_or_else(|| "unknown".into());
        // Volumes belong to the POD, not to a container. Since every container now
        // has its own rootfs copy (B17) at <files>/pods/<uid>/<container>/rootfs,
        // deriving the directory from `rootfs.parent()` would give each container
        // a private emptyDir. Anchor at the "pods" component instead so all
        // containers of the pod (including init containers) share it.
        let base = {
            let mut found = None;
            for anc in rootfs.ancestors() {
                if anc.file_name().and_then(|n| n.to_str()) == Some("pods") {
                    found = Some(anc.join(&uid).join("volumes"));
                    break;
                }
            }
            found.unwrap_or_else(|| {
                rootfs
                    .parent()
                    .map(|p| p.join("volumes").join(&uid))
                    .unwrap_or_else(|| rootfs.join("volumes").join(&uid))
            })
        };
        let namespace = pod
            .metadata
            .namespace
            .clone()
            .unwrap_or_else(|| "default".into());

        for volume in spec.volumes.iter().flatten() {
            let name = volume.name.clone();
            let dir = base.join(&name);

            if volume.empty_dir.is_some() {
                // NOTE: never wipe here. `materialize_volumes` runs once per
                // container, so removing the directory would erase what an init
                // container just wrote (and what the main container expects).
                // A fresh pod gets a fresh <uid> directory anyway.
                tokio::fs::create_dir_all(&dir).await?;
                out.insert(name, dir);
            } else if let Some(hp) = volume.host_path.as_ref() {
                // B12: a missing mount source is fatal instead of silently skipped.
                let path = resolve_host_path(&name, &hp.path)?;
                out.insert(name, path);
            } else if let Some(pvc) = volume.persistent_volume_claim.as_ref() {
                // B10: resolve the Bound PVC through its PV's hostPath.
                let claim = pvc.claim_name.clone();
                match self.cp_broker.pvc_host_path(&namespace, &claim).await {
                    Ok(Some(path)) => {
                        tokio::fs::create_dir_all(&path).await?;
                        out.insert(name, path);
                    }
                    Ok(None) => {
                        return Err(DroidError::Workload(format!(
                            "PVC {namespace}/{claim} is not Bound to a hostPath PV"
                        )))
                    }
                    Err(e) => {
                        return Err(DroidError::Workload(format!(
                            "PVC {namespace}/{claim}: {e}"
                        )))
                    }
                }
            } else if let Some(cm) = volume.config_map.as_ref() {
                let cm_name = cm.name.clone().unwrap_or_default();
                let data = self
                    .cp_broker
                    .get_config_map(&namespace, &cm_name)
                    .await
                    .map_err(|e| DroidError::Workload(format!("configMap \"{cm_name}\": {e}")))?;
                write_config_dir(&dir, &data).await?;
                for item in cm.items.iter().flatten() {
                    if let Some(v) = data.get(&item.key) {
                        let target = dir.join(&item.path);
                        if let Some(parent) = target.parent() {
                            let _ = tokio::fs::create_dir_all(parent).await;
                        }
                        let _ = tokio::fs::write(target, v).await;
                    }
                }
                out.insert(name, dir);
            } else if let Some(sec) = volume.secret.as_ref() {
                let sec_name = sec.secret_name.clone().unwrap_or_default();
                let data = self
                    .cp_broker
                    .get_secret(&namespace, &sec_name)
                    .await
                    .map_err(|e| DroidError::Workload(format!("secret \"{sec_name}\": {e}")))?;
                write_secret_dir(&dir, &data).await?;
                out.insert(name, dir);
            } else {
                tracing::debug!(volume = %name, "unsupported volume source - ignored");
            }
        }
        Ok(out)
    }

    /// B14: drop a service-account token the pods can use, reusing the agent's
    /// own credentials (the app cannot mint per-pod tokens without more RBAC).
    async fn materialize_service_account(&self, rootfs: &Path) {
        let Ok(creds) = self.cp_broker.agent_credentials().await else {
            return;
        };
        let dir = rootfs.join("var/run/secrets/kubernetes.io/serviceaccount");
        if tokio::fs::create_dir_all(&dir).await.is_err() {
            return;
        }
        let _ = tokio::fs::write(dir.join("token"), creds.token).await;
        let _ = tokio::fs::write(dir.join("namespace"), creds.namespace).await;
        if !creds.ca_pem.is_empty() {
            let _ = tokio::fs::write(dir.join("ca.crt"), creds.ca_pem).await;
        }
    }

    /// Build the command vector from the container spec and image config defaults.
    fn resolve_command(
        container: &k8s_openapi::api::core::v1::Container,
        image_config: Option<&ContainerConfig>,
    ) -> Vec<String> {
        // Kubernetes semantics:
        // - `command` overrides image ENTRYPOINT; if it is set, image CMD is
        //   NOT appended (only `args` is used).
        // - without `command`, image ENTRYPOINT + (`args` or image CMD).
        let entrypoint: Vec<String> = container
            .command
            .clone()
            .or_else(|| image_config.and_then(|c| c.entrypoint.clone()))
            .unwrap_or_default();

        let args: Vec<String> = match (&container.command, &container.args) {
            (Some(_), Some(args)) => args.clone(),
            (Some(_), None) => Vec::new(),
            (None, Some(args)) => args.clone(),
            (None, None) => image_config.and_then(|c| c.cmd.clone()).unwrap_or_default(),
        };

        entrypoint.into_iter().chain(args).collect()
    }

    fn resolve_env(
        container: &k8s_openapi::api::core::v1::Container,
        image_config: Option<&ContainerConfig>,
    ) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = image_config
            .and_then(|c| c.env.as_ref())
            .map(|vars| {
                vars.iter()
                    .filter_map(|e| {
                        let mut parts = e.splitn(2, '=');
                        let k = parts.next()?.to_string();
                        let v = parts.next().unwrap_or("").to_string();
                        Some((k, v))
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Pod-level env vars override image defaults
        if let Some(pod_env) = &container.env {
            for e in pod_env {
                if let Some(val) = &e.value {
                    env.push((e.name.clone(), val.clone()));
                }
            }
        }

        env
    }

    fn resolve_mounts(
        container: &k8s_openapi::api::core::v1::Container,
        volumes: &std::collections::HashMap<String, PathBuf>,
    ) -> Vec<Mount> {
        container
            .volume_mounts
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .filter_map(|vm| {
                let source = volumes.get(&vm.name)?.clone();
                Some(Mount {
                    source,
                    target: PathBuf::from(&vm.mount_path),
                    read_only: vm.read_only.unwrap_or(false),
                })
            })
            .collect()
    }

    /// B13: standard env vars + `valueFrom` (`fieldRef` / `resourceFieldRef`).
    ///
    /// `envFrom.configMapRef/secretRef` still needs API lookups and is not
    /// implemented; everything else a normal manifest uses works.
    fn resolve_env_with_pod(
        container: &k8s_openapi::api::core::v1::Container,
        image_config: Option<&ContainerConfig>,
        pod: &Pod,
    ) -> Vec<(String, String)> {
        // Start from the image's ENV, then overlay the pod spec.
        let mut env = Self::resolve_env(container, image_config);

        let pod_name = pod.metadata.name.clone().unwrap_or_default();
        let namespace = pod
            .metadata
            .namespace
            .clone()
            .unwrap_or_else(|| "default".to_string());
        let pod_ip = pod
            .status
            .as_ref()
            .and_then(|s| s.pod_ip.clone())
            .filter(|ip| !ip.is_empty())
            // Our pods share the node's network namespace, and at container start
            // the pod status has not been reported yet - fall back to the node IP.
            .unwrap_or_else(crate::net_util::primary_ipv4);
        let host_ip = pod
            .status
            .as_ref()
            .and_then(|s| s.host_ip.clone())
            .unwrap_or_else(|| pod_ip.clone());
        let node_name = pod
            .spec
            .as_ref()
            .and_then(|s| s.node_name.clone())
            .unwrap_or_default();
        let api_host = Self::apiserver_endpoint();
        let api_host_name = api_host.0;
        let api_port = api_host.1;

        // What a real kubelet injects into every container.
        let builtin = [
            ("KUBERNETES_SERVICE_HOST", api_host_name.clone()),
            ("KUBERNETES_SERVICE_PORT", api_port.clone()),
            ("KUBERNETES_PORT_443_TCP", api_port),
            ("HOSTNAME", pod_name.clone()),
            ("POD_NAME", pod_name.clone()),
            ("POD_NAMESPACE", namespace.clone()),
            ("POD_IP", pod_ip.clone()),
            ("NODE_NAME", node_name.clone()),
            ("HOME", "/root".to_string()),
        ];
        for (k, v) in builtin {
            if !v.is_empty() {
                env.retain(|(name, _)| name != k);
                env.push((k.to_string(), v));
            }
        }

        // valueFrom: fieldRef / resourceFieldRef
        for item in container.env.iter().flatten() {
            let Some(from) = &item.value_from else {
                continue;
            };
            let value = if let Some(field) = &from.field_ref {
                match field.field_path.as_str() {
                    "metadata.name" => pod_name.clone(),
                    "metadata.namespace" => namespace.clone(),
                    "metadata.uid" => pod.metadata.uid.clone().unwrap_or_default(),
                    "spec.nodeName" => node_name.clone(),
                    "status.podIP" | "status.podIPs" => pod_ip.clone(),
                    "status.hostIP" => host_ip.clone(),
                    other => {
                        tracing::debug!(field = other, "unsupported fieldRef");
                        String::new()
                    }
                }
            } else if let Some(res) = &from.resource_field_ref {
                // We do not enforce limits, so report the declared limit as both
                // limit and request - that is what apps like Java read.
                let source = res
                    .container_name
                    .as_deref()
                    .filter(|n| *n == container.name)
                    .map(|_| res)
                    .or(Some(res));
                let _ = source;
                let limit = Self::resource_value(container, &res.resource);
                limit.unwrap_or_default()
            } else {
                String::new()
            };
            if !value.is_empty() {
                env.retain(|(name, _)| name != &item.name);
                env.push((item.name.clone(), value));
            }
        }

        env
    }

    /// Read `resources.limits.<name>` (e.g. "cpu", "memory") as a string.
    fn resource_value(
        container: &k8s_openapi::api::core::v1::Container,
        resource: &str,
    ) -> Option<String> {
        // Some manifests spell it "limits.memory", the API keeps just "memory".
        let key = resource
            .rsplit('.')
            .next()
            .filter(|k| !k.is_empty())
            .unwrap_or(resource);
        let resources = container.resources.as_ref()?;
        resources
            .limits
            .as_ref()
            .and_then(|l| l.get(key))
            .or_else(|| resources.requests.as_ref().and_then(|r| r.get(key)))
            .map(|q| q.0.clone())
    }

    /// Read `resources.limits.<name>` only. Unlike [`Self::resource_value`] this
    /// does not fall back to `requests`, because a request is not a cap.
    fn resource_limit(
        container: &k8s_openapi::api::core::v1::Container,
        resource: &str,
    ) -> Option<String> {
        let resources = container.resources.as_ref()?;
        resources
            .limits
            .as_ref()
            .and_then(|l| l.get(resource))
            .map(|q| q.0.clone())
    }

    /// apiserver host:port for the `KUBERNETES_SERVICE_*` variables.
    ///
    /// Taken from the agent's kubeconfig (the app does not pass it via env), so
    /// clients that talk to the API keep working inside containers.
    fn apiserver_endpoint() -> (String, String) {
        static CACHE: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
        CACHE
            .get_or_init(|| {
                if let Ok(host) = std::env::var("DROIDNODE_APISERVER_HOST") {
                    let port = std::env::var("DROIDNODE_APISERVER_PORT")
                        .unwrap_or_else(|_| "6443".to_string());
                    return (host, port);
                }
                let Ok(path) = std::env::var("KUBECONFIG") else {
                    return (String::new(), "443".to_string());
                };
                let Ok(text) = std::fs::read_to_string(path) else {
                    return (String::new(), "443".to_string());
                };
                for line in text.lines() {
                    let t = line.trim();
                    if let Some(rest) = t.strip_prefix("server:") {
                        let url = rest.trim().trim_matches('"');
                        if let Some(after) = url.split("://").nth(1) {
                            let after = after.trim_end_matches('/');
                            let (host, port) = match after.rsplit_once(':') {
                                Some((h, p)) => (h.to_string(), p.to_string()),
                                None => (after.to_string(), "443".to_string()),
                            };
                            return (host, port);
                        }
                    }
                }
                (String::new(), "443".to_string())
            })
            .clone()
    }

    /// B15/B16: host aliases and DNS config inside the container rootfs.
    ///
    /// These are written into the image rootfs, which is shared by every pod
    /// using that image - acceptable until per-pod writable layers (B17).
    async fn prepare_rootfs_files(pod: &Pod, rootfs: &Path) {
        let etc = rootfs.join("etc");
        if tokio::fs::create_dir_all(&etc).await.is_err() {
            return;
        }

        // resolv.conf: cluster DNS + the standard search path.
        let dns = std::env::var("DROIDNODE_CLUSTER_DNS").unwrap_or_else(|_| "10.43.0.10".into());
        let namespace = pod
            .metadata
            .namespace
            .clone()
            .unwrap_or_else(|| "default".to_string());
        let resolv = format!(
            "nameserver {dns}\nsearch {namespace}.svc.cluster.local svc.cluster.local cluster.local\noptions ndots:5\n"
        );
        let _ = tokio::fs::write(etc.join("resolv.conf"), resolv).await;

        // /etc/hosts: keep the base content, append hostAliases.
        let hosts_path = etc.join("hosts");
        let mut hosts = tokio::fs::read_to_string(&hosts_path)
            .await
            .unwrap_or_else(|_| "127.0.0.1 localhost\n::1 localhost\n".to_string());
        if let Some(aliases) = pod.spec.as_ref().and_then(|s| s.host_aliases.as_ref()) {
            for alias in aliases {
                let names = alias.hostnames.clone().unwrap_or_default().join(" ");
                if let Some(ip) = alias.ip.clone() {
                    if !names.is_empty() {
                        hosts.push_str(&format!("{ip} {names}\n"));
                    }
                }
            }
        }
        let _ = tokio::fs::write(&hosts_path, hosts).await;
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
