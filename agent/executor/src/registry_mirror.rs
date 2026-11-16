//! Registry mirror rewriting.
//!
//! The phone often cannot reach `registry-1.docker.io` directly, so an image pull
//! can be rewritten to a mirror host *before* the OCI broker builds URLs; the
//! rest of the code (manifest / blob / token requests) is unchanged because the
//! mirror implements the standard `/v2/...` API.
//!
//! **Nothing is compiled in.** Mirror hosts are per-deployment and frequently
//! private/paid, so they must never be committed to the repository. Configure
//! them (empty or missing = no rewrite, pull the upstream directly):
//!
//!   environment variables (wins over the file):
//!     DROIDNODE_REGISTRY_MIRROR       docker.io / registry-1.docker.io / index.docker.io
//!     DROIDNODE_REGISTRY_MIRROR_K8S   registry.k8s.io
//!     DROIDNODE_REGISTRY_MIRROR_QUAY  quay.io
//!
//!   optional on-device file (same keys, `KEY=VALUE` lines):
//!     $DROIDNODE_DATA_DIR/registry-mirrors.env
//!
//! Template: `deploy/registry-mirrors.env.example`. The real file is gitignored;
//! push it with `adb push ... /data/local/tmp/` + `run-as com.droidnode cp`.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::models::ImageRef;

/// Docker Hub is addressed by several host names depending on how the image
/// reference was written.
const DOCKER_HOSTS: [&str; 3] = ["docker.io", "registry-1.docker.io", "index.docker.io"];

const ENV_DOCKER: &str = "DROIDNODE_REGISTRY_MIRROR";
const ENV_K8S: &str = "DROIDNODE_REGISTRY_MIRROR_K8S";
const ENV_QUAY: &str = "DROIDNODE_REGISTRY_MIRROR_QUAY";

const ENV_FILE_NAME: &str = "registry-mirrors.env";

/// Parse `KEY=VALUE` lines from the optional on-device config file.
/// Missing file, unreadable file, or bad lines are silently ignored.
fn file_values() -> HashMap<String, String> {
    let mut out = HashMap::new();
    let path = crate::config::get().config_dir().join(ENV_FILE_NAME);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return out;
    };
    for raw in text.lines() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let mut value = value.trim();
        if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            value = &value[1..value.len() - 1];
        }
        if key.is_empty() || value.is_empty() {
            continue;
        }
        out.insert(key.to_string(), value.to_string());
    }
    out
}

/// Environment wins; the file is the fallback; empty string = not configured.
fn resolve(key: &str, file: &HashMap<String, String>) -> String {
    if let Some(value) = crate::config::env_value(key) {
        return value;
    }
    file.get(key).cloned().unwrap_or_default()
}

/// upstream registry host -> mirror host, built from whatever is configured.
fn build_table<F>(get: F) -> HashMap<String, String>
where
    F: Fn(&str) -> String,
{
    let docker = get(ENV_DOCKER);
    let k8s = get(ENV_K8S);
    let quay = get(ENV_QUAY);

    let mut map = HashMap::new();
    if !docker.is_empty() {
        for host in DOCKER_HOSTS {
            map.insert(host.to_string(), docker.clone());
        }
    }
    if !k8s.is_empty() {
        map.insert("registry.k8s.io".to_string(), k8s);
    }
    if !quay.is_empty() {
        map.insert("quay.io".to_string(), quay);
    }
    map
}

/// Runtime mirror table (env + on-device file), resolved once per process.
fn table() -> &'static HashMap<String, String> {
    static TABLE: OnceLock<HashMap<String, String>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let file = file_values();
        build_table(|key| resolve(key, &file))
    })
}

/// True when at least one mirror is configured.
pub fn is_configured() -> bool {
    !table().is_empty()
}

/// Log the resolved table once at startup.
///
/// The settings page reads this line to show what the agent is actually using:
/// a pull-time `registry mirror applied` line only appears after an image pull,
/// so without this the UI would have to guess.
pub fn log_summary() {
    let mirrors = table();
    let docker = mirrors.get("docker.io").cloned().unwrap_or_default();
    let k8s = mirrors.get("registry.k8s.io").cloned().unwrap_or_default();
    let quay = mirrors.get("quay.io").cloned().unwrap_or_default();
    if docker.is_empty() && k8s.is_empty() && quay.is_empty() {
        tracing::info!("registry mirrors: none (pulling upstream directly)");
    } else {
        tracing::info!(
            docker = %docker,
            k8s = %k8s,
            quay = %quay,
            "registry mirrors configured"
        );
    }
}

fn rewrite(image: &mut ImageRef, mirrors: &HashMap<String, String>) {
    let Some(mirror) = mirrors.get(&image.registry) else {
        return;
    };
    if mirror == &image.registry {
        return;
    }
    tracing::info!(
        from = %image.registry,
        to = %mirror,
        image = %image.repository,
        reference = %image.reference,
        "registry mirror applied"
    );
    image.registry = mirror.clone();
}

/// Rewrite `image.registry` in place if it has a configured mirror.
pub fn apply(image: &mut ImageRef) {
    rewrite(image, table());
}

/// Convenience wrapper used by the parsers.
pub fn apply_to(mut image: ImageRef) -> ImageRef {
    apply(&mut image);
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(registry: &str) -> ImageRef {
        ImageRef {
            registry: registry.to_string(),
            repository: "library/alpine".to_string(),
            reference: "latest".to_string(),
        }
    }

    fn table_with(docker: &str, k8s: &str, quay: &str) -> HashMap<String, String> {
        let docker = docker.to_string();
        let k8s = k8s.to_string();
        let quay = quay.to_string();
        build_table(|key| match key {
            ENV_DOCKER => docker.clone(),
            ENV_K8S => k8s.clone(),
            ENV_QUAY => quay.clone(),
            _ => String::new(),
        })
    }

    #[test]
    fn configured_mirror_rewrites_docker_hosts() {
        let mirrors = table_with("mirror.invalid", "", "");
        for host in DOCKER_HOSTS {
            let mut i = img(host);
            rewrite(&mut i, &mirrors);
            assert_eq!(i.registry, "mirror.invalid", "host {host} not rewritten");
        }
    }

    #[test]
    fn no_config_keeps_upstream() {
        let mirrors = table_with("", "", "");
        assert!(mirrors.is_empty());
        let mut i = img("registry-1.docker.io");
        rewrite(&mut i, &mirrors);
        assert_eq!(i.registry, "registry-1.docker.io");
    }

    #[test]
    fn unknown_registry_is_untouched() {
        let mirrors = table_with("mirror.invalid", "", "");
        let mut i = img("ghcr.io");
        rewrite(&mut i, &mirrors);
        assert_eq!(i.registry, "ghcr.io");
    }

    #[test]
    fn already_mirrored_is_idempotent() {
        let mirrors = table_with("mirror.invalid", "", "");
        let mut i = img("registry-1.docker.io");
        rewrite(&mut i, &mirrors);
        let once = i.registry.clone();
        rewrite(&mut i, &mirrors);
        assert_eq!(i.registry, once);
    }
}
