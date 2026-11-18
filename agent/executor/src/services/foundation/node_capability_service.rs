use async_trait::async_trait;
use tracing::instrument;

use crate::error::Result;
use crate::models::{
    BatteryInfo, CpuInfo, MemoryInfo, NetworkInfo, NodeConditions, NodeImageInfo, NodeProfile,
    RuntimeInfo, StorageInfo,
};
use crate::status_ipc::DeviceState;

const RUNTIME_NAME: &str = "proot-oci-runner";
const RUNTIME_VERSION: &str = env!("CARGO_PKG_VERSION");
const LOW_BATTERY_THRESHOLD: u8 = 20;
const LOW_MEMORY_THRESHOLD_BYTES: u64 = 512 * 1024 * 1024; // 512 MB

// ─── Trait ────────────────────────────────────────────────────────────────────

#[async_trait]
pub trait NodeCapabilityService: Send + Sync {
    async fn get_profile(&self) -> Result<NodeProfile>;
}

// ─── Implementation ───────────────────────────────────────────────────────────

pub struct NodeCapabilityServiceImpl {
    node_id: String,
    layers_root: std::path::PathBuf,
    rootfs_base: std::path::PathBuf,
    /// Telemetry pushed over the Unix socket. When a sample is fresh it wins
    /// over the platform stubs (which cannot read battery/thermal on Android).
    device: DeviceState,
}

impl NodeCapabilityServiceImpl {
    pub fn new(
        node_id: String,
        layers_root: std::path::PathBuf,
        rootfs_base: std::path::PathBuf,
        device: DeviceState,
    ) -> Self {
        Self {
            node_id,
            layers_root,
            rootfs_base,
            device,
        }
    }
}

#[async_trait]
impl NodeCapabilityService for NodeCapabilityServiceImpl {
    #[instrument(skip(self))]
    async fn get_profile(&self) -> Result<NodeProfile> {
        let cpu = read_cpu_info();
        let memory = read_memory_info();
        let storage = read_storage_info(&self.layers_root);
        let telemetry = self.device.snapshot().await;
        let (battery, network) = if telemetry.is_fresh() {
            let battery = match telemetry.battery_percent {
                Some(percent) => BatteryInfo {
                    percent,
                    charging: telemetry.battery_charging.unwrap_or(false),
                },
                None => read_battery_info(),
            };
            let network = match telemetry.network_type.as_deref() {
                Some(kind) if !kind.is_empty() => NetworkInfo {
                    network_type: kind.to_string(),
                },
                _ => read_network_info(),
            };
            (battery, network)
        } else {
            (read_battery_info(), read_network_info())
        };
        let images = read_node_images(self.rootfs_base.clone()).await;

        let conditions = NodeConditions {
            ready: is_ready(&memory, &battery, &network),
            battery_pressure: battery_pressure(&battery),
            memory_pressure: memory.available_bytes < LOW_MEMORY_THRESHOLD_BYTES,
            network_available: network.network_type != "none",
        };

        Ok(NodeProfile {
            node_id: self.node_id.clone(),
            cpu,
            memory,
            storage,
            runtime: RuntimeInfo {
                name: RUNTIME_NAME.into(),
                version: RUNTIME_VERSION.into(),
            },
            battery,
            network,
            conditions,
            images,
        })
    }
}

// ─── Platform-specific readers ────────────────────────────────────────────────

#[cfg(unix)]
fn read_cpu_info() -> CpuInfo {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);

    // Detect architecture via uname
    let arch = {
        let mut u: libc::utsname = unsafe { std::mem::zeroed() };
        unsafe { libc::uname(&mut u) };
        let machine = unsafe {
            std::ffi::CStr::from_ptr(u.machine.as_ptr())
                .to_string_lossy()
                .to_string()
        };
        machine
    };

    CpuInfo { arch, cores }
}

#[cfg(not(unix))]
fn read_cpu_info() -> CpuInfo {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);
    CpuInfo {
        arch: std::env::consts::ARCH.to_string(),
        cores,
    }
}

#[cfg(unix)]
pub fn read_memory_info() -> MemoryInfo {
    let content = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let mut total = 0u64;
    let mut available = 0u64;

    for line in content.lines() {
        if line.starts_with("MemTotal:") {
            total = parse_meminfo_kb(line);
        } else if line.starts_with("MemAvailable:") {
            available = parse_meminfo_kb(line);
        }
    }

    MemoryInfo {
        total_bytes: total * 1024,
        available_bytes: available * 1024,
    }
}

#[cfg(not(unix))]
pub fn read_memory_info() -> MemoryInfo {
    MemoryInfo {
        total_bytes: 4 * 1024 * 1024 * 1024,
        available_bytes: 2 * 1024 * 1024 * 1024,
    }
}

pub fn read_storage_info(path: &std::path::Path) -> StorageInfo {
    // Use std::fs for statvfs equivalent (available on all platforms via statvfs crate or libc)
    #[cfg(unix)]
    {
        use std::ffi::CString;
        let cpath = CString::new(path.to_str().unwrap_or("/")).unwrap();
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        unsafe { libc::statvfs(cpath.as_ptr(), &mut stat) };
        // Use `f_frsize` (fragment size) for both numbers; that is the unit
        // `f_blocks`/`f_bavail` are counted in on Linux/Android. The node
        // used to report only free space, so `kubectl describe node` had no
        // ephemeral-storage Capacity at all.
        let block_size = if stat.f_frsize > 0 {
            stat.f_frsize
        } else {
            stat.f_bsize
        };
        StorageInfo {
            total_bytes: stat.f_blocks * block_size,
            available_bytes: stat.f_bavail * block_size,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        StorageInfo {
            total_bytes: 64 * 1024 * 1024 * 1024,
            available_bytes: 64 * 1024 * 1024 * 1024,
        }
    }
}

fn read_battery_info() -> BatteryInfo {
    // On Android this is populated by the Kotlin layer via IPC.
    // For Linux development, report a full always-charging battery.
    BatteryInfo {
        percent: 100,
        charging: true,
    }
}

fn read_network_info() -> NetworkInfo {
    // On Android this comes from the Kotlin layer.
    // Stub: assume WiFi for dev builds.
    NetworkInfo {
        network_type: "wifi".into(),
    }
}

/// `Node.status.images`. Scans the on-device image store (the same one the
/// `/images` endpoint and the app's image page use). The scan may backfill
/// metadata/sizes for pre-existing rootfs, so it runs on a blocking thread.
async fn read_node_images(base: std::path::PathBuf) -> Vec<NodeImageInfo> {
    tokio::task::spawn_blocking(move || {
        crate::services::foundation::image_store::list(&base)
            .into_iter()
            .map(|meta| {
                let mut names = vec![meta.name.clone()];
                if !meta.manifest_digest.is_empty() {
                    names.push(format!("{}@{}", meta.name, meta.manifest_digest));
                }
                NodeImageInfo {
                    names,
                    size_bytes: meta.total_size(),
                }
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

#[cfg(unix)]
fn parse_meminfo_kb(line: &str) -> u64 {
    // Format: "MemTotal:       8192000 kB"
    line.split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn is_ready(memory: &MemoryInfo, battery: &BatteryInfo, network: &NetworkInfo) -> bool {
    let memory_ok = memory.available_bytes >= LOW_MEMORY_THRESHOLD_BYTES;
    let battery_ok = !battery_pressure(battery);
    let network_ok = network.network_type != "none";
    memory_ok && battery_ok && network_ok
}

fn battery_pressure(battery: &BatteryInfo) -> bool {
    battery.percent < LOW_BATTERY_THRESHOLD && !battery.charging
}
