//! D3: make ClusterIP Services usable from inside proot containers.
//!
//! Containers share the device network namespace and cannot bind a 10.43.x.x
//! address (no CAP_NET_ADMIN, no netns). Instead we give every Service a
//! deterministic 127.10.x.y loopback address, listen on `servicePort` there,
//! and proxy to a Ready EndpointSlice address. Each Pod's `/etc/hosts` maps
//! `svc`, `svc.ns`, `svc.ns.svc` and the FQDN to that loopback address.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::brokers::{ControlPlaneBroker, EndpointInfo, ServiceInfo};
use crate::exposers::virtual_kubelet_exposer::KubeletState;

const RECONCILE_SECS: u64 = 10;

struct Proxy {
    targets: Arc<RwLock<Vec<(String, u16)>>>,
    task: JoinHandle<()>,
}

/// D3 reconcile loop. Spawn this once from main.
pub async fn run(cp: Arc<dyn ControlPlaneBroker>, state: KubeletState) {
    let mut proxies: HashMap<(String, u16), Proxy> = HashMap::new();
    // Privileged ports (<1024) cannot be bound without root; report each once.
    let mut skipped: std::collections::HashSet<(String, u16)> = std::collections::HashSet::new();
    // EndpointSlice errors are reported once, not every 10 s.
    let mut endpoints_error_reported = false;
    loop {
        let services = match cp.list_all_services().await {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "D3: could not list services");
                tokio::time::sleep(Duration::from_secs(RECONCILE_SECS)).await;
                continue;
            }
        };
        // Do not silently degrade to "every Service has no backend": that looks
        // like a working name that answers nothing, and it is what a missing
        // `discovery.k8s.io/endpointslices` grant in the agent ClusterRole
        // produces. Keep the previous routes and say why.
        let endpoints = match cp.list_ready_endpoints().await {
            Ok(e) => {
                endpoints_error_reported = false;
                e
            }
            Err(e) => {
                if !endpoints_error_reported {
                    warn!(
                        error = %e,
                        "D3: could not list endpointslices - Service proxies keep their last \
                         backends. The agent ClusterRole needs \
                         discovery.k8s.io/endpointslices: get,list,watch"
                    );
                    endpoints_error_reported = true;
                }
                tokio::time::sleep(Duration::from_secs(RECONCILE_SECS)).await;
                continue;
            }
        };

        let mut desired: HashMap<(String, u16), Vec<(String, u16)>> = HashMap::new();
        for svc in &services {
            let ip = loopback_ip(&svc.namespace, &svc.name);
            for (service_port, target_port) in &svc.service_ports {
                if *service_port <= 0 || *target_port <= 0 {
                    continue;
                }
                let targets = endpoint_targets(&endpoints, svc, *target_port);
                desired.insert((ip.clone(), *service_port as u16), targets);
            }
        }

        // Start listeners for new services / update backends.
        for (key, targets) in &desired {
            if let Some(proxy) = proxies.get(key) {
                *proxy.targets.write().await = targets.clone();
                continue;
            }
            let ip = key.0.clone();
            let port = key.1;
            let listener = match TcpListener::bind((ip.as_str(), port)).await {
                Ok(l) => l,
                Err(e) => {
                    if skipped.insert(key.clone()) {
                        let hint = if port < 1024 {
                            " (privileged port: use a NodePort for this service)"
                        } else {
                            ""
                        };
                        warn!(ip = %ip, port, error = %e, "D3: loopback listener bind failed{}", hint);
                    }
                    continue;
                }
            };
            let shared = Arc::new(RwLock::new(targets.clone()));
            let task_shared = Arc::clone(&shared);
            let task = tokio::spawn(async move {
                let rr = AtomicUsize::new(0);
                loop {
                    let Ok((mut client, _)) = listener.accept().await else {
                        break;
                    };
                    let targets = Arc::clone(&task_shared);
                    let idx = rr.fetch_add(1, Ordering::Relaxed);
                    tokio::spawn(async move {
                        let list = targets.read().await.clone();
                        if list.is_empty() {
                            return;
                        }
                        let (addr, port) = &list[idx % list.len()];
                        let upstream = format!("{addr}:{port}");
                        match TcpStream::connect(&upstream).await {
                            Ok(mut upstream) => {
                                let _ =
                                    tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                            }
                            Err(e) => {
                                debug!(upstream, error = %e, "D3: backend connect failed");
                            }
                        }
                    });
                }
            });
            skipped.remove(key);
            info!(ip = %ip, port, targets = targets.len(), "D3: service proxy listening");
            proxies.insert(
                key.clone(),
                Proxy {
                    targets: shared,
                    task,
                },
            );
        }

        // Remove proxies whose Service/port disappeared.
        let stale: Vec<(String, u16)> = proxies
            .keys()
            .filter(|k| !desired.contains_key(*k))
            .cloned()
            .collect();
        for key in &stale {
            skipped.remove(key);
        }
        for key in stale {
            if let Some(proxy) = proxies.remove(&key) {
                proxy.task.abort();
                info!(ip = %key.0, port = key.1, "D3: service proxy removed");
            }
        }

        write_hosts(&state, &services).await;
        tokio::time::sleep(Duration::from_secs(RECONCILE_SECS)).await;
    }
}

fn endpoint_targets(
    endpoints: &[EndpointInfo],
    svc: &ServiceInfo,
    target_port: i32,
) -> Vec<(String, u16)> {
    let mut out = Vec::new();
    for ep in endpoints {
        if ep.namespace != svc.namespace || ep.service != svc.name || ep.port != target_port {
            continue;
        }
        for addr in &ep.addresses {
            if !out.iter().any(|(a, _)| a == addr) {
                out.push((addr.clone(), target_port as u16));
            }
        }
    }
    out
}

/// Deterministic loopback address for a Service: 127.10.x.y.
fn loopback_ip(namespace: &str, name: &str) -> String {
    let mut hash: u32 = 2_166_136_261;
    for byte in format!("{namespace}/{name}").bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(16_777_619);
    }
    let a = 1 + ((hash >> 16) & 0x0f) as u8;
    let c = 1 + (hash & 0xfe) as u8;
    format!("127.10.{a}.{c}")
}

const HOSTS_BEGIN: &str = "# droidnode-services-begin";
const HOSTS_END: &str = "# droidnode-services-end";

/// `/etc/hosts` block for the cluster services (pure; unit-tested).
pub(crate) fn hosts_block(services: &[ServiceInfo]) -> String {
    let mut block = String::from(HOSTS_BEGIN);
    block.push('\n');
    for svc in services {
        let ip = loopback_ip(&svc.namespace, &svc.name);
        let names = [
            svc.name.clone(),
            format!("{}.{}", svc.name, svc.namespace),
            format!("{}.{}.svc", svc.name, svc.namespace),
            format!("{}.{}.svc.cluster.local", svc.name, svc.namespace),
        ];
        for name in names {
            block.push_str(&format!("{ip} {name}\n"));
        }
    }
    block.push_str(HOSTS_END);
    block.push('\n');
    block
}

async fn write_hosts(state: &KubeletState, services: &[ServiceInfo]) {
    let block = hosts_block(services);

    let rootfs: Vec<std::path::PathBuf> = state
        .exec_targets
        .read()
        .await
        .values()
        .map(|t| t.rootfs.clone())
        .collect();
    for rootfs in rootfs {
        let path = rootfs.join("etc/hosts");
        let base = match tokio::fs::read_to_string(&path).await {
            Ok(v) => v,
            Err(_) => "127.0.0.1 localhost\n::1 localhost\n".to_string(),
        };
        let cleaned = remove_block(&base);
        let next = format!("{cleaned}{block}");
        if next != base {
            if let Err(e) = tokio::fs::write(&path, next).await {
                warn!(path = %path.display(), error = %e, "D3: could not update hosts");
            }
        }
    }
}

fn remove_block(text: &str) -> String {
    let Some(start) = text.find(HOSTS_BEGIN) else {
        return text.to_string();
    };
    let Some(end_rel) = text[start..].find(HOSTS_END) else {
        return text.to_string();
    };
    let end = start + end_rel + HOSTS_END.len();
    let mut out = String::with_capacity(text.len());
    out.push_str(text[..start].trim_end());
    out.push('\n');
    out.push_str(text[end..].trim_start());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(namespace: &str, name: &str) -> ServiceInfo {
        ServiceInfo {
            namespace: namespace.into(),
            name: name.into(),
            cluster_ip: String::new(),
            ports: vec![],
            service_ports: vec![],
        }
    }

    #[test]
    fn hosts_block_lists_every_service_alias() {
        let block = hosts_block(&[svc("default", "web")]);
        assert!(block.starts_with(HOSTS_BEGIN));
        assert!(block.trim_end().ends_with(HOSTS_END));
        for name in [
            "web",
            "web.default",
            "web.default.svc",
            "web.default.svc.cluster.local",
        ] {
            assert!(block.contains(&format!(" {name}\n")), "missing {name}");
        }
        assert!(block.contains("127.10."));
    }

    #[test]
    fn hosts_block_is_deterministic() {
        let a = hosts_block(&[svc("default", "web"), svc("kube-system", "dns")]);
        let b = hosts_block(&[svc("default", "web"), svc("kube-system", "dns")]);
        assert_eq!(a, b);
        assert!(a.contains("dns.kube-system.svc.cluster.local"));
    }
}
