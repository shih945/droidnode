//! On-device image store: metadata, listing, deletion and layer GC.
//!
//! Two things live on disk per image:
//!   files/layers/<digest>/layer.tar.gz      compressed layers (shared)
//!   code_cache/rootfs/<registry>/<repo>/<ref>  merged, unpacked rootfs
//!
//! There is no central index - each rootfs carries a `.droidnode_image_meta.json`
//! so the store survives agent restarts without any extra bookkeeping. Images
//! pulled before this existed simply get their metadata backfilled on first read.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::error::Result;

pub const META_FILE: &str = ".droidnode_image_meta.json";
const READY_FILE: &str = ".droidnode_ready";
const MAX_SCAN_DEPTH: usize = 6;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerMeta {
    pub digest: String,
    /// compressed blob size in bytes
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageMeta {
    /// Image reference as the pod asked for it, e.g. "alpine:latest"
    pub name: String,
    /// Registry after mirror rewriting (informational).
    pub registry: String,
    pub repository: String,
    pub reference: String,
    /// Manifest digest - the "hash" shown in the UI.
    #[serde(default)]
    pub manifest_digest: String,
    #[serde(default)]
    pub config_digest: String,
    #[serde(default)]
    pub layers: Vec<LayerMeta>,
    /// Sum of compressed layers.
    #[serde(default)]
    pub compressed_size: u64,
    /// Size of the unpacked rootfs.
    #[serde(default)]
    pub unpacked_size: u64,
    #[serde(default)]
    pub pulled_at: String,
    /// Absolute path of the rootfs directory.
    pub rootfs_dir: String,
}

impl ImageMeta {
    /// Total on-disk footprint of this image (layers are shared, so this is the
    /// upper bound rather than an exclusive accounting).
    pub fn total_size(&self) -> u64 {
        self.compressed_size + self.unpacked_size
    }
}

pub fn meta_path(rootfs: &Path) -> PathBuf {
    rootfs.join(META_FILE)
}

pub fn read_meta(rootfs: &Path) -> Option<ImageMeta> {
    let bytes = std::fs::read(meta_path(rootfs)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn write_meta(rootfs: &Path, meta: &ImageMeta) -> Result<()> {
    let tmp = meta_path(rootfs).with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(meta)?)?;
    std::fs::rename(&tmp, meta_path(rootfs))?;
    Ok(())
}

/// Recursively total a directory's size (follows no symlinks).
pub fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            total += dir_size(&entry.path());
        } else if let Ok(md) = entry.metadata() {
            total += md.len();
        }
    }
    total
}

/// Find every prepared rootfs under `base` (a directory containing READY_FILE).
///
/// Skips the `pods` subtree: per-pod writable rootfs copies (B17) live under
/// `<image store>/pods/<uid>/<container>/rootfs` and inherit the image's
/// READY/meta files, so scanning them reported every running pod as another
/// copy of its image (the app image page showed alpine 20+ times).
fn find_rootfs(base: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    if base.file_name().map(|name| name == "pods").unwrap_or(false) {
        return;
    }
    if base.join(READY_FILE).exists() {
        out.push(base.to_path_buf());
        return;
    }
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            find_rootfs(&entry.path(), depth + 1, out);
        }
    }
}

/// Derive a display name for a rootfs that predates metadata.
fn fallback_name(base: &Path, rootfs: &Path) -> String {
    rootfs
        .strip_prefix(base)
        .ok()
        .map(|p| {
            let parts: Vec<String> = p
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect();
            if parts.len() >= 3 {
                let registry = &parts[0];
                let reference = parts.last().unwrap();
                let repo = parts[1..parts.len() - 1].join("/");
                if registry.contains("docker.io") || registry.contains("library") {
                    format!("{repo}:{reference}")
                } else {
                    format!("{registry}/{repo}:{reference}")
                }
            } else {
                p.to_string_lossy().to_string()
            }
        })
        .unwrap_or_else(|| rootfs.to_string_lossy().to_string())
}

/// List all images, backfilling metadata (sizes, name) when missing.
pub fn list(base: &Path) -> Vec<ImageMeta> {
    let mut dirs = Vec::new();
    find_rootfs(base, 0, &mut dirs);

    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for rootfs in dirs {
        match read_meta(&rootfs) {
            Some(meta) => {
                // Defensive: several paths can carry a copy of the same image
                // meta (pod rootfs copies), report each image once.
                if seen.insert(meta.rootfs_dir.clone()) {
                    out.push(meta);
                }
            }
            None => {
                // Pre-existing image: measure once and persist.
                let unpacked = dir_size(&rootfs);
                let name = fallback_name(base, &rootfs);
                let meta = ImageMeta {
                    name,
                    registry: String::new(),
                    repository: String::new(),
                    reference: String::new(),
                    manifest_digest: String::new(),
                    config_digest: String::new(),
                    layers: Vec::new(),
                    compressed_size: 0,
                    unpacked_size: unpacked,
                    pulled_at: String::new(),
                    rootfs_dir: rootfs.to_string_lossy().to_string(),
                };
                if let Err(e) = write_meta(&rootfs, &meta) {
                    warn!(rootfs = %rootfs.display(), error = %e, "could not backfill image meta");
                }
                out.push(meta);
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// G3: remove half-written rootfs trees left behind by a crash/power loss.
///
/// A rootfs only gets `READY_FILE` as its very last write, so anything with
/// image-like content (meta file or a top-level `bin`/`etc`) but no sentinel is
/// incomplete and must be rebuilt from layers. The `pods` subtree is handled
/// separately: per-pod copies use `.droidnode_pod_rootfs_ready`.
///
/// Returns `(removed_images, removed_pod_rootfs, reclaimed_bytes)`.
pub fn cleanup_incomplete(base: &Path) -> (usize, usize, u64) {
    let mut images = 0usize;
    let mut bytes = 0u64;
    cleanup_images(base, 0, &mut images, &mut bytes);
    prune_empty_dirs(base, 0);
    let pods = cleanup_pod_rootfs(base);
    (images, pods, bytes)
}

fn cleanup_images(dir: &Path, depth: usize, removed: &mut usize, bytes: &mut u64) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    if dir.file_name().map(|name| name == "pods").unwrap_or(false) {
        return;
    }
    // Complete images are never touched.
    if dir.join(READY_FILE).exists() {
        return;
    }
    // Only consider directories deep enough to be `<registry>/<repo>/<ref>`.
    let looks_like_rootfs = dir.join(META_FILE).exists()
        || dir.join("bin").is_dir()
        || dir.join("etc").is_dir();
    if depth >= 3 && looks_like_rootfs {
        let size = dir_size(dir);
        if std::fs::remove_dir_all(dir).is_ok() {
            *removed += 1;
            *bytes += size;
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            cleanup_images(&entry.path(), depth + 1, removed, bytes);
        }
    }
}

fn cleanup_pod_rootfs(base: &Path) -> usize {
    const POD_READY: &str = ".droidnode_pod_rootfs_ready";
    let pods = base.join("pods");
    let mut removed = 0usize;
    let Ok(uids) = std::fs::read_dir(&pods) else {
        return 0;
    };
    for uid in uids.flatten() {
        let Ok(containers) = std::fs::read_dir(uid.path()) else {
            continue;
        };
        for container in containers.flatten() {
            let rootfs = container.path().join("rootfs");
            if rootfs.is_dir() && !rootfs.join(POD_READY).exists()
                && std::fs::remove_dir_all(&rootfs).is_ok()
            {
                removed += 1;
            }
            remove_if_empty(&container.path());
        }
        remove_if_empty(&uid.path());
    }
    removed
}

/// Remove empty leftovers (`fake-registry/fake-image`) so a failed unpack does
/// not leave a skeleton tree that confuses later listings.
fn prune_empty_dirs(dir: &Path, depth: usize) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    if dir.file_name().map(|name| name == "pods").unwrap_or(false) {
        return;
    }
    // Never descend into a complete image: its internals may legitimately
    // contain empty directories.
    if dir.join(READY_FILE).exists() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut empty = true;
    for entry in entries.flatten() {
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            prune_empty_dirs(&entry.path(), depth + 1);
        }
        // Re-read: a child may have just deleted itself.
        if entry.path().exists() {
            empty = false;
        }
    }
    if empty && depth > 0 {
        let _ = std::fs::remove_dir(dir);
    }
}

fn remove_if_empty(dir: &Path) {
    if std::fs::read_dir(dir).map(|mut it| it.next().is_none()).unwrap_or(false) {
        let _ = std::fs::remove_dir(dir);
    }
}

/// Delete one image (rootfs) and garbage-collect layers no longer referenced.
/// Returns the number of bytes reclaimed.
pub fn delete(base: &Path, layers_base: &Path, rootfs_dir: &str) -> Result<u64> {    let target = PathBuf::from(rootfs_dir);
    // Safety: only delete something that lives under the rootfs base.
    let canonical_base = std::fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
    let canonical_target =
        std::fs::canonicalize(&target).unwrap_or_else(|_| target.to_path_buf());
    if !canonical_target.starts_with(&canonical_base) {
        return Err(crate::error::DroidError::Filesystem(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("refusing to delete outside the image store: {rootfs_dir}"),
        )));
    }

    let freed_rootfs = dir_size(&canonical_target);
    std::fs::remove_dir_all(&canonical_target)?;
    info!(rootfs = %canonical_target.display(), bytes = freed_rootfs, "image removed");

    // Layer GC: anything not referenced by a remaining image goes away.
    let mut referenced = std::collections::HashSet::new();
    for meta in list(base) {
        for layer in meta.layers {
            referenced.insert(layer.digest.replace(':', "-"));
        }
    }

    let mut freed_layers = 0u64;
    if let Ok(entries) = std::fs::read_dir(layers_base) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("sha256-") && !referenced.contains(&name) {
                let size = dir_size(&entry.path());
                if std::fs::remove_dir_all(entry.path()).is_ok() {
                    freed_layers += size;
                    info!(layer = %name, bytes = size, "unreferenced layer removed");
                }
            }
        }
    }

    Ok(freed_rootfs + freed_layers)
}
