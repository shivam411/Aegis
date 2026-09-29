//! Resource limits and metrics: the sampler that measures every app and
//! the host, live limit changes, and pressure alerts.

use crate::{ControlError, ControlPlane, ControlResult};
use aegis_config::ResourceLimits;
use aegis_metrics::history::{Point, Sample};
use aegis_metrics::HostSnapshot;
use aegis_projection::ProjectState;
use aegis_resources::{Capabilities, MIN_CPU_CORES, MIN_MEMORY_BYTES};
use aegis_types::{ProcessId, ProjectId};
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// The history series holding host metrics.
pub const HOST_SERIES: &str = "host";

/// A new sample, for live charts.
#[derive(Debug, Clone, Serialize)]
pub struct MetricsUpdate {
    /// A project id, or "host".
    pub series: String,
    pub sample: Sample,
}

/// What the host can do and how much of it is promised to apps.
#[derive(Debug, Clone, Serialize)]
pub struct ResourceOverview {
    pub capabilities: Capabilities,
    pub host_cpu_cores: usize,
    pub host_memory_bytes: u64,
    /// Sum of all apps' memory limits.
    pub committed_memory_bytes: u64,
    /// Sum of all apps' CPU limits.
    pub committed_cpu_cores: f64,
}

/// How long a condition must last before it is reported, and how often the
/// same alert may repeat.
const SUSTAINED: Duration = Duration::from_secs(30);
const ALERT_COOLDOWN: Duration = Duration::from_secs(15 * 60);
const OOM_COOLDOWN: Duration = Duration::from_secs(60);
const DISK_COOLDOWN: Duration = Duration::from_secs(6 * 3600);
const DISK_LOW_RATIO: f64 = 0.10;

#[derive(Default)]
struct Previous {
    cpu_usec: u64,
    nr_periods: u64,
    nr_throttled: u64,
    oom_kills: u64,
    at: Option<Instant>,
}

#[derive(Default)]
struct Pressure {
    throttled_since: Option<Instant>,
    memory_since: Option<Instant>,
    last_alert: HashMap<&'static str, Instant>,
}

impl Pressure {
    fn allow(&mut self, kind: &'static str, cooldown: Duration) -> bool {
        let now = Instant::now();
        match self.last_alert.get(kind) {
            Some(t) if now.duration_since(*t) < cooldown => false,
            _ => {
                self.last_alert.insert(kind, now);
                true
            }
        }
    }
}

struct Measured {
    sample: Sample,
    new_oom_kills: u64,
}

impl ControlPlane {
    pub fn resource_capabilities(&self) -> Capabilities {
        self.inner.resources.capabilities()
    }

    pub fn host(&self) -> &aegis_metrics::HostMonitor {
        &self.inner.host
    }

    pub fn resource_overview(&self) -> ResourceOverview {
        let host = self.inner.host.latest();
        let projects = self.inner.projection.get_projects();
        ResourceOverview {
            capabilities: self.resource_capabilities(),
            host_cpu_cores: host.cpu_count,
            host_memory_bytes: host.memory_total_bytes,
            committed_memory_bytes: projects
                .iter()
                .filter_map(|p| p.resources.memory_bytes)
                .sum(),
            committed_cpu_cores: projects.iter().filter_map(|p| p.resources.cpu_cores).sum(),
        }
    }

    /// The latest sample of a project (or `HOST_SERIES`).
    pub fn latest_sample(&self, series: &str) -> Option<Sample> {
        self.inner.history.latest(series)
    }

    pub async fn metrics(
        &self,
        series: &str,
        range: Duration,
        max_points: usize,
    ) -> ControlResult<Vec<Point>> {
        self.inner
            .history
            .range(series, now_ms(), range, max_points)
            .await
            .map_err(|e| ControlError::Internal(e.into()))
    }

    pub fn subscribe_metrics(&self) -> tokio::sync::broadcast::Receiver<MetricsUpdate> {
        self.inner.metrics_tx.subscribe()
    }

    /// Changes a project's limits and applies them to its running processes
    /// at once, without a restart. Risky changes (below current memory use,
    /// or promising apps more memory than the host has) need `force`.
    pub async fn set_resource_limits(
        &self,
        project_id: ProjectId,
        limits: ResourceLimits,
        force: bool,
    ) -> ControlResult<ProjectState> {
        let project = self
            .inner
            .projection
            .get_project(&project_id)
            .ok_or_else(|| ControlError::NotFound(format!("Unknown project {}", project_id)))?;
        let caps = self.resource_capabilities();
        let unavailable = |what: &str| {
            ControlError::FailedPrecondition(format!(
                "{} limits are not available on this host: {}",
                what,
                caps.reason.clone().unwrap_or_default()
            ))
        };
        let host = self.inner.host.latest();

        if let Some(cpu) = limits.cpu_cores {
            if !caps.cpu {
                return Err(unavailable("CPU"));
            }
            if !cpu.is_finite() || cpu < MIN_CPU_CORES || cpu > host.cpu_count as f64 {
                return Err(ControlError::InvalidArgument(format!(
                    "The CPU limit must be between {} and {} cores (this host's size)",
                    MIN_CPU_CORES, host.cpu_count
                )));
            }
        }
        if let Some(memory) = limits.memory_bytes {
            if !caps.memory {
                return Err(unavailable("Memory"));
            }
            if memory < MIN_MEMORY_BYTES {
                return Err(ControlError::InvalidArgument(format!(
                    "The memory limit must be at least {} MiB",
                    MIN_MEMORY_BYTES / 1024 / 1024
                )));
            }
        }
        if let Some(pids) = limits.pids_max {
            if !caps.pids {
                return Err(unavailable("Process-count"));
            }
            if pids < 1 {
                return Err(ControlError::InvalidArgument(
                    "The process limit must be at least 1".into(),
                ));
            }
        }

        if !force {
            if let (Some(memory), Some(latest)) = (
                limits.memory_bytes,
                self.inner.history.latest(&project_id.to_string()),
            ) {
                if memory < latest.memory_bytes && latest.procs > 0 {
                    return Err(ControlError::NeedsConfirmation(format!(
                        "{} is using {} MiB right now; a {} MiB limit will make the kernel reclaim memory and may kill it",
                        project.name,
                        latest.memory_bytes / 1024 / 1024,
                        memory / 1024 / 1024
                    )));
                }
            }
            if let Some(memory) = limits.memory_bytes {
                let others: u64 = self
                    .inner
                    .projection
                    .get_projects()
                    .iter()
                    .filter(|p| p.id != project_id)
                    .filter_map(|p| p.resources.memory_bytes)
                    .sum();
                if others + memory > host.memory_total_bytes {
                    return Err(ControlError::NeedsConfirmation(format!(
                        "Apps would be promised {} MiB of memory in total, but this host has {} MiB",
                        (others + memory) / 1024 / 1024,
                        host.memory_total_bytes / 1024 / 1024
                    )));
                }
            }
        }

        let previous = project.resources.clone();
        let applied = match self.inner.resources.set_limits(project_id, limits.clone()) {
            Ok(n) => n,
            Err(e) => {
                // Keep what the kernel and the event log agree on.
                let _ = self.inner.resources.set_limits(project_id, previous);
                return Err(ControlError::FailedPrecondition(format!(
                    "The kernel refused the new limits: {}",
                    e
                )));
            }
        };
        self.record(
            "ResourceLimitsChanged",
            json!({
                "project_id": project_id.to_string(),
                "limits": limits,
                "previous": previous,
                "applied_to_processes": applied,
                "forced": force,
            }),
        )
        .await?;
        Ok(self
            .inner
            .projection
            .get_project(&project_id)
            .unwrap_or(project))
    }

    pub(crate) fn spawn_sampler(&self) {
        let interval = self.inner.settings.sample_interval;
        let weak = std::sync::Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut previous: HashMap<ProcessId, Previous> = HashMap::new();
            let mut pressure: HashMap<String, Pressure> = HashMap::new();
            let mut last_prune = Instant::now() - Duration::from_secs(3600);
            let mut ticker =
                tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let Some(inner) = weak.upgrade() else { return };
                let control = ControlPlane { inner };
                if control.inner.shutting_down.load(Ordering::SeqCst) {
                    control.inner.history.flush().await;
                    return;
                }
                control.sample_all(&mut previous, &mut pressure).await;
                if last_prune.elapsed() >= Duration::from_secs(3600) {
                    last_prune = Instant::now();
                    if let Err(e) = control.inner.history.prune(now_ms()).await {
                        tracing::warn!(error = %e, "Could not prune old metrics");
                    }
                }
            }
        });
    }

    async fn sample_all(
        &self,
        previous: &mut HashMap<ProcessId, Previous>,
        pressure: &mut HashMap<String, Pressure>,
    ) {
        let at = now_ms();
        let monitor = self.inner.host.clone();
        let host = tokio::task::spawn_blocking(move || monitor.sample())
            .await
            .ok();
        if let Some(host) = &host {
            let sample = host_sample(at, host);
            self.publish(HOST_SERIES, sample).await;
            self.check_disk(host, pressure.entry(HOST_SERIES.into()).or_default())
                .await;
        }

        // Every process that is up, grouped by project (two during a
        // graceful switch).
        let mut by_project: HashMap<ProjectId, Vec<aegis_process::ProcessInfo>> = HashMap::new();
        for p in self.inner.supervisor.list_processes() {
            if matches!(
                p.status,
                aegis_process::ProcessStatus::Running | aegis_process::ProcessStatus::Backoff
            ) {
                by_project.entry(p.project_id).or_default().push(p);
            }
        }
        let live: Vec<ProcessId> = by_project.values().flatten().map(|p| p.id).collect();
        previous.retain(|id, _| live.contains(id));

        for (project_id, processes) in by_project {
            let resources = self.inner.resources.clone();
            let procs = processes.clone();
            let prev: Vec<(ProcessId, Previous)> = procs
                .iter()
                .map(|p| (p.id, previous.remove(&p.id).unwrap_or_default()))
                .collect();
            let limits = self.inner.resources.limits(&project_id);
            let measured =
                tokio::task::spawn_blocking(move || measure(&resources, &procs, prev, at, &limits))
                    .await;
            let Ok((m, prev)) = measured else { continue };
            previous.extend(prev);
            let key = project_id.to_string();
            let p = pressure.entry(key.clone()).or_default();
            self.check_pressure(&project_id, &m, p).await;
            self.publish(&key, m.sample).await;
        }
    }

    async fn publish(&self, series: &str, sample: Sample) {
        self.inner.history.record(series, sample.clone()).await;
        let _ = self.inner.metrics_tx.send(MetricsUpdate {
            series: series.to_string(),
            sample,
        });
    }

    async fn alert(
        &self,
        project_id: Option<&ProjectId>,
        kind: &str,
        message: String,
        extra: serde_json::Value,
    ) {
        tracing::warn!(kind, %message, "Resource pressure");
        let mut payload = json!({"kind": kind, "message": message});
        if let Some(id) = project_id {
            payload["project_id"] = json!(id.to_string());
        }
        if let (Some(obj), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
            for (k, v) in extra {
                obj.insert(k.clone(), v.clone());
            }
        }
        let _ = self.record("ResourcePressureDetected", payload).await;
    }

    async fn check_pressure(&self, project_id: &ProjectId, m: &Measured, p: &mut Pressure) {
        let name = self
            .inner
            .projection
            .get_project(project_id)
            .map(|p| p.name)
            .unwrap_or_else(|| project_id.to_string());
        let s = &m.sample;
        let now = Instant::now();

        if m.new_oom_kills > 0 && p.allow("oom_kill", OOM_COOLDOWN) {
            self.alert(
                Some(project_id),
                "oom_kill",
                format!(
                    "{} went over its {} MiB memory limit and the kernel killed it",
                    name,
                    s.memory_limit_bytes.unwrap_or(0) / 1024 / 1024
                ),
                json!({"oom_kills": s.oom_kills, "memory_limit_bytes": s.memory_limit_bytes}),
            )
            .await;
        }

        if s.throttled_ratio >= 0.5 {
            let since = *p.throttled_since.get_or_insert(now);
            if now.duration_since(since) >= SUSTAINED && p.allow("cpu_throttled", ALERT_COOLDOWN) {
                self.alert(
                    Some(project_id),
                    "cpu_throttled",
                    format!(
                        "{} has been held back by its {:.2}-core CPU limit for over {} s",
                        name,
                        s.cpu_limit_cores.unwrap_or(0.0),
                        SUSTAINED.as_secs()
                    ),
                    json!({"throttled_ratio": s.throttled_ratio, "cpu_limit_cores": s.cpu_limit_cores}),
                )
                .await;
            }
        } else {
            p.throttled_since = None;
        }

        let near_limit = s
            .memory_limit_bytes
            .is_some_and(|l| l > 0 && s.memory_bytes as f64 >= l as f64 * 0.9);
        if near_limit {
            let since = *p.memory_since.get_or_insert(now);
            if now.duration_since(since) >= SUSTAINED && p.allow("memory_pressure", ALERT_COOLDOWN)
            {
                self.alert(
                    Some(project_id),
                    "memory_pressure",
                    format!(
                        "{} has been using over 90% of its {} MiB memory limit for over {} s",
                        name,
                        s.memory_limit_bytes.unwrap_or(0) / 1024 / 1024,
                        SUSTAINED.as_secs()
                    ),
                    json!({"memory_bytes": s.memory_bytes, "memory_limit_bytes": s.memory_limit_bytes}),
                )
                .await;
            }
        } else {
            p.memory_since = None;
        }
    }

    async fn check_disk(&self, host: &HostSnapshot, p: &mut Pressure) {
        if host.disk_total_bytes == 0 {
            return;
        }
        let free = host.disk_available_bytes as f64 / host.disk_total_bytes as f64;
        if free < DISK_LOW_RATIO && p.allow("disk_low", DISK_COOLDOWN) {
            self.alert(
                None,
                "disk_low",
                format!(
                    "Only {:.0}% ({} MiB) of the disk holding {} is free",
                    free * 100.0,
                    host.disk_available_bytes / 1024 / 1024,
                    host.disk_mount_point
                ),
                json!({
                    "disk_available_bytes": host.disk_available_bytes,
                    "disk_total_bytes": host.disk_total_bytes,
                    "mount_point": host.disk_mount_point,
                }),
            )
            .await;
        }
    }
}

/// Measures a project's processes. Runs on a blocking thread (it reads
/// many small files).
fn measure(
    resources: &aegis_resources::ResourceManager,
    processes: &[aegis_process::ProcessInfo],
    previous: Vec<(ProcessId, Previous)>,
    at: i64,
    limits: &ResourceLimits,
) -> (Measured, Vec<(ProcessId, Previous)>) {
    let mut previous: HashMap<ProcessId, Previous> = previous.into_iter().collect();
    let mut sample = Sample {
        at,
        cpu_limit_cores: limits.cpu_cores,
        memory_limit_bytes: limits.memory_bytes,
        ..Default::default()
    };
    let mut throttled = (0u64, 0u64);
    let mut new_oom_kills = 0;
    let now = Instant::now();
    let mut out = Vec::new();
    for p in processes {
        let cg = resources.stats(&p.id);
        let pids = match &cg {
            Some(c) if !c.procs.is_empty() => c.procs.clone(),
            _ => p
                .pid
                .map(aegis_metrics::proc::process_group)
                .unwrap_or_default(),
        };
        let totals = aegis_metrics::proc::totals(&pids);
        let (cpu_usec, memory) = match &cg {
            Some(c) if c.cpu_usage_usec > 0 || c.memory_bytes > 0 => {
                (c.cpu_usage_usec, c.memory_bytes)
            }
            _ => (totals.cpu_usec, totals.rss_bytes),
        };
        let prev = previous.remove(&p.id).unwrap_or_default();
        if let Some(t) = prev.at {
            let elapsed = now.duration_since(t).as_micros().max(1) as f64;
            // A restart outside a cgroup resets the counter.
            sample.cpu_cores += cpu_usec.saturating_sub(prev.cpu_usec) as f64 / elapsed;
        }
        sample.memory_bytes += memory;
        sample.threads += totals.threads;
        sample.fds += totals.fds;
        sample.procs += totals.procs;
        sample.restarts = sample.restarts.max(p.restart_count);
        let (periods, nr_throttled, ooms) = cg
            .as_ref()
            .map_or((0, 0, 0), |c| (c.nr_periods, c.nr_throttled, c.oom_kills));
        if prev.at.is_some() {
            throttled.0 += periods.saturating_sub(prev.nr_periods);
            throttled.1 += nr_throttled.saturating_sub(prev.nr_throttled);
            new_oom_kills += ooms.saturating_sub(prev.oom_kills);
        }
        sample.oom_kills += ooms;
        if let Some(limit) = cg.as_ref().and_then(|c| c.memory_limit_bytes) {
            sample.memory_limit_bytes = Some(limit);
        }
        out.push((
            p.id,
            Previous {
                cpu_usec,
                nr_periods: periods,
                nr_throttled,
                oom_kills: ooms,
                at: Some(now),
            },
        ));
    }
    if throttled.0 > 0 {
        sample.throttled_ratio = throttled.1 as f64 / throttled.0 as f64;
    }
    (
        Measured {
            sample,
            new_oom_kills,
        },
        out,
    )
}

fn host_sample(at: i64, host: &HostSnapshot) -> Sample {
    Sample {
        at,
        cpu_cores: host.cpu_percent as f64 / 100.0 * host.cpu_count as f64,
        memory_bytes: host.memory_used_bytes,
        cpu_limit_cores: Some(host.cpu_count as f64),
        memory_limit_bytes: Some(host.memory_total_bytes),
        ..Default::default()
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
