use std::path::PathBuf;
use std::sync::OnceLock;

/// Process-wide configuration read once from the environment.
///
/// Modules must not call `std::env::var` themselves: everything goes through
/// here so defaults live in one place and the effective config is greppable.
/// `node-agent` remains the only place that *builds* the agent config; this is
/// the runtime view shared by the executor.
pub struct RuntimeConfig {
    data_dir: PathBuf,
    node_id: Option<String>,
    node_ip: Option<String>,
    node_iface: Option<String>,
    kubelet_port: Option<u16>,
    max_pods: u32,
    proot_path: Option<PathBuf>,
    spdy_helper: Option<String>,
    tmpdir: String,
    kubelet_client_ca: Option<PathBuf>,
    apiserver: Option<(String, u16)>,
    cluster_dns: String,
    proot_smoke: bool,
    layers_dir: Option<PathBuf>,
    rootfs_dir: Option<PathBuf>,
}

static CONFIG: OnceLock<RuntimeConfig> = OnceLock::new();

pub fn get() -> &'static RuntimeConfig {
    CONFIG.get_or_init(RuntimeConfig::from_env)
}

/// Trimmed, non-empty environment value. Used by the file/env config readers.
pub fn env_value(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn kubeconfig_path() -> Option<PathBuf> {
    env_value("KUBECONFIG").map(PathBuf::from)
}

fn default_data_dir() -> PathBuf {
    if let Some(dir) = env_value("DROIDNODE_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let home = env_value("HOME")
        .or_else(|| env_value("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".droidnode")
}

impl RuntimeConfig {
    fn from_env() -> Self {
        Self {
            data_dir: default_data_dir(),
            node_id: env_value("DROIDNODE_NODE_ID"),
            node_ip: env_value("DROIDNODE_NODE_IP"),
            node_iface: env_value("DROIDNODE_NODE_IFACE"),
            kubelet_port: env_value("DROIDNODE_KUBELET_PORT").and_then(|v| v.parse().ok()),
            max_pods: env_value("DROIDNODE_MAX_PODS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(20),
            proot_path: env_value("DROIDNODE_PROOT_PATH").map(PathBuf::from),
            spdy_helper: env_value("DROIDNODE_SPDY_HELPER"),
            tmpdir: env_value("TMPDIR")
                .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().to_string()),
            kubelet_client_ca: env_value("DROIDNODE_KUBELET_CLIENT_CA").map(PathBuf::from),
            apiserver: env_value("DROIDNODE_APISERVER_HOST").map(|host| {
                let port = env_value("DROIDNODE_APISERVER_PORT")
                    .and_then(|v| v.parse::<u16>().ok())
                    .unwrap_or(6443);
                (host, port)
            }),
            cluster_dns: env_value("DROIDNODE_CLUSTER_DNS").unwrap_or_else(|| "10.43.0.10".into()),
            proot_smoke: env_value("DROIDNODE_PROOT_SMOKE")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            layers_dir: env_value("DROIDNODE_LAYERS_DIR").map(PathBuf::from),
            rootfs_dir: env_value("DROIDNODE_ROOTFS_DIR").map(PathBuf::from),
        }
    }

    pub fn data_dir(&self) -> PathBuf {
        self.data_dir.clone()
    }

    pub fn node_id(&self) -> Option<&str> {
        self.node_id.as_deref()
    }

    pub fn layers_dir(&self) -> Option<PathBuf> {
        self.layers_dir.clone()
    }

    pub fn rootfs_dir(&self) -> Option<PathBuf> {
        self.rootfs_dir.clone()
    }

    pub fn node_ip(&self) -> Option<&str> {
        self.node_ip.as_deref()
    }

    pub fn node_iface(&self) -> Option<&str> {
        self.node_iface.as_deref()
    }

    pub fn kubelet_port(&self) -> Option<u16> {
        self.kubelet_port
    }

    pub fn max_pods(&self) -> u32 {
        self.max_pods
    }

    pub fn proot_path(&self) -> Option<PathBuf> {
        self.proot_path.clone()
    }

    pub fn spdy_helper(&self) -> Option<&str> {
        self.spdy_helper.as_deref()
    }

    pub fn tmpdir(&self) -> &str {
        &self.tmpdir
    }

    pub fn kubelet_client_ca(&self) -> Option<PathBuf> {
        self.kubelet_client_ca.clone()
    }

    pub fn apiserver(&self) -> Option<(String, u16)> {
        self.apiserver.clone()
    }

    pub fn cluster_dns(&self) -> &str {
        &self.cluster_dns
    }

    /// Opt-in proot smoke test at startup (pulls Alpine into the image store).
    pub fn proot_smoke(&self) -> bool {
        self.proot_smoke
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_value_ignores_blank() {
        std::env::set_var("DROIDNODE_CONFIG_TEST", "  ");
        assert_eq!(env_value("DROIDNODE_CONFIG_TEST"), None);
        std::env::set_var("DROIDNODE_CONFIG_TEST", " value ");
        assert_eq!(
            env_value("DROIDNODE_CONFIG_TEST"),
            Some("value".to_string())
        );
        std::env::remove_var("DROIDNODE_CONFIG_TEST");
    }
}
