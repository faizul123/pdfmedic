//! Process-level resource metrics (Linux, read from /proc).

/// Peak resident set size (VmHWM) in KiB; 0 if unavailable.
pub fn peak_rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

/// User + system CPU time of this process in milliseconds; 0 if unavailable.
/// Assumes the usual 100 ticks/second.
pub fn process_cpu_ms() -> f64 {
    let Ok(s) = std::fs::read_to_string("/proc/self/stat") else { return 0.0 };
    // Fields after the ")" of the comm field; utime=14th, stime=15th overall.
    let Some(rest) = s.rsplit_once(')').map(|(_, r)| r) else { return 0.0 };
    let f: Vec<&str> = rest.split_whitespace().collect();
    let utime: f64 = f.get(11).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let stime: f64 = f.get(12).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    (utime + stime) * 10.0
}
