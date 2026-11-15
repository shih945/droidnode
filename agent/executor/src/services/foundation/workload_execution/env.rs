use super::*;

impl WorkloadExecutionServiceImpl {
    /// Build the command vector from the container spec and image config defaults.
    pub(super) fn resolve_command(
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

    pub(super) fn resolve_env(
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

    pub(super) fn resolve_mounts(
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
    pub(super) fn resolve_env_with_pod(
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
    pub(super) fn resource_value(
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
    pub(super) fn resource_limit(
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
    pub(super) fn apiserver_endpoint() -> (String, String) {
        static CACHE: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
        CACHE
            .get_or_init(|| {
                if let Some((host, port)) = crate::config::get().apiserver() {
                    return (host, port.to_string());
                }
                let Some(path) = crate::config::kubeconfig_path() else {
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
}
