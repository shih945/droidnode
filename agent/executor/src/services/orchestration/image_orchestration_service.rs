use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;
use tracing::{info, instrument};

use crate::error::Result;
use crate::models::{ImageRef, RegistryAuth};
use crate::services::foundation::{
    image_pull_service::ImagePullService,
    image_unpack_service::ImageUnpackService,
};

// ─── Trait ────────────────────────────────────────────────────────────────────

#[async_trait]
pub trait ImageOrchestrationService: Send + Sync {
    /// Pull → unpack → merge → return path to merged rootfs.
    async fn prepare_image(
        &self,
        image_ref: &ImageRef,
        policy: crate::models::PullPolicy,
        auth: Option<&RegistryAuth>,
    ) -> Result<PathBuf>;
}

// ─── Implementation ───────────────────────────────────────────────────────────

pub struct ImageOrchestrationServiceImpl {
    pull_service: Arc<dyn ImagePullService>,
    unpack_service: Arc<dyn ImageUnpackService>,
    rootfs_base: PathBuf,
}

impl ImageOrchestrationServiceImpl {
    pub fn new(
        pull_service: Arc<dyn ImagePullService>,
        unpack_service: Arc<dyn ImageUnpackService>,
        rootfs_base: PathBuf,
    ) -> Self {
        Self { pull_service, unpack_service, rootfs_base }
    }

    fn image_rootfs_path(&self, image_ref: &ImageRef) -> PathBuf {
        let safe_ref = image_ref.reference.replace(':', "-").replace('@', "_at_");
        self.rootfs_base
            .join(&image_ref.registry)
            .join(&image_ref.repository)
            .join(&safe_ref)
    }
}

#[async_trait]
impl ImageOrchestrationService for ImageOrchestrationServiceImpl {
    #[instrument(skip(self, auth), fields(image = %image_ref.repository, reference = %image_ref.reference))]
    async fn prepare_image(
        &self,
        image_ref: &ImageRef,
        policy: crate::models::PullPolicy,
        auth: Option<&RegistryAuth>,
    ) -> Result<PathBuf> {
        use crate::models::PullPolicy;
        let rootfs = self.image_rootfs_path(image_ref);
        let ready = rootfs.join(".droidnode_ready").exists();

        // C6: imagePullPolicy
        match policy {
            PullPolicy::IfNotPresent if ready => {
                info!(rootfs = %rootfs.display(), "rootfs already prepared (IfNotPresent)");
                return Ok(rootfs);
            }
            PullPolicy::Never if !ready => {
                return Err(crate::error::DroidError::OciRegistry(format!(
                    "image {}:{} is not present locally and imagePullPolicy is Never",
                    image_ref.repository, image_ref.reference
                )));
            }
            PullPolicy::Always if ready => {
                info!(rootfs = %rootfs.display(), "imagePullPolicy=Always - re-pulling");
                let _ = tokio::fs::remove_file(rootfs.join(".droidnode_ready")).await;
            }
            _ => {}
        }

        // 1. Pull all layers (downloads only what is not cached)
        let pulled = self.pull_service.pull_image(image_ref, auth).await?;
        info!(layers = pulled.layer_digests.len(), "image pulled");

        // 2. Apply layers in order (base → top) into the merged rootfs directory
        fs::create_dir_all(&rootfs).await?;
        for digest in &pulled.layer_digests {
            info!(%digest, rootfs = %rootfs.display(), "applying layer");
            self.unpack_service.unpack_layer(digest, &rootfs).await?;
        }

        // 4. Persist the image's ContainerConfig (entrypoint, cmd, env) so the
        //    execution service can resolve the command without re-fetching from registry.
        if let Some(container_config) = pulled.config.config.as_ref() {
            let json = serde_json::to_vec(container_config)?;
            fs::write(rootfs.join(".droidnode_image_config.json"), json).await?;
        }

        // 5. Record on-disk metadata for the image list / deletion UI.
        //    The OCI broker does not surface the manifest digest (response
        //    headers are dropped), so the config digest is used as the image id -
        //    it is the same value `docker images` shows in the IMAGE ID column.
        let config_digest = pulled.manifest.config.digest.clone();
        let layers: Vec<crate::services::foundation::image_store::LayerMeta> = pulled
            .manifest
            .layers
            .iter()
            .map(|d| crate::services::foundation::image_store::LayerMeta {
                digest: d.digest.clone(),
                size: d.size.max(0) as u64,
            })
            .collect();
        let compressed_size: u64 = layers.iter().map(|l| l.size).sum();
        let unpacked_size = tokio::task::spawn_blocking({
            let rootfs = rootfs.clone();
            move || crate::services::foundation::image_store::dir_size(&rootfs)
        })
        .await
        .unwrap_or(0);

        let meta = crate::services::foundation::image_store::ImageMeta {
            name: friendly_name(&image_ref.registry, &image_ref.repository, &image_ref.reference),
            registry: image_ref.registry.clone(),
            repository: image_ref.repository.clone(),
            reference: image_ref.reference.clone(),
            manifest_digest: config_digest.clone(),
            config_digest,
            layers,
            compressed_size,
            unpacked_size,
            pulled_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            rootfs_dir: rootfs.to_string_lossy().to_string(),
        };
        let _ = crate::services::foundation::image_store::write_meta(&rootfs, &meta);
        info!(
            image = %meta.name,
            bytes = meta.total_size(),
            "image metadata recorded"
        );

        // 6. Sentinel file marks this rootfs as fully prepared
        fs::write(rootfs.join(".droidnode_ready"), b"1").await?;
        info!(rootfs = %rootfs.display(), "rootfs ready");

        Ok(rootfs)
    }
}

/// Drop the mirror host and the implicit `library/` namespace so the UI shows
/// "alpine:latest" instead of "<mirror-host>/library/alpine:latest".
///
/// `library/` only exists for Docker Hub official images, so it also covers pulls
/// that went through a mirror: after rewriting, `registry` holds the mirror host
/// and is no longer recognisable as Docker Hub.
fn friendly_name(registry: &str, repository: &str, reference: &str) -> String {
    let official = repository.starts_with("library/");
    let repo = repository.strip_prefix("library/").unwrap_or(repository);
    let docker_hub = matches!(
        registry,
        "docker.io" | "registry-1.docker.io" | "index.docker.io"
    );
    if docker_hub || official {
        format!("{}:{}", repo, reference)
    } else {
        format!("{}/{}:{}", registry, repo, reference)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brokers::{FilesystemBroker, FilesystemBrokerImpl, OciRegistryBroker, OciRegistryBrokerImpl};
    use crate::services::foundation::{
        image_pull_service::{ImagePullService, ImagePullServiceImpl},
        image_unpack_service::{ImageUnpackService, ImageUnpackServiceImpl},
    };
    use std::sync::Arc;

    #[tokio::test]
    async fn test_prepare_alpine_rootfs() {
        let tmp = std::env::temp_dir().join("droidnode_orch_test");
        let layers_dir = tmp.join("layers");
        let rootfs_dir = tmp.join("rootfs");

        let oci = Arc::new(OciRegistryBrokerImpl::new());
        let fs = Arc::new(FilesystemBrokerImpl::new(layers_dir));

        let pull = Arc::new(ImagePullServiceImpl::new(
            Arc::clone(&oci) as Arc<dyn OciRegistryBroker>,
            Arc::clone(&fs) as Arc<dyn FilesystemBroker>,
        ));
        let unpack = Arc::new(ImageUnpackServiceImpl::new(
            Arc::clone(&fs) as Arc<dyn FilesystemBroker>,
        ));

        let svc = ImageOrchestrationServiceImpl::new(
            pull as Arc<dyn ImagePullService>,
            unpack as Arc<dyn ImageUnpackService>,
            rootfs_dir.clone(),
        );

        let image_ref = crate::models::ImageRef::parse("alpine:latest").unwrap();
        let rootfs = svc.prepare_image(&image_ref, crate::models::PullPolicy::IfNotPresent, None).await.unwrap();

        assert!(rootfs.join(".droidnode_ready").exists(), "sentinel missing");
        assert!(rootfs.join("bin").exists(), "rootfs /bin missing");
        assert!(rootfs.join("etc").exists(), "rootfs /etc missing");

        println!("rootfs at: {}", rootfs.display());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
