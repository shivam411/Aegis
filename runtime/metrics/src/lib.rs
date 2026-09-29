//! Host metrics: CPU, memory, disk and load, sampled in the background so
//! reads are instant and CPU usage is measured over a real interval.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use sysinfo::{Disks, System};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct HostSnapshot {
    pub hostname: String,
    pub os: String,
    pub uptime_secs: u64,
    pub cpu_count: usize,
    /// Average over all cores since the previous sample, 0-100.
    pub cpu_percent: f32,
    /// 1, 5 and 15 minute load averages (0 on platforms without them).
    pub load_average: [f64; 3],
    pub memory_total_bytes: u64,
    pub memory_used_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
    /// The filesystem holding Aegis's data directory.
    pub disk_mount_point: String,
    pub disk_total_bytes: u64,
    pub disk_available_bytes: u64,
    pub sampled_at: String,
}

struct State {
    system: System,
    latest: Option<HostSnapshot>,
}

#[derive(Clone)]
pub struct HostMonitor {
    state: Arc<Mutex<State>>,
    disk_path: PathBuf,
}

impl HostMonitor {
    /// `disk_path` selects which filesystem to report (normally the data dir).
    pub fn new(disk_path: PathBuf) -> Self {
        let mut system = System::new();
        system.refresh_cpu_usage();
        Self {
            state: Arc::new(Mutex::new(State {
                system,
                latest: None,
            })),
            disk_path,
        }
    }

    /// Refreshes every `interval` in the background.
    pub fn start(&self, interval: Duration) -> tokio::task::JoinHandle<()> {
        let this = self.clone();
        tokio::spawn(async move {
            let mut tick =
                tokio::time::interval(interval.max(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL));
            loop {
                tick.tick().await;
                let monitor = this.clone();
                let _ = tokio::task::spawn_blocking(move || monitor.sample()).await;
            }
        })
    }

    /// Takes a fresh sample (CPU usage is relative to the previous one).
    pub fn sample(&self) -> HostSnapshot {
        let mut state = self.state.lock().unwrap();
        state.system.refresh_cpu_usage();
        state.system.refresh_memory();
        let (mount, total, available) = disk_for(&self.disk_path);
        let load = System::load_average();
        let snapshot = HostSnapshot {
            hostname: System::host_name().unwrap_or_default(),
            os: System::long_os_version().unwrap_or_default(),
            uptime_secs: System::uptime(),
            cpu_count: state.system.cpus().len(),
            cpu_percent: state.system.global_cpu_usage(),
            load_average: [load.one, load.five, load.fifteen],
            memory_total_bytes: state.system.total_memory(),
            memory_used_bytes: state.system.used_memory(),
            swap_total_bytes: state.system.total_swap(),
            swap_used_bytes: state.system.used_swap(),
            disk_mount_point: mount,
            disk_total_bytes: total,
            disk_available_bytes: available,
            sampled_at: chrono::Utc::now().to_rfc3339(),
        };
        state.latest = Some(snapshot.clone());
        snapshot
    }

    /// The most recent sample, taking one if there is none yet.
    pub fn latest(&self) -> HostSnapshot {
        let cached = self.state.lock().unwrap().latest.clone();
        cached.unwrap_or_else(|| self.sample())
    }
}

/// The disk whose mount point is the longest prefix of `path`.
fn disk_for(path: &Path) -> (String, u64, u64) {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let disks = Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|d| path.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| {
            (
                d.mount_point().to_string_lossy().to_string(),
                d.total_space(),
                d.available_space(),
            )
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_host_snapshot_is_plausible() {
        let dir = std::env::temp_dir();
        let monitor = HostMonitor::new(dir);
        let first = monitor.latest();
        assert!(first.cpu_count >= 1);
        assert!(first.memory_total_bytes > 0);
        assert!(first.memory_used_bytes <= first.memory_total_bytes);
        assert!((0.0..=100.0).contains(&first.cpu_percent));
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        let second = monitor.sample();
        assert!((0.0..=100.0).contains(&second.cpu_percent));
        assert_eq!(monitor.latest(), second);
        if cfg!(target_os = "linux") {
            assert!(second.disk_total_bytes > 0, "{second:?}");
            assert!(second.disk_available_bytes <= second.disk_total_bytes);
        }
    }
}
