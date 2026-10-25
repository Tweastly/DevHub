//! The server itself: CPU, memory, disk, uptime. /proc/stat, /proc/meminfo and
//! /proc/uptime are not namespaced, so they describe the whole VPS even from inside
//! the DevHub container; the disk is measured on the data directory (a host bind mount).

use bollard::Docker;

#[derive(Clone, Default)]
pub struct Host {
    pub name: String,
    pub ncpu: i64,
    pub cpu_pct: f64,
    pub mem_used_mb: i64,
    pub mem_total_mb: i64,
    pub disk_used: u64,
    pub disk_total: u64,
    pub uptime_secs: i64,
    pub images_bytes: i64,
}

impl Host {
    pub fn disk_pct(&self) -> f64 {
        if self.disk_total == 0 { 0.0 } else { self.disk_used as f64 / self.disk_total as f64 * 100.0 }
    }
}

/// Keeps the previous /proc/stat sample to compute CPU usage between two reads.
#[derive(Default)]
pub struct Sampler {
    prev: Option<(u64, u64)>,
    tick: u64,
}

fn cpu_times() -> Option<(u64, u64)> {
    let s = std::fs::read_to_string("/proc/stat").ok()?;
    let v: Vec<u64> = s.lines().next()?.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
    let idle = v.get(3)? + v.get(4).copied().unwrap_or(0);
    Some((v.iter().sum(), idle))
}

fn meminfo() -> (i64, i64) {
    let s = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let field = |k: &str| {
        s.lines()
            .find(|l| l.starts_with(k))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
            / 1024
    };
    let total = field("MemTotal:");
    (total - field("MemAvailable:"), total)
}

fn disk(path: &str) -> (u64, u64) {
    let Ok(c) = std::ffi::CString::new(path) else { return (0, 0) };
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return (0, 0);
    }
    let total = s.f_blocks as u64 * s.f_frsize as u64;
    let free = s.f_bavail as u64 * s.f_frsize as u64;
    (total - free, total)
}

impl Sampler {
    /// Refreshes `h`. Cheap reads every call; Docker info/df only every ~5 min.
    pub async fn sample(&mut self, d: &Docker, data_dir: &str, h: &mut Host) {
        if let Some((total, idle)) = cpu_times() {
            if let Some((pt, pi)) = self.prev {
                let dt = total.saturating_sub(pt) as f64;
                if dt > 0.0 {
                    h.cpu_pct = (1.0 - idle.saturating_sub(pi) as f64 / dt) * 100.0;
                }
            }
            self.prev = Some((total, idle));
        }
        (h.mem_used_mb, h.mem_total_mb) = meminfo();
        (h.disk_used, h.disk_total) = disk(data_dir);
        h.uptime_secs = std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|s| s.split('.').next().and_then(|x| x.parse().ok()))
            .unwrap_or(0);
        if self.tick % 300 == 0 {
            if let Ok(i) = d.info().await {
                h.name = i.name.unwrap_or_else(|| "serveur".into());
                h.ncpu = i.ncpu.unwrap_or(1);
            }
            if let Ok(df) = d.df().await {
                h.images_bytes = df.images.unwrap_or_default().iter().map(|i| i.size).sum();
            }
        }
        self.tick += 1;
    }
}
