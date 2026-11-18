//! `imagePullSecrets`.
//!
//! A Pod references one or more Secrets of type `kubernetes.io/dockerconfigjson`
//! (or the legacy `kubernetes.io/dockercfg`). We fetch the Secret payload and
//! resolve the credentials that match the image's **upstream** registry (not the
//! mirror host the pull actually goes to).
//!
//! The matching rules mirror what kubelet/containerd effectively do:
//!   - keys are registry hosts, sometimes written as `https://host/v1/`
//!   - Docker Hub appears under several aliases (`docker.io`,
//!     `index.docker.io`, `registry-1.docker.io`, `registry.hub.docker.com`)
//!   - ports are part of the host (`registry.local:5000`)

use std::collections::HashMap;

use base64::Engine;
use serde::Deserialize;

use crate::models::{ImageRef, RegistryAuth};

#[derive(Debug, Deserialize, Default)]
struct DockerConfigFile {
    #[serde(default)]
    auths: HashMap<String, DockerAuthEntry>,
}

#[derive(Debug, Deserialize, Default)]
struct DockerAuthEntry {
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
    /// base64("username:password")
    #[serde(default)]
    auth: Option<String>,
}

impl DockerAuthEntry {
    fn to_auth(&self) -> Option<RegistryAuth> {
        if let (Some(username), Some(password)) = (&self.username, &self.password) {
            if !username.is_empty() {
                return Some(RegistryAuth {
                    username: username.clone(),
                    password: password.clone(),
                });
            }
        }
        let raw = self.auth.as_deref()?;
        let decoded = decode_base64(raw)?;
        let text = String::from_utf8(decoded).ok()?;
        let (username, password) = text.split_once(':')?;
        if username.is_empty() {
            return None;
        }
        Some(RegistryAuth {
            username: username.to_string(),
            password: password.to_string(),
        })
    }
}

fn decode_base64(raw: &str) -> Option<Vec<u8>> {
    let engine = base64::engine::general_purpose::STANDARD;
    engine
        .decode(raw)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(raw))
        .ok()
}

/// Pick credentials for `image` out of decoded Secret payloads.
///
/// `payloads` are the raw values of `.dockerconfigjson` / `.dockercfg` (already
/// base64-decoded by the Kubernetes API layer). Secrets are tried in the order
/// the Pod listed them, matching kubelet's behaviour.
pub fn resolve(secret_payloads: &[Vec<u8>], image: &str) -> Option<RegistryAuth> {
    let upstream = ImageRef::parse_unmirrored(image).ok()?;
    let host = normalize_registry(&upstream.registry);
    for payload in secret_payloads {
        let Some(auths) = parse_auths(payload) else {
            continue;
        };
        for (key, entry) in &auths {
            if registry_matches(key, &host) {
                if let Some(auth) = entry.to_auth() {
                    return Some(auth);
                }
            }
        }
    }
    None
}

/// Accept both `{"auths": {...}}` (dockerconfigjson) and the bare map
/// (legacy dockercfg).
fn parse_auths(payload: &[u8]) -> Option<HashMap<String, DockerAuthEntry>> {
    if let Ok(file) = serde_json::from_slice::<DockerConfigFile>(payload) {
        if !file.auths.is_empty() {
            return Some(file.auths);
        }
    }
    serde_json::from_slice::<HashMap<String, DockerAuthEntry>>(payload).ok()
}

/// `https://index.docker.io/v1/` → `index.docker.io`
fn normalize_registry(key: &str) -> String {
    let key = key.trim();
    let key = key
        .strip_prefix("https://")
        .or_else(|| key.strip_prefix("http://"))
        .unwrap_or(key);
    let host = key.split('/').next().unwrap_or(key);
    host.trim_end_matches('/').to_ascii_lowercase()
}

fn is_docker_hub(host: &str) -> bool {
    matches!(
        host,
        "docker.io" | "index.docker.io" | "registry-1.docker.io" | "registry.hub.docker.com"
    )
}

fn registry_matches(key: &str, image_host: &str) -> bool {
    let key_host = normalize_registry(key);
    key_host == image_host || (is_docker_hub(&key_host) && is_docker_hub(image_host))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dockerconfigjson(key: &str, user: &str, pass: &str) -> Vec<u8> {
        format!(r#"{{"auths":{{"{key}":{{"username":"{user}","password":"{pass}"}}}}}}"#)
            .into_bytes()
    }

    #[test]
    fn docker_hub_alias_matches_official_image() {
        let payload = dockerconfigjson("https://index.docker.io/v1/", "alice", "s3cret");
        let auth = resolve(&[payload], "alpine:latest").expect("auth");
        assert_eq!(auth.username, "alice");
        assert_eq!(auth.password, "s3cret");
    }

    #[test]
    fn private_registry_with_port_matches() {
        let payload = dockerconfigjson("registry.local:5000", "bob", "pw");
        let auth = resolve(&[payload], "registry.local:5000/team/app:1.0").expect("auth");
        assert_eq!(auth.username, "bob");
    }

    #[test]
    fn unrelated_registry_does_not_match() {
        let payload = dockerconfigjson("ghcr.io", "bob", "pw");
        assert!(resolve(&[payload], "alpine:latest").is_none());
    }

    #[test]
    fn legacy_auth_field_is_decoded() {
        let encoded = base64::engine::general_purpose::STANDARD.encode("carol:pass:with:colons");
        let payload = format!(r#"{{"auths":{{"registry.example.com":{{"auth":"{encoded}"}}}}}}"#)
            .into_bytes();
        let auth = resolve(&[payload], "registry.example.com/app:1").expect("auth");
        assert_eq!(auth.username, "carol");
        // Only the first ':' separates user from password.
        assert_eq!(auth.password, "pass:with:colons");
    }

    #[test]
    fn bare_dockercfg_map_is_accepted() {
        let payload = br#"{"registry.example.com":{"username":"dave","password":"pw"}}"#.to_vec();
        let auth = resolve(&[payload], "registry.example.com/app:1").expect("auth");
        assert_eq!(auth.username, "dave");
    }

    #[test]
    fn first_matching_secret_wins() {
        let a = dockerconfigjson("ghcr.io", "wrong", "wrong");
        let b = dockerconfigjson("docker.io", "right", "right");
        let auth = resolve(&[a, b], "alpine:latest").expect("auth");
        assert_eq!(auth.username, "right");
    }
}
