//! Minimal `/proc` based resource accounting for the kubelet resource metrics
//! endpoint (`/metrics/resource`), which is what metrics-server scrapes to power
//! `kubectl top node` / `kubectl top pod`.
//!
//! Everything here is O(processes) per scrape and is expected to be called by
//! metrics-server roughly every 15-60s.

/// Clock ticks per second (`_SC_CLK_TCK`), usually 100.
pub fn clock_ticks() -> f64 {
    #[cfg(unix)]
    {
        let t = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if t > 0 {
            return t as f64;
        }
    }
    100.0
}

/// Memory page size in bytes.
pub fn page_size() -> u64 {
    #[cfg(unix)]
    {
        let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if p > 0 {
            return p as u64;
        }
    }
    4096
}

/// Cumulative CPU time consumed by the node, in seconds.
///
/// Preferred source is /proc/stat. **Android denies it to apps** (SELinux, even
/// though /proc/meminfo is readable), so we fall back to summing the CPU time of
/// every process we can see - which on a phone node is exactly the agent plus its
/// containers. metrics-server only needs a monotonically increasing counter.
pub fn node_cpu_seconds() -> Option<f64> {
    if let Ok(stat) = std::fs::read_to_string("/proc/stat") {
        if let Some(line) = stat.lines().find(|l| l.starts_with("cpu ")) {
            let total: u64 = line
                .split_whitespace()
                .skip(1)
                .filter_map(|f| f.parse::<u64>().ok())
                .sum();
            if total > 0 {
                return Some(total as f64 / clock_ticks());
            }
        }
    }

    let entries = std::fs::read_dir("/proc").ok()?;
    let mut ticks = 0u64;
    let mut seen = 0usize;
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        if let Some(st) = read_proc_stat(pid) {
            ticks += st.utime + st.stime;
            seen += 1;
        }
    }
    if seen == 0 {
        None
    } else {
        Some(ticks as f64 / clock_ticks())
    }
}

/// (MemTotal, MemAvailable) in bytes from /proc/meminfo.
pub fn node_memory() -> Option<(u64, u64)> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut total = 0u64;
    let mut available = 0u64;
    for line in meminfo.lines() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("MemTotal:") => {
                total = it.next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) * 1024
            }
            Some("MemAvailable:") => {
                available = it.next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) * 1024
            }
            _ => {}
        }
    }
    if total == 0 {
        None
    } else {
        Some((total, available))
    }
}

struct ProcStat {
    pgrp: i32,
    utime: u64,
    stime: u64,
    rss_pages: i64,
}

/// Parse the interesting fields out of /proc/<pid>/stat.
///
/// The `comm` field is wrapped in parentheses and may itself contain spaces or
/// parentheses, so everything up to the LAST ')' is skipped.
fn read_proc_stat(pid: i32) -> Option<ProcStat> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &raw[raw.rfind(')')? + 1..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    // After comm: state(3) ppid(4) pgrp(5) ... utime(14) stime(15) ... rss(24)
    Some(ProcStat {
        pgrp: f.get(2)?.parse().ok()?,
        utime: f.get(11)?.parse().ok()?,
        stime: f.get(12)?.parse().ok()?,
        rss_pages: f.get(21)?.parse().ok()?,
    })
}

/// Cumulative CPU seconds and current RSS of every process in `pgid`.
///
/// proot is started as its own process group (see ProotBroker), so the whole
/// container process tree shares one pgid.
pub fn process_group_usage(pgid: i32) -> (f64, u64) {
    let ticks = clock_ticks();
    let page = page_size();
    let mut cpu_ticks = 0u64;
    let mut rss = 0u64;

    let Ok(entries) = std::fs::read_dir("/proc") else {
        return (0.0, 0);
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Ok(pid) = name.to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Some(st) = read_proc_stat(pid) else {
            continue;
        };
        if st.pgrp != pgid {
            continue;
        }
        cpu_ticks += st.utime + st.stime;
        if st.rss_pages > 0 {
            rss += st.rss_pages as u64 * page;
        }
    }
    (cpu_ticks as f64 / ticks, rss)
}
