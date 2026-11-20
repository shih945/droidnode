//! Local network helpers.
//!
//! The "UDP connect to 8.8.8.8" trick returns the address of the *default
//! route* interface, which on a phone is often the cellular one (e.g.
//! 10.84.x.x) even though the cluster reaches us over Wi-Fi. That produced
//! kubelet certificates whose only IP SAN was a cellular address, so
//! kube-apiserver rejected them with:
//!   x509: certificate is valid for 192.0.2.10, not 192.0.2.11
//! Enumerating the interfaces gives us every address to put in the SAN, and a
//! private one to advertise to the cluster.
//!
//! Interface names matter too: Wi-Fi Direct (`p2p*`), USB tethering
//! (`rndis*`/`usb*`) and tunnels (`tun*`) carry private addresses that the
//! control plane cannot route to. Advertising one of those makes the node
//! Ready but `kubectl logs/exec` time out and Service endpoints unreachable,
//! so they are skipped when a real LAN address exists.

#[cfg(unix)]
pub fn local_ipv4_interfaces() -> Vec<(String, String)> {
    use std::ffi::CStr;

    let mut out: Vec<(String, String)> = Vec::new();
    // SAFETY: standard getifaddrs/getnameinfo usage; the list is freed below.
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return out;
        }
        let mut cur = ifap;
        while !cur.is_null() {
            let addr = (*cur).ifa_addr;
            if !addr.is_null() && (*addr).sa_family as i32 == libc::AF_INET {
                // c_char is u8 on aarch64-android and i8 elsewhere, so use the
                // libc alias rather than a fixed signedness.
                let mut buf = [0 as libc::c_char; 64];
                // Android's getnameinfo takes size_t for the buffer lengths,
                // other unixes take socklen_t; u64/usize cast covers both.
                let rc = libc::getnameinfo(
                    addr,
                    std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                    buf.as_mut_ptr(),
                    buf.len() as _,
                    std::ptr::null_mut(),
                    0 as _,
                    libc::NI_NUMERICHOST,
                );
                if rc == 0 {
                    let ip = CStr::from_ptr(buf.as_ptr()).to_string_lossy().to_string();
                    let name = if (*cur).ifa_name.is_null() {
                        String::new()
                    } else {
                        CStr::from_ptr((*cur).ifa_name)
                            .to_string_lossy()
                            .to_string()
                    };
                    if ip != "127.0.0.1" && !out.iter().any(|(_, seen)| seen == &ip) {
                        out.push((name, ip));
                    }
                }
            }
            cur = (*cur).ifa_next;
        }
        libc::freeifaddrs(ifap);
    }
    out
}

#[cfg(not(unix))]
pub fn local_ipv4_interfaces() -> Vec<(String, String)> {
    Vec::new()
}

/// Every IPv4 address, for the kubelet certificate SAN list.
#[cfg(unix)]
pub fn local_ipv4_addresses() -> Vec<String> {
    local_ipv4_interfaces()
        .into_iter()
        .map(|(_, ip)| ip)
        .collect()
}

#[cfg(not(unix))]
pub fn local_ipv4_addresses() -> Vec<String> {
    Vec::new()
}

/// True for RFC1918 / link-local style addresses, which are the ones a LAN
/// cluster will actually use.
pub fn is_private(ip: &str) -> bool {
    if ip.starts_with("10.") || ip.starts_with("192.168.") {
        return true;
    }
    if let Some(rest) = ip.strip_prefix("172.") {
        if let Some(second) = rest.split('.').next() {
            if let Ok(n) = second.parse::<u8>() {
                return (16..=31).contains(&n);
            }
        }
    }
    false
}

/// Interfaces whose addresses the cluster cannot dial: Wi-Fi Direct, USB
/// tethering, and tunnels.
pub(crate) fn is_tether_interface(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["p2p", "rndis", "usb", "tun", "tap", "dummy", "ip6tnl"]
        .iter()
        .any(|prefix| n.starts_with(prefix))
}

/// Read a value from the environment, falling back to `$DROIDNODE_DATA_DIR/<file>`
/// (`KEY=VALUE` lines, `#` comments). Empty values count as unset.
fn env_or_file(key: &str, file: &str) -> Option<String> {
    if let Some(value) = crate::config::env_value(key) {
        return Some(value);
    }
    let dir = crate::config::get().config_dir();
    let path = std::path::PathBuf::from(dir).join(file);
    let text = std::fs::read_to_string(path).ok()?;
    text.lines().find_map(|line| {
        let line = line.trim();
        if line.starts_with('#') {
            return None;
        }
        let (key_in_file, value) = line.split_once('=')?;
        if key_in_file.trim() != key {
            return None;
        }
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

/// Registries that should be reached over plain HTTP.
///
/// Configured with `DROIDNODE_INSECURE_REGISTRIES=host:port,other:port`
/// (comma separated), or the same key in `$DROIDNODE_DATA_DIR/registry-mirrors.env`.
/// Intended for a local registry during bring-up; production should use TLS.
pub fn is_insecure_registry(host: &str) -> bool {
    let list =
        env_or_file("DROIDNODE_INSECURE_REGISTRIES", "registry-mirrors.env").unwrap_or_default();
    list.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .any(|entry| entry.eq_ignore_ascii_case(host))
}

/// A registry addressed by a private/loopback IP literal is assumed to be a
/// LAN registry without TLS (see ImageRef::registry_url). Hostnames are not
/// guessed: they must be listed in DROIDNODE_INSECURE_REGISTRIES explicitly.
pub fn is_private_registry(host: &str) -> bool {
    let ip = host.rsplit_once(':').map(|(head, _)| head).unwrap_or(host);
    ip.parse::<std::net::IpAddr>()
        .map(|addr| match addr {
            std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
            std::net::IpAddr::V6(v6) => v6.is_loopback(),
        })
        .unwrap_or(false)
}

/// Interface the operator pinned in the app, or `None` for automatic selection.
///
/// Set with `DROIDNODE_NODE_IFACE=wlan0` (or a literal IP address) in
/// `$DROIDNODE_DATA_DIR/network-preference.env`. A multi-homed phone can put
/// Wi-Fi Direct / USB tethering interfaces ahead of the LAN one, so letting the
/// user choose is more reliable than any heuristic.
pub fn preferred_interface() -> Option<String> {
    env_or_file("DROIDNODE_NODE_IFACE", "network-preference.env")
}

/// 只在结果变化时打日志：`primary_ipv4` 每次心跳都会调用。
fn note_pin(interface: &str, ip: &str) {
    let state = format!("pinned {interface}={ip}");
    if remember(&state) {
        tracing::info!(
            interface = %interface,
            ip = %ip,
            "node address pinned by network preference"
        );
    }
}

fn note_pin_fallback(preference: &str) {
    let state = format!("fallback {preference}");
    if remember(&state) {
        tracing::warn!(
            preference = %preference,
            "preferred interface has no IPv4 address right now; falling back to automatic selection"
        );
    }
}

fn remember(state: &str) -> bool {
    use std::sync::{Mutex, OnceLock};
    static LAST: OnceLock<Mutex<String>> = OnceLock::new();
    let mut last = LAST
        .get_or_init(|| Mutex::new(String::new()))
        .lock()
        .unwrap();
    if *last == state {
        return false;
    }
    *last = state.to_string();
    true
}

/// Best address to advertise for this node: the interface the operator pinned
/// when it still has an address, otherwise a private IPv4 on a real LAN
/// interface, then any private address (cellular), then whatever the default
/// route reports, then loopback.
pub fn primary_ipv4() -> String {
    let ifaces = local_ipv4_interfaces();

    if let Some(preferred) = preferred_interface() {
        // Accept either an interface name (wlan0) or a literal address.
        let by_name = ifaces
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(&preferred));
        let by_ip = ifaces.iter().find(|(_, ip)| ip == &preferred);
        if let Some((name, ip)) = by_name.or(by_ip) {
            note_pin(name, ip);
            return ip.clone();
        }
        note_pin_fallback(&preferred);
    }

    let private_lan = ifaces
        .iter()
        .filter(|(name, _)| !is_tether_interface(name))
        .map(|(_, ip)| ip)
        .find(|ip| is_private(ip))
        .cloned();
    if let Some(ip) = private_lan {
        return ip;
    }
    if let Some((_, ip)) = ifaces.iter().find(|(_, ip)| is_private(ip)) {
        return ip.clone();
    }
    if let Some((_, ip)) = ifaces.first() {
        return ip.clone();
    }
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_registries_are_detected() {
        assert!(is_private_registry("192.168.0.10:5000"));
        assert!(is_private_registry("10.0.0.5"));
        assert!(is_private_registry("127.0.0.1:5000"));
        assert!(!is_private_registry("registry-1.docker.io"));
        assert!(!is_private_registry("8.8.8.8:5000"));
    }

    #[test]
    fn private_ranges_are_detected() {
        for ip in ["10.1.2.3", "192.168.0.5", "172.16.0.1", "172.31.255.254"] {
            assert!(is_private(ip), "{ip} should be private");
        }
    }

    #[test]
    fn public_ranges_are_rejected() {
        for ip in ["8.8.8.8", "172.15.1.1", "172.32.0.1", "100.64.0.1"] {
            assert!(!is_private(ip), "{ip} should not be private");
        }
    }

    #[test]
    fn tether_and_tunnel_interfaces_are_skipped() {
        for name in [
            "p2p0", "rndis0", "usb0", "tun0", "tap1", "dummy0", "ip6tnl0",
        ] {
            assert!(is_tether_interface(name), "{name} should be skipped");
        }
    }

    #[test]
    fn wifi_and_cellular_interfaces_are_kept() {
        for name in ["wlan0", "wlan1", "eth0", "ccmni0", "rmnet_data0"] {
            assert!(!is_tether_interface(name), "{name} should be usable");
        }
    }
}
