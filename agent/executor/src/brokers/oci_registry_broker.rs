use async_trait::async_trait;
use bytes::Bytes;
use reqwest::{header, Client, StatusCode};
use serde::Deserialize;
use tracing::{debug, instrument};

use crate::error::{DroidError, Result};
use crate::models::{Digest, ImageConfig, ImageRef, Manifest, RegistryAuth};

// ─── Trait ────────────────────────────────────────────────────────────────────

#[async_trait]
pub trait OciRegistryBroker: Send + Sync {
    async fn fetch_manifest(
        &self,
        image_ref: &ImageRef,
        auth: Option<&RegistryAuth>,
    ) -> Result<Manifest>;
    async fn fetch_layer(
        &self,
        image_ref: &ImageRef,
        digest: &Digest,
        auth: Option<&RegistryAuth>,
    ) -> Result<Bytes>;
    async fn fetch_config(
        &self,
        image_ref: &ImageRef,
        digest: &Digest,
        auth: Option<&RegistryAuth>,
    ) -> Result<ImageConfig>;
}

// ─── Implementation ───────────────────────────────────────────────────────────

pub struct OciRegistryBrokerImpl {
    client: Client,
}

impl Default for OciRegistryBrokerImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl OciRegistryBrokerImpl {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .user_agent("droidnode/0.1.0")
                // Without these a pull against an unreachable registry hangs for
                // minutes while the reconcile loop holds its lock (see B20/C9).
                .connect_timeout(std::time::Duration::from_secs(20))
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("failed to build HTTP client"),
        }
    }

    // Attempt the request; on 401 fetch a bearer token and retry once.
    // With credentials we send Basic auth on the first attempt too: some
    // registries accept it directly, and the token endpoint needs it anyway.
    async fn authenticated_get(
        &self,
        url: &str,
        auth: Option<&RegistryAuth>,
    ) -> Result<reqwest::Response> {
        let resp = with_auth(self.client.get(url), auth).send().await?;

        if resp.status() == StatusCode::UNAUTHORIZED {
            let token = self.resolve_token(&resp, auth).await?;
            let resp2 = self
                .client
                .get(url)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .send()
                .await?;
            return Ok(resp2);
        }

        Ok(resp)
    }

    // Parse the WWW-Authenticate header and exchange it for a bearer token.
    async fn resolve_token(
        &self,
        resp: &reqwest::Response,
        auth: Option<&RegistryAuth>,
    ) -> Result<String> {
        let www_auth = resp
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let realm = extract_challenge_param(&www_auth, "realm")
            .ok_or_else(|| DroidError::OciRegistry("missing realm in WWW-Authenticate".into()))?;
        let service = extract_challenge_param(&www_auth, "service").unwrap_or_default();
        let scope = extract_challenge_param(&www_auth, "scope").unwrap_or_default();

        let token_url = format!("{realm}?service={service}&scope={scope}");
        debug!(%token_url, authenticated = auth.is_some(), "fetching OCI auth token");

        #[derive(Deserialize)]
        struct TokenResp {
            token: Option<String>,
            access_token: Option<String>,
        }

        let req = with_auth(self.client.get(&token_url), auth);
        let tok: TokenResp = req.send().await?.json().await?;
        tok.token
            .or(tok.access_token)
            .ok_or_else(|| DroidError::OciRegistry("token response contained no token".into()))
    }

    // Fetch a manifest URL and return the raw JSON body.
    // Handles auth transparently. Accepts both single manifests and manifest lists.
    async fn fetch_manifest_value(
        &self,
        image_ref: &ImageRef,
        auth: Option<&RegistryAuth>,
    ) -> Result<serde_json::Value> {
        let url = format!(
            "{}/v2/{}/manifests/{}",
            image_ref.registry_url(),
            image_ref.repository,
            image_ref.reference
        );

        const ACCEPT: &str = "application/vnd.oci.image.manifest.v1+json, \
            application/vnd.docker.distribution.manifest.v2+json, \
            application/vnd.docker.distribution.manifest.list.v2+json, \
            application/vnd.oci.image.index.v1+json";

        let resp = with_auth(self.client.get(&url).header(header::ACCEPT, ACCEPT), auth)
            .send()
            .await?;

        let resp = if resp.status() == StatusCode::UNAUTHORIZED {
            let token = self.resolve_token(&resp, auth).await?;
            self.client
                .get(&url)
                .header(header::ACCEPT, ACCEPT)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .send()
                .await?
        } else {
            resp
        };

        if !resp.status().is_success() {
            return Err(DroidError::ImageNotFound(format!(
                "{}:{}",
                image_ref.repository, image_ref.reference
            )));
        }

        Ok(resp.json().await?)
    }
}

fn with_auth(
    builder: reqwest::RequestBuilder,
    auth: Option<&RegistryAuth>,
) -> reqwest::RequestBuilder {
    match auth {
        Some(a) => builder.basic_auth(&a.username, Some(&a.password)),
        None => builder,
    }
}

fn extract_challenge_param(header: &str, key: &str) -> Option<String> {
    // Format: Bearer realm="...",service="...",scope="..."
    let search = format!("{key}=\"");
    let start = header.find(&search)? + search.len();
    let end = header[start..].find('"')? + start;
    Some(header[start..end].to_string())
}

#[async_trait]
impl OciRegistryBroker for OciRegistryBrokerImpl {
    #[instrument(skip(self, auth), fields(image = %image_ref.repository))]
    async fn fetch_manifest(
        &self,
        image_ref: &ImageRef,
        auth: Option<&RegistryAuth>,
    ) -> Result<Manifest> {
        let body = self.fetch_manifest_value(image_ref, auth).await?;

        // Manifest list / OCI image index: has a "manifests" array, no "layers".
        // Pick the best matching linux platform and fetch that specific manifest.
        if let Some(manifests) = body.get("manifests").and_then(|m| m.as_array()) {
            let target_arch = if cfg!(target_arch = "aarch64") {
                "arm64"
            } else {
                "amd64"
            };

            // C1: never fall back to another architecture. Running an amd64 image
            // inside proot fails with "cannot execute binary file" much later and
            // is very confusing; fail here with a clear message instead.
            let entry = manifests
                .iter()
                .find(|m| {
                    let p = m.get("platform");
                    p.and_then(|p| p.get("os")).and_then(|v| v.as_str()) == Some("linux")
                        && p.and_then(|p| p.get("architecture"))
                            .and_then(|v| v.as_str())
                            == Some(target_arch)
                })
                .ok_or_else(|| {
                    let available: Vec<String> = manifests
                        .iter()
                        .filter_map(|m| {
                            let p = m.get("platform")?;
                            Some(format!(
                                "{}/{}",
                                p.get("os").and_then(|v| v.as_str()).unwrap_or("?"),
                                p.get("architecture")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("?")
                            ))
                        })
                        .collect();
                    DroidError::OciRegistry(format!(
                        "image has no linux/{target_arch} variant (available: {}). \
                         Cannot run a foreign-architecture image inside proot.",
                        available.join(", ")
                    ))
                })?;

            let digest = entry
                .get("digest")
                .and_then(|d| d.as_str())
                .ok_or_else(|| {
                    DroidError::OciRegistry("manifest list entry has no digest".into())
                })?;

            debug!(%digest, arch = target_arch, "resolved manifest list to platform manifest");

            let platform_ref = ImageRef {
                registry: image_ref.registry.clone(),
                repository: image_ref.repository.clone(),
                reference: digest.to_string(),
            };
            let platform_body = self.fetch_manifest_value(&platform_ref, auth).await?;
            return serde_json::from_value(platform_body).map_err(DroidError::Json);
        }

        serde_json::from_value(body).map_err(DroidError::Json)
    }

    #[instrument(skip(self, auth), fields(digest = %digest))]
    async fn fetch_layer(
        &self,
        image_ref: &ImageRef,
        digest: &Digest,
        auth: Option<&RegistryAuth>,
    ) -> Result<Bytes> {
        let url = format!(
            "{}/v2/{}/blobs/{}",
            image_ref.registry_url(),
            image_ref.repository,
            digest
        );

        debug!(%url, "fetching layer blob");
        let resp = self.authenticated_get(&url, auth).await?;

        if !resp.status().is_success() {
            return Err(DroidError::OciRegistry(format!(
                "blob fetch failed: {}",
                resp.status()
            )));
        }

        // C2: verify the blob really matches its digest before it is unpacked.
        // A truncated download or a mirror serving the wrong content would
        // otherwise surface as a bizarre tar error much later.
        let bytes = resp.bytes().await?;
        let computed = sha256_hex(&bytes);
        let expected = digest.hex().to_string();
        if computed != expected {
            return Err(DroidError::OciRegistry(format!(
                "layer digest mismatch: expected {expected}, got {computed} (corrupted download or wrong mirror response)"
            )));
        }
        Ok(bytes)
    }

    #[instrument(skip(self, auth), fields(digest = %digest))]
    async fn fetch_config(
        &self,
        image_ref: &ImageRef,
        digest: &Digest,
        auth: Option<&RegistryAuth>,
    ) -> Result<ImageConfig> {
        let url = format!(
            "{}/v2/{}/blobs/{}",
            image_ref.registry_url(),
            image_ref.repository,
            digest
        );

        debug!(%url, "fetching image config");
        let resp = self.authenticated_get(&url, auth).await?;

        if !resp.status().is_success() {
            return Err(DroidError::OciRegistry(format!(
                "config fetch failed: {}",
                resp.status()
            )));
        }

        Ok(resp.json::<ImageConfig>().await?)
    }
}

/// Hex-encoded sha256 of a blob (C2: verify layer digests before unpacking).
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ImageRef;

    #[test]
    fn basic_auth_header_is_set_for_pull_secrets() {
        let client = Client::new();
        let auth = RegistryAuth {
            username: "c5user".into(),
            password: "c5pass".into(),
        };
        let req = with_auth(client.get("https://registry.example.com/v2/"), Some(&auth))
            .build()
            .unwrap();
        let header = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(header.starts_with("Basic "), "got {header:?}");
        // "c5user:c5pass" — proves both fields are encoded, not just the user.
        assert_eq!(header, "Basic YzV1c2VyOmM1cGFzcw==");
    }

    #[test]
    fn no_auth_means_no_authorization_header() {
        let client = Client::new();
        let req = with_auth(client.get("https://registry.example.com/v2/"), None)
            .build()
            .unwrap();
        assert!(req.headers().get(header::AUTHORIZATION).is_none());
    }

    #[tokio::test]
    async fn test_pull_alpine_manifest() {
        if std::env::var("DROIDNODE_SKIP_NETWORK_TESTS").is_ok() {
            eprintln!("skipping: registry access disabled via DROIDNODE_SKIP_NETWORK_TESTS");
            return;
        }

        let broker = OciRegistryBrokerImpl::new();
        let image_ref = ImageRef::parse("alpine:latest").unwrap();
        let manifest = broker.fetch_manifest(&image_ref, None).await.unwrap();
        assert!(!manifest.layers.is_empty());
        println!("layers: {}", manifest.layers.len());
    }
}
