//! Per-app resource limits with Linux cgroups.
//!
//! Every supervised process gets its own cgroup, created just before it
//! starts (the child joins it before running the app's command, so anything
//! it forks is covered too). Limits are set per project and written to the
//! cgroups of all of that project's processes; changing them takes effect
//! immediately, without a restart, and they are applied again on every start.
//!
//! Backends, chosen at startup:
//!
//! - **cgroup v2** (current distributions). Under the systemd unit that
//!   `install.sh --server` writes (`Delegate=yes`), the daemon moves itself
//!   into `<its cgroup>/daemon` and creates apps under `<its cgroup>/apps`.
//!   Run as root outside such a unit, it uses `/sys/fs/cgroup/aegis`.
//! - **cgroup v1** (older distributions and some container hosts), as root:
//!   `<controller mount>/<the daemon's cgroup>/aegis/<process>`.
//! - **none**: no limits. Metrics still work from `/proc`; the reason is
//!   reported so the UI can explain why limits are unavailable.

use aegis_process::{Placement, SpawnHook};
use aegis_types::{ProcessId, ProjectId};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// CPU period used for quotas (100 ms, the kernel default).
const CPU_PERIOD_USEC: u64 = 100_000;
/// Smallest CPU limit: 1% of one core.
pub const MIN_CPU_CORES: f64 = 0.01;
/// Smallest memory limit: 8 MiB.
pub const MIN_MEMORY_BYTES: u64 = 8 * 1024 * 1024;
/// Values at or above this in v1 memory files mean "unlimited".
const V1_UNLIMITED: u64 = 1 << 60;

pub use aegis_config::ResourceLimits;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum BackendKind {
    CgroupV2,
    CgroupV1,
    None,
}

impl BackendKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CgroupV2 => "cgroup2",
            Self::CgroupV1 => "cgroup1",
            Self::None => "none",
        }
    }
}

/// What this host supports, for the API and UI.
#[derive(Debug, Clone, Serialize)]
pub struct Capabilities {
    pub backend: BackendKind,
    pub cpu: bool,
    pub memory: bool,
    pub pids: bool,
    /// Where app cgroups are created.
    pub location: Option<PathBuf>,
    /// Why limits are unavailable (or partly so).
    pub reason: Option<String>,
}

impl Capabilities {
    pub fn any(&self) -> bool {
        self.cpu || self.memory || self.pids
    }
}

/// Counters read from a process's cgroup.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CgroupStats {
    /// Total CPU time used, in microseconds.
    pub cpu_usage_usec: u64,
    /// CPU quota periods, and how many of them hit the limit.
    pub nr_periods: u64,
    pub nr_throttled: u64,
    pub throttled_usec: u64,
    /// Memory in use, excluding page cache that can be dropped at once.
    pub memory_bytes: u64,
    /// The hard limit in effect.
    pub memory_limit_bytes: Option<u64>,
    /// Times the kernel killed a process here for exceeding the limit.
    pub oom_kills: u64,
    /// Times usage went over the soft limit (v2) or hit the limit (v1).
    pub memory_high_events: u64,
    pub pids: Option<u64>,
    /// Processes in the cgroup.
    pub procs: Vec<u32>,
}

/// How to find cgroups.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Mode {
    /// Detect (see the module docs).
    #[default]
    Auto,
    /// Never use cgroups.
    Off,
    /// cgroup v2: create app cgroups under this directory, which must have
    /// controllers enabled for its children.
    V2At(PathBuf),
    /// cgroup v1: create app cgroups under these per-controller directories.
    V1At(V1Dirs),
}

/// Per-controller directories (cgroup v1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct V1Dirs {
    pub memory: Option<PathBuf>,
    pub cpu: Option<PathBuf>,
    pub cpuacct: Option<PathBuf>,
    pub pids: Option<PathBuf>,
}

impl V1Dirs {
    fn all(&self) -> impl Iterator<Item = &PathBuf> {
        [&self.memory, &self.cpu, &self.cpuacct, &self.pids]
            .into_iter()
            .flatten()
    }

    fn child(&self, name: &str) -> V1Dirs {
        V1Dirs {
            memory: self.memory.as_ref().map(|d| d.join(name)),
            cpu: self.cpu.as_ref().map(|d| d.join(name)),
            cpuacct: self.cpuacct.as_ref().map(|d| d.join(name)),
            pids: self.pids.as_ref().map(|d| d.join(name)),
        }
    }
}

#[derive(Debug, Clone)]
enum Backend {
    V2 {
        parent: PathBuf,
        cpu: bool,
        memory: bool,
        pids: bool,
    },
    V1(V1Dirs),
    None(String),
}

/// One process's cgroup.
#[derive(Debug, Clone)]
enum Group {
    V2(PathBuf),
    V1(V1Dirs),
}

impl Group {
    fn join_files(&self) -> Vec<PathBuf> {
        match self {
            Group::V2(dir) => vec![dir.join("cgroup.procs")],
            Group::V1(dirs) => dirs.all().map(|d| d.join("cgroup.procs")).collect(),
        }
    }

    fn dirs(&self) -> Vec<PathBuf> {
        match self {
            Group::V2(dir) => vec![dir.clone()],
            Group::V1(dirs) => dirs.all().cloned().collect(),
        }
    }
}

#[derive(Default)]
struct State {
    limits: HashMap<ProjectId, ResourceLimits>,
    groups: HashMap<ProcessId, (ProjectId, Group)>,
}

/// Creates app cgroups and applies limits. Install it on the supervisor
/// with `ProcessSupervisor::set_spawn_hook`.
pub struct ResourceManager {
    backend: Backend,
    state: Mutex<State>,
}

impl ResourceManager {
    pub fn new(mode: &Mode) -> Self {
        let backend = match mode {
            Mode::Off => Backend::None("Resource limits are turned off in the configuration ([resources] cgroups = \"off\")".into()),
            Mode::V2At(dir) => v2_at(dir),
            Mode::V1At(dirs) => v1_at(dirs.clone()),
            Mode::Auto => detect(),
        };
        match &backend {
            Backend::None(reason) => tracing::info!(%reason, "Resource limits unavailable"),
            Backend::V2 { parent, .. } => {
                tracing::info!(location = %parent.display(), "Resource limits: cgroup v2")
            }
            Backend::V1(dirs) => tracing::info!(
                location = %dirs.memory.as_ref().or(dirs.cpu.as_ref()).map(|p| p.display().to_string()).unwrap_or_default(),
                "Resource limits: cgroup v1"
            ),
        }
        let manager = Self {
            backend,
            state: Mutex::new(State::default()),
        };
        manager.remove_leftovers();
        manager
    }

    pub fn capabilities(&self) -> Capabilities {
        match &self.backend {
            Backend::V2 {
                parent,
                cpu,
                memory,
                pids,
            } => Capabilities {
                backend: BackendKind::CgroupV2,
                cpu: *cpu,
                memory: *memory,
                pids: *pids,
                location: Some(parent.clone()),
                reason: missing_reason(*cpu, *memory),
            },
            Backend::V1(dirs) => Capabilities {
                backend: BackendKind::CgroupV1,
                cpu: dirs.cpu.is_some(),
                memory: dirs.memory.is_some(),
                pids: dirs.pids.is_some(),
                location: dirs.memory.clone().or(dirs.cpu.clone()),
                reason: missing_reason(dirs.cpu.is_some(), dirs.memory.is_some()),
            },
            Backend::None(reason) => Capabilities {
                backend: BackendKind::None,
                cpu: false,
                memory: false,
                pids: false,
                location: None,
                reason: Some(reason.clone()),
            },
        }
    }

    /// Limits for a project, without applying them (e.g. restored from
    /// history at startup, before any process runs).
    pub fn load_limits(&self, project_id: ProjectId, limits: ResourceLimits) {
        self.state.lock().unwrap().limits.insert(project_id, limits);
    }

    pub fn limits(&self, project_id: &ProjectId) -> ResourceLimits {
        self.state
            .lock()
            .unwrap()
            .limits
            .get(project_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Stores the project's limits and writes them to the cgroups of its
    /// running processes. Returns how many processes they were applied to.
    pub fn set_limits(
        &self,
        project_id: ProjectId,
        limits: ResourceLimits,
    ) -> Result<usize, anyhow::Error> {
        let caps = self.capabilities();
        if limits.cpu_cores.is_some() && !caps.cpu {
            anyhow::bail!("CPU limits are not available on this host");
        }
        if limits.memory_bytes.is_some() && !caps.memory {
            anyhow::bail!("Memory limits are not available on this host");
        }
        if limits.pids_max.is_some() && !caps.pids {
            anyhow::bail!("Process-count limits are not available on this host");
        }
        let groups: Vec<Group> = {
            let mut state = self.state.lock().unwrap();
            state.limits.insert(project_id, limits.clone());
            state
                .groups
                .values()
                .filter(|(p, _)| *p == project_id)
                .map(|(_, g)| g.clone())
                .collect()
        };
        let mut errors = Vec::new();
        for group in &groups {
            if let Err(e) = self.apply(group, &limits) {
                errors.push(e.to_string());
            }
        }
        if !errors.is_empty() {
            anyhow::bail!("{}", errors.join("; "));
        }
        Ok(groups.len())
    }

    /// Counters for a process, if it has a cgroup.
    pub fn stats(&self, process_id: &ProcessId) -> Option<CgroupStats> {
        let group = self
            .state
            .lock()
            .unwrap()
            .groups
            .get(process_id)
            .map(|(_, g)| g.clone())?;
        Some(read_stats(&group))
    }

    fn create(&self, process_id: &ProcessId) -> Option<Group> {
        let name = process_id.to_string();
        match &self.backend {
            Backend::V2 { parent, .. } => Some(Group::V2(parent.join(name))),
            Backend::V1(dirs) => Some(Group::V1(dirs.child(&name))),
            Backend::None(_) => None,
        }
    }

    fn apply(&self, group: &Group, limits: &ResourceLimits) -> Result<(), anyhow::Error> {
        match (group, &self.backend) {
            (
                Group::V2(dir),
                Backend::V2 {
                    cpu, memory, pids, ..
                },
            ) => apply_v2(dir, limits, *cpu, *memory, *pids),
            (Group::V1(dirs), _) => apply_v1(dirs, limits),
            _ => Ok(()),
        }
    }

    /// Removes empty app cgroups left by an earlier run.
    fn remove_leftovers(&self) {
        let parents: Vec<PathBuf> = match &self.backend {
            Backend::V2 { parent, .. } => vec![parent.clone()],
            Backend::V1(dirs) => dirs.all().cloned().collect(),
            Backend::None(_) => vec![],
        };
        for parent in parents {
            if let Ok(entries) = std::fs::read_dir(&parent) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let is_ours = entry
                        .file_name()
                        .to_str()
                        .is_some_and(|n| n.parse::<ProcessId>().is_ok());
                    if is_ours && path.is_dir() {
                        let _ = std::fs::remove_dir(&path);
                    }
                }
            }
        }
    }
}

fn missing_reason(cpu: bool, memory: bool) -> Option<String> {
    match (cpu, memory) {
        (true, true) => None,
        (false, true) => {
            Some("The cpu controller is not available, so CPU limits are disabled".into())
        }
        (true, false) => {
            Some("The memory controller is not available, so memory limits are disabled".into())
        }
        (false, false) => Some("Neither the cpu nor the memory controller is available".into()),
    }
}

impl SpawnHook for ResourceManager {
    fn prepare(&self, process_id: &ProcessId, project_id: &ProjectId) -> Placement {
        let Some(group) = self.create(process_id) else {
            return Placement::default();
        };
        let mut placement = Placement::default();
        let mut ok = true;
        for dir in group.dirs() {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                placement.warnings.push(format!(
                    "Could not create cgroup {}: {}",
                    dir.display(),
                    e
                ));
                ok = false;
            }
        }
        if !ok {
            return placement;
        }
        let limits = self.limits(project_id);
        if let Err(e) = self.apply(&group, &limits) {
            placement
                .warnings
                .push(format!("Could not apply resource limits: {}", e));
        }
        placement.join_files = group.join_files();
        self.state
            .lock()
            .unwrap()
            .groups
            .insert(*process_id, (*project_id, group));
        placement
    }

    fn finished(&self, process_id: &ProcessId) {
        let Some((_, group)) = self.state.lock().unwrap().groups.remove(process_id) else {
            return;
        };
        // The kernel releases a cgroup once its last process has been
        // reaped, which can take a moment after the kill.
        std::thread::spawn(move || {
            for dir in group.dirs() {
                for _ in 0..40 {
                    if std::fs::remove_dir(&dir).is_ok() || !dir.exists() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        });
    }
}

// ----------------------------------------------------------------------
// Detection
// ----------------------------------------------------------------------

const CGROUP_ROOT: &str = "/sys/fs/cgroup";

fn detect() -> Backend {
    if !cfg!(target_os = "linux") {
        return Backend::None(
            "Resource limits need Linux cgroups; metrics are still collected".into(),
        );
    }
    let root = Path::new(CGROUP_ROOT);
    let own = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    if root.join("cgroup.controllers").exists() {
        return detect_v2(root, &own);
    }
    if root.join("memory").join("memory.limit_in_bytes").exists()
        || root.join("cpu").join("cpu.cfs_quota_us").exists()
    {
        if !is_root() {
            return Backend::None(
                "This host uses cgroup v1, where resource limits need the daemon to run as root"
                    .into(),
            );
        }
        let mut dirs = V1Dirs::default();
        for (name, slot) in [
            ("memory", &mut dirs.memory),
            ("cpu", &mut dirs.cpu),
            ("cpuacct", &mut dirs.cpuacct),
            ("pids", &mut dirs.pids),
        ] {
            let mount = root.join(name);
            if !mount.exists() {
                continue;
            }
            let own_path = own_cgroup_v1(&own, name).unwrap_or_else(|| "/".into());
            let dir = mount.join(own_path.trim_start_matches('/')).join("aegis");
            if std::fs::create_dir_all(&dir).is_ok() {
                *slot = Some(dir);
            }
        }
        return v1_at(dirs);
    }
    Backend::None("No usable cgroup hierarchy was found at /sys/fs/cgroup".into())
}

fn detect_v2(root: &Path, own: &str) -> Backend {
    let own_path = own
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .unwrap_or("/")
        .trim()
        .to_string();
    // Under systemd with Delegate=yes the service's cgroup is ours to manage.
    let delegated = std::env::var_os("INVOCATION_ID").is_some() && own_path.ends_with(".service");
    if delegated {
        let service = root.join(own_path.trim_start_matches('/'));
        match delegate(&service) {
            Ok(apps) => return v2_at(&apps),
            Err(e) => tracing::warn!(error = %e, "Could not use the delegated cgroup"),
        }
    }
    if is_root() {
        // Enable the controllers for children of the root, then for ours.
        for c in ["cpu", "memory", "pids"] {
            let _ = std::fs::write(root.join("cgroup.subtree_control"), format!("+{c}"));
        }
        let base = root.join("aegis");
        if let Err(e) = std::fs::create_dir_all(&base) {
            return Backend::None(format!("Could not create {}: {}", base.display(), e));
        }
        enable_controllers(&base);
        return v2_at(&base);
    }
    Backend::None(
        "Resource limits need the daemon to run as root, or as a systemd service with Delegate=yes (install.sh --server sets this up)"
            .into(),
    )
}

/// Moves the daemon into `<service>/daemon` (a cgroup that has processes
/// can't have controllers enabled for its children) and prepares
/// `<service>/apps`.
fn delegate(service: &Path) -> Result<PathBuf, anyhow::Error> {
    let daemon = service.join("daemon");
    std::fs::create_dir_all(&daemon)?;
    std::fs::write(daemon.join("cgroup.procs"), std::process::id().to_string())?;
    enable_controllers(service);
    let apps = service.join("apps");
    std::fs::create_dir_all(&apps)?;
    enable_controllers(&apps);
    Ok(apps)
}

fn enable_controllers(dir: &Path) {
    let available = std::fs::read_to_string(dir.join("cgroup.controllers")).unwrap_or_default();
    for c in ["cpu", "memory", "pids"] {
        if available.split_whitespace().any(|a| a == c) {
            let _ = std::fs::write(dir.join("cgroup.subtree_control"), format!("+{c}"));
        }
    }
}

fn v2_at(parent: &Path) -> Backend {
    let enabled =
        std::fs::read_to_string(parent.join("cgroup.subtree_control")).unwrap_or_default();
    let has = |c: &str| enabled.split_whitespace().any(|a| a == c);
    let (cpu, memory, pids) = (has("cpu"), has("memory"), has("pids"));
    if !cpu && !memory {
        return Backend::None(format!(
            "The cpu and memory controllers are not enabled for {} (cgroup.subtree_control)",
            parent.display()
        ));
    }
    Backend::V2 {
        parent: parent.to_path_buf(),
        cpu,
        memory,
        pids,
    }
}

fn v1_at(dirs: V1Dirs) -> Backend {
    if dirs.memory.is_none() && dirs.cpu.is_none() {
        return Backend::None("Could not create cgroups for the cpu or memory controllers".into());
    }
    for dir in dirs.all() {
        let _ = std::fs::create_dir_all(dir);
    }
    Backend::V1(dirs)
}

/// The daemon's own path in a v1 hierarchy, from /proc/self/cgroup lines
/// like `4:memory:/some/path` or `3:cpu,cpuacct:/`.
fn own_cgroup_v1(proc_cgroup: &str, controller: &str) -> Option<String> {
    proc_cgroup.lines().find_map(|line| {
        let mut parts = line.splitn(3, ':');
        let _id = parts.next()?;
        let controllers = parts.next()?;
        let path = parts.next()?;
        controllers
            .split(',')
            .any(|c| c == controller)
            .then(|| path.to_string())
    })
}

fn is_root() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

// ----------------------------------------------------------------------
// Writing limits
// ----------------------------------------------------------------------

fn write(dir: &Path, file: &str, value: &str) -> Result<(), anyhow::Error> {
    let path = dir.join(file);
    std::fs::write(&path, value)
        .map_err(|e| anyhow::anyhow!("writing '{}' to {}: {}", value, path.display(), e))
}

fn cpu_quota_usec(cores: f64) -> u64 {
    ((cores * CPU_PERIOD_USEC as f64).round() as u64).max(1000)
}

fn apply_v2(
    dir: &Path,
    limits: &ResourceLimits,
    cpu: bool,
    memory: bool,
    pids: bool,
) -> Result<(), anyhow::Error> {
    if cpu {
        let value = match limits.cpu_cores {
            Some(c) => format!("{} {}", cpu_quota_usec(c), CPU_PERIOD_USEC),
            None => format!("max {}", CPU_PERIOD_USEC),
        };
        write(dir, "cpu.max", &value)?;
    }
    if memory {
        match limits.memory_bytes {
            Some(bytes) => {
                write(dir, "memory.high", &(bytes / 10 * 9).to_string())?;
                write(dir, "memory.max", &bytes.to_string())?;
                // Without this the app swaps instead of hitting the limit.
                if dir.join("memory.swap.max").exists() {
                    write(dir, "memory.swap.max", "0")?;
                }
            }
            None => {
                write(dir, "memory.max", "max")?;
                write(dir, "memory.high", "max")?;
                if dir.join("memory.swap.max").exists() {
                    write(dir, "memory.swap.max", "max")?;
                }
            }
        }
    }
    if pids {
        let value = limits
            .pids_max
            .map_or_else(|| "max".to_string(), |n| n.to_string());
        write(dir, "pids.max", &value)?;
    }
    Ok(())
}

fn apply_v1(dirs: &V1Dirs, limits: &ResourceLimits) -> Result<(), anyhow::Error> {
    if let Some(dir) = &dirs.cpu {
        write(dir, "cpu.cfs_period_us", &CPU_PERIOD_USEC.to_string())?;
        let quota = limits
            .cpu_cores
            .map_or_else(|| "-1".to_string(), |c| cpu_quota_usec(c).to_string());
        write(dir, "cpu.cfs_quota_us", &quota)?;
    }
    if let Some(dir) = &dirs.memory {
        // The kernel requires memory.limit_in_bytes <= memory.memsw.limit_in_bytes
        // at all times, so the order of the two writes depends on direction.
        let memsw = dir.join("memory.memsw.limit_in_bytes").exists();
        match limits.memory_bytes {
            Some(bytes) => {
                let current = read_u64(&dir.join("memory.limit_in_bytes")).unwrap_or(u64::MAX);
                let value = bytes.to_string();
                if memsw && bytes >= current {
                    write(dir, "memory.memsw.limit_in_bytes", &value)?;
                }
                write(dir, "memory.limit_in_bytes", &value)?;
                if memsw && bytes < current {
                    write(dir, "memory.memsw.limit_in_bytes", &value)?;
                }
                write(
                    dir,
                    "memory.soft_limit_in_bytes",
                    &(bytes / 10 * 9).to_string(),
                )?;
            }
            None => {
                if memsw {
                    write(dir, "memory.memsw.limit_in_bytes", "-1")?;
                }
                write(dir, "memory.limit_in_bytes", "-1")?;
                write(dir, "memory.soft_limit_in_bytes", "-1")?;
            }
        }
    }
    if let Some(dir) = &dirs.pids {
        let value = limits
            .pids_max
            .map_or_else(|| "max".to_string(), |n| n.to_string());
        write(dir, "pids.max", &value)?;
    }
    Ok(())
}

// ----------------------------------------------------------------------
// Reading counters
// ----------------------------------------------------------------------

fn read_u64(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Parses "key value" lines (cpu.stat, memory.stat, memory.events, ...).
fn read_keyed(path: &Path) -> HashMap<String, u64> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            Some((it.next()?.to_string(), it.next()?.parse().ok()?))
        })
        .collect()
}

fn read_procs(dir: &Path) -> Vec<u32> {
    std::fs::read_to_string(dir.join("cgroup.procs"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

fn read_stats(group: &Group) -> CgroupStats {
    let mut s = CgroupStats::default();
    match group {
        Group::V2(dir) => {
            let cpu = read_keyed(&dir.join("cpu.stat"));
            s.cpu_usage_usec = cpu.get("usage_usec").copied().unwrap_or(0);
            s.nr_periods = cpu.get("nr_periods").copied().unwrap_or(0);
            s.nr_throttled = cpu.get("nr_throttled").copied().unwrap_or(0);
            s.throttled_usec = cpu.get("throttled_usec").copied().unwrap_or(0);
            let current = read_u64(&dir.join("memory.current")).unwrap_or(0);
            let stat = read_keyed(&dir.join("memory.stat"));
            s.memory_bytes =
                current.saturating_sub(stat.get("inactive_file").copied().unwrap_or(0));
            s.memory_limit_bytes = read_u64(&dir.join("memory.max"));
            let events = read_keyed(&dir.join("memory.events"));
            s.oom_kills = events.get("oom_kill").copied().unwrap_or(0);
            s.memory_high_events = events.get("high").copied().unwrap_or(0);
            s.pids = read_u64(&dir.join("pids.current"));
            s.procs = read_procs(dir);
        }
        Group::V1(dirs) => {
            if let Some(dir) = &dirs.cpuacct {
                s.cpu_usage_usec = read_u64(&dir.join("cpuacct.usage")).unwrap_or(0) / 1000;
            } else if let Some(dir) = &dirs.cpu {
                // "cpu,cpuacct" mounted together.
                s.cpu_usage_usec = read_u64(&dir.join("cpuacct.usage")).unwrap_or(0) / 1000;
            }
            if let Some(dir) = &dirs.cpu {
                let cpu = read_keyed(&dir.join("cpu.stat"));
                s.nr_periods = cpu.get("nr_periods").copied().unwrap_or(0);
                s.nr_throttled = cpu.get("nr_throttled").copied().unwrap_or(0);
                s.throttled_usec = cpu.get("throttled_time").copied().unwrap_or(0) / 1000;
            }
            if let Some(dir) = &dirs.memory {
                let usage = read_u64(&dir.join("memory.usage_in_bytes")).unwrap_or(0);
                let stat = read_keyed(&dir.join("memory.stat"));
                let inactive = stat
                    .get("total_inactive_file")
                    .or(stat.get("inactive_file"))
                    .copied()
                    .unwrap_or(0);
                s.memory_bytes = usage.saturating_sub(inactive);
                s.memory_limit_bytes =
                    read_u64(&dir.join("memory.limit_in_bytes")).filter(|l| *l < V1_UNLIMITED);
                s.oom_kills = read_keyed(&dir.join("memory.oom_control"))
                    .get("oom_kill")
                    .copied()
                    .unwrap_or(0);
                s.memory_high_events = read_u64(&dir.join("memory.failcnt")).unwrap_or(0);
                s.procs = read_procs(dir);
            }
            if let Some(dir) = &dirs.pids {
                s.pids = read_u64(&dir.join("pids.current"));
                if s.procs.is_empty() {
                    s.procs = read_procs(dir);
                }
            }
            if s.procs.is_empty() {
                if let Some(dir) = &dirs.cpu {
                    s.procs = read_procs(dir);
                }
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fake_v2(root: &Path) -> ResourceManager {
        std::fs::write(root.join("cgroup.subtree_control"), "cpu memory pids").unwrap();
        ResourceManager::new(&Mode::V2At(root.to_path_buf()))
    }

    #[test]
    fn test_v2_limits_are_written_in_kernel_format() {
        let root = TempDir::new().unwrap();
        let rm = fake_v2(root.path());
        let caps = rm.capabilities();
        assert_eq!(caps.backend, BackendKind::CgroupV2);
        assert!(caps.cpu && caps.memory && caps.pids);

        let project = ProjectId::new();
        let process = ProcessId::new();
        rm.load_limits(
            project,
            ResourceLimits {
                cpu_cores: Some(0.5),
                memory_bytes: Some(256 * 1024 * 1024),
                pids_max: Some(64),
            },
        );
        let placement = rm.prepare(&process, &project);
        assert!(placement.warnings.is_empty(), "{:?}", placement.warnings);
        let dir = root.path().join(process.to_string());
        assert_eq!(placement.join_files, vec![dir.join("cgroup.procs")]);
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap();
        assert_eq!(read("cpu.max"), "50000 100000");
        assert_eq!(read("memory.max"), "268435456");
        assert_eq!(read("memory.high"), (268435456u64 / 10 * 9).to_string());
        assert_eq!(read("pids.max"), "64");

        // Live change: written to the running process's cgroup at once.
        std::fs::write(dir.join("memory.swap.max"), "max").unwrap();
        let applied = rm
            .set_limits(
                project,
                ResourceLimits {
                    cpu_cores: Some(2.0),
                    memory_bytes: None,
                    pids_max: None,
                },
            )
            .unwrap();
        assert_eq!(applied, 1);
        assert_eq!(read("cpu.max"), "200000 100000");
        assert_eq!(read("memory.max"), "max");
        assert_eq!(read("memory.swap.max"), "max");
        assert_eq!(read("pids.max"), "max");
        // Other projects are untouched.
        assert_eq!(
            rm.set_limits(ProjectId::new(), ResourceLimits::default())
                .unwrap(),
            0
        );

        // Counters.
        std::fs::write(
            dir.join("cpu.stat"),
            "usage_usec 1500\nuser_usec 1000\nsystem_usec 500\nnr_periods 10\nnr_throttled 4\nthrottled_usec 900\n",
        )
        .unwrap();
        std::fs::write(dir.join("memory.current"), "10000").unwrap();
        std::fs::write(
            dir.join("memory.stat"),
            "anon 6000\nfile 4000\ninactive_file 3000\n",
        )
        .unwrap();
        std::fs::write(dir.join("memory.max"), "20000").unwrap();
        std::fs::write(
            dir.join("memory.events"),
            "low 0\nhigh 7\nmax 2\noom 1\noom_kill 1\n",
        )
        .unwrap();
        std::fs::write(dir.join("pids.current"), "3").unwrap();
        std::fs::write(dir.join("cgroup.procs"), "10\n11\n").unwrap();
        let s = rm.stats(&process).unwrap();
        assert_eq!(
            s,
            CgroupStats {
                cpu_usage_usec: 1500,
                nr_periods: 10,
                nr_throttled: 4,
                throttled_usec: 900,
                memory_bytes: 7000,
                memory_limit_bytes: Some(20000),
                oom_kills: 1,
                memory_high_events: 7,
                pids: Some(3),
                procs: vec![10, 11],
            }
        );

        rm.finished(&process);
        assert!(rm.stats(&process).is_none());
    }

    #[test]
    fn test_v1_limits_and_counters() {
        let root = TempDir::new().unwrap();
        let dirs = V1Dirs {
            memory: Some(root.path().join("memory")),
            cpu: Some(root.path().join("cpu")),
            cpuacct: Some(root.path().join("cpuacct")),
            pids: None,
        };
        let rm = ResourceManager::new(&Mode::V1At(dirs));
        let caps = rm.capabilities();
        assert_eq!(caps.backend, BackendKind::CgroupV1);
        assert!(caps.cpu && caps.memory && !caps.pids);
        assert!(rm
            .set_limits(
                ProjectId::new(),
                ResourceLimits {
                    pids_max: Some(5),
                    ..Default::default()
                }
            )
            .is_err());

        let project = ProjectId::new();
        let process = ProcessId::new();
        let placement = rm.prepare(&process, &project);
        assert_eq!(placement.join_files.len(), 3);
        let mem = root.path().join("memory").join(process.to_string());
        let cpu = root.path().join("cpu").join(process.to_string());
        let read = |d: &Path, f: &str| std::fs::read_to_string(d.join(f)).unwrap();
        assert_eq!(read(&cpu, "cpu.cfs_quota_us"), "-1");
        assert_eq!(read(&mem, "memory.limit_in_bytes"), "-1");

        std::fs::write(
            mem.join("memory.memsw.limit_in_bytes"),
            "9223372036854771712",
        )
        .unwrap();
        std::fs::write(mem.join("memory.limit_in_bytes"), "9223372036854771712").unwrap();
        rm.set_limits(
            project,
            ResourceLimits {
                cpu_cores: Some(0.25),
                memory_bytes: Some(64 * 1024 * 1024),
                pids_max: None,
            },
        )
        .unwrap();
        assert_eq!(read(&cpu, "cpu.cfs_quota_us"), "25000");
        assert_eq!(read(&cpu, "cpu.cfs_period_us"), "100000");
        assert_eq!(read(&mem, "memory.limit_in_bytes"), "67108864");
        assert_eq!(read(&mem, "memory.memsw.limit_in_bytes"), "67108864");

        std::fs::write(
            root.path()
                .join("cpuacct")
                .join(process.to_string())
                .join("cpuacct.usage"),
            "5000000",
        )
        .unwrap();
        std::fs::write(
            cpu.join("cpu.stat"),
            "nr_periods 20\nnr_throttled 5\nthrottled_time 2000000\n",
        )
        .unwrap();
        std::fs::write(mem.join("memory.usage_in_bytes"), "50000").unwrap();
        std::fs::write(
            mem.join("memory.stat"),
            "cache 100\ntotal_inactive_file 10000\n",
        )
        .unwrap();
        std::fs::write(
            mem.join("memory.oom_control"),
            "oom_kill_disable 0\nunder_oom 0\noom_kill 2\n",
        )
        .unwrap();
        std::fs::write(mem.join("memory.failcnt"), "9").unwrap();
        std::fs::write(mem.join("cgroup.procs"), "42\n").unwrap();
        let s = rm.stats(&process).unwrap();
        assert_eq!(s.cpu_usage_usec, 5000);
        assert_eq!(s.nr_throttled, 5);
        assert_eq!(s.throttled_usec, 2000);
        assert_eq!(s.memory_bytes, 40000);
        assert_eq!(s.memory_limit_bytes, Some(67108864));
        assert_eq!(s.oom_kills, 2);
        assert_eq!(s.memory_high_events, 9);
        assert_eq!(s.procs, vec![42]);
    }

    #[test]
    fn test_off_and_missing_controllers() {
        let rm = ResourceManager::new(&Mode::Off);
        let caps = rm.capabilities();
        assert_eq!(caps.backend, BackendKind::None);
        assert!(!caps.any());
        assert!(caps.reason.unwrap().contains("turned off"));
        assert!(rm
            .prepare(&ProcessId::new(), &ProjectId::new())
            .join_files
            .is_empty());
        assert!(rm
            .set_limits(
                ProjectId::new(),
                ResourceLimits {
                    cpu_cores: Some(1.0),
                    ..Default::default()
                }
            )
            .is_err());
        // Clearing limits is always fine.
        rm.set_limits(ProjectId::new(), ResourceLimits::default())
            .unwrap();

        let root = TempDir::new().unwrap();
        std::fs::write(root.path().join("cgroup.subtree_control"), "pids").unwrap();
        let caps = ResourceManager::new(&Mode::V2At(root.path().to_path_buf())).capabilities();
        assert_eq!(caps.backend, BackendKind::None);
        assert!(caps.reason.unwrap().contains("not enabled"));
    }

    #[test]
    fn test_own_cgroup_v1_parsing() {
        let proc = "12:pids:/user.slice\n4:memory:/docker/abc\n3:cpu,cpuacct:/docker/abc\n0::/\n";
        assert_eq!(
            own_cgroup_v1(proc, "memory").as_deref(),
            Some("/docker/abc")
        );
        assert_eq!(
            own_cgroup_v1(proc, "cpuacct").as_deref(),
            Some("/docker/abc")
        );
        assert_eq!(own_cgroup_v1(proc, "pids").as_deref(), Some("/user.slice"));
        assert_eq!(own_cgroup_v1(proc, "blkio"), None);
    }
}
