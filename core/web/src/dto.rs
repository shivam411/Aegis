//! JSON request and response bodies. Kept separate from the internal read
//! models so the HTTP contract (and its OpenAPI schema) is explicit.

use aegis_control::ProjectView;
use aegis_projection::{DeploymentState, ProcessState, ReleaseState, ScheduleState};
use aegis_types::Event;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct StatusDto {
    pub version: String,
    pub project_count: usize,
    pub running_processes: usize,
    pub event_count: u64,
    pub data_dir: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ScheduleDto {
    /// UTC hour, 0-23.
    pub hour: u32,
    pub minute: u32,
    pub branch: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProcessDto {
    pub id: String,
    pub project_id: String,
    pub pid: Option<u32>,
    /// Running, Backoff, Stopping, Stopped, Exited or Failed.
    pub status: String,
    /// "running" or "stopped": what the operator last asked for.
    pub desired: String,
    pub restart_count: u32,
    pub release_version: Option<String>,
    pub command: Option<String>,
    pub last_start: Option<String>,
    pub last_exit_code: Option<i32>,
    /// "process" or "docker". For "docker", `pid` is the container's main
    /// process (as seen from the host).
    pub runtime: String,
    /// The container's name (docker only).
    pub container: Option<String>,
    /// The image the container runs (docker only).
    pub image: Option<String>,
    /// Instance number (1-based) when the app runs several instances.
    #[serde(default)]
    pub instance: Option<u32>,
    /// The port the process listens on (`PORT`).
    #[serde(default)]
    pub port: Option<u16>,
}

/// One instance of an app.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct InstanceDto {
    pub process: ProcessDto,
    /// CPU in use (cores) at the latest sample, while it runs.
    pub cpu_cores: Option<f64>,
    /// Memory in use (bytes) at the latest sample, while it runs.
    pub memory_bytes: Option<u64>,
}

/// The previous release, kept running after a BlueGreen switch so a
/// rollback to it is instant.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct StandbyDto {
    pub version: String,
    pub process_ids: Vec<String>,
    /// When it is stopped (RFC 3339).
    pub until: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProjectDto {
    pub id: String,
    pub name: String,
    pub repository_url: String,
    pub branch: String,
    pub runtime: Option<String>,
    pub source_dir: Option<String>,
    /// The folder of the source that is built and run (monorepo apps).
    #[serde(default)]
    pub source_subdir: Option<String>,
    /// Version of the live release.
    pub current_release: Option<String>,
    pub created_at: String,
    pub schedule: Option<ScheduleDto>,
    /// Settings edited in the dashboard (override aegis.toml).
    pub settings: ProjectSettingsDto,
    /// The process serving the live release.
    pub process: Option<ProcessDto>,
    /// CPU, memory and process limits.
    pub resources: ResourceLimitsDto,
    /// The latest measurement, while the app runs (all instances together).
    pub usage: Option<SampleDto>,
    /// Every instance serving the live release, in instance order (one
    /// entry for single-instance apps). Resource limits apply to each
    /// instance on its own.
    #[serde(default)]
    pub instances: Vec<InstanceDto>,
    /// The previous release's instances after a BlueGreen switch.
    #[serde(default)]
    pub standby: Option<StandbyDto>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ReleaseDto {
    pub id: String,
    pub project_id: String,
    pub version: String,
    /// Built, Active, Inactive, Failed or Archived.
    pub status: String,
    pub commit_sha: String,
    pub commit_message: String,
    pub checksum: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DeploymentDto {
    pub id: String,
    pub project_id: String,
    pub release_id: Option<String>,
    pub version: Option<String>,
    pub strategy: String,
    pub trigger: Option<String>,
    /// Queued, InProgress, Success, Failed or RolledBack.
    pub status: String,
    /// Last pipeline stage reached.
    pub stage: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EventDto {
    pub id: String,
    pub event_type: String,
    pub created_at: String,
    #[schema(value_type = Object)]
    pub payload: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LogLineDto {
    pub timestamp: String,
    /// "out", "err" or "sys" (messages from the supervisor).
    pub stream: String,
    pub line: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LogsDto {
    pub process_id: String,
    pub lines: Vec<LogLineDto>,
}

#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct CreateProjectRequest {
    pub name: String,
    /// Reuse an existing project id (e.g. from aegis.toml).
    pub project_id: Option<String>,
    pub repository_url: Option<String>,
    pub branch: Option<String>,
    pub runtime: Option<String>,
    /// Directory on the server that deployments copy code from.
    pub source_dir: Option<String>,
    /// Build and run only this folder of the source, e.g. `apps/web` in a
    /// monorepo (relative, no `..`).
    pub source_subdir: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct DeployRequest {
    pub branch: Option<String>,
    /// GracefulSwitch (default: the project's setting, else GracefulSwitch),
    /// Immediate, Rolling or BlueGreen. Rolling and BlueGreen need the app
    /// to be served by the built-in proxy.
    pub strategy: Option<String>,
    /// Copy code from this server directory instead of the project's default source.
    pub source_dir: Option<String>,
    /// Clone this repository instead of the project's default source.
    pub repository_url: Option<String>,
    pub commit: Option<String>,
    /// Release name; generated from the current time when omitted.
    pub version: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DeployAccepted {
    pub deployment_id: String,
    pub project_id: String,
    pub project_name: String,
}

#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RollbackRequest {
    /// Defaults to the release that was live before the current one.
    pub version: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RollbackResult {
    /// The release that is now live.
    pub version: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduleRequest {
    /// UTC hour, 0-23.
    pub hour: u32,
    #[serde(default)]
    pub minute: u32,
    /// Defaults to the project's branch.
    #[serde(default)]
    pub branch: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ErrorDetail {
    /// not_found, invalid_argument, failed_precondition, forbidden or internal.
    pub code: String,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

impl From<ScheduleState> for ScheduleDto {
    fn from(s: ScheduleState) -> Self {
        Self {
            hour: s.hour,
            minute: s.minute,
            branch: s.branch,
        }
    }
}

impl From<ProcessState> for ProcessDto {
    fn from(p: ProcessState) -> Self {
        Self {
            id: p.id.to_string(),
            project_id: p.project_id.to_string(),
            pid: p.pid,
            status: p.status,
            desired: p.desired,
            restart_count: p.restart_count,
            release_version: p.release_version,
            command: p.command,
            last_start: p.last_start,
            last_exit_code: p.last_exit_code,
            runtime: p.runtime.unwrap_or_else(|| "process".into()),
            container: p.container,
            image: p.image,
            instance: p.instance,
            port: p.port,
        }
    }
}

impl From<aegis_control::InstanceView> for InstanceDto {
    fn from(i: aegis_control::InstanceView) -> Self {
        Self {
            process: i.process.into(),
            cpu_cores: i.cpu_cores,
            memory_bytes: i.memory_bytes,
        }
    }
}

impl From<ProjectView> for ProjectDto {
    fn from(view: ProjectView) -> Self {
        let p = view.project;
        Self {
            id: p.id.to_string(),
            name: p.name,
            repository_url: p.repository_url,
            branch: p.branch,
            runtime: p.runtime,
            source_dir: p.source_dir,
            source_subdir: p.source_subdir,
            current_release: p.current_release,
            created_at: p.created_at,
            schedule: p.schedule.map(Into::into),
            settings: p.settings.into(),
            resources: p.resources.into(),
            process: view.process.map(Into::into),
            usage: view.usage.map(Into::into),
            instances: view.instances.into_iter().map(Into::into).collect(),
            standby: view.standby.map(|s| StandbyDto {
                version: s.version,
                process_ids: s.process_ids.iter().map(|i| i.to_string()).collect(),
                until: s.until,
            }),
        }
    }
}

impl From<ReleaseState> for ReleaseDto {
    fn from(r: ReleaseState) -> Self {
        Self {
            id: r.id.to_string(),
            project_id: r.project_id.to_string(),
            version: r.version,
            status: r.status,
            commit_sha: r.commit_sha,
            commit_message: r.commit_message,
            checksum: r.checksum,
            created_at: r.created_at,
        }
    }
}

impl From<DeploymentState> for DeploymentDto {
    fn from(d: DeploymentState) -> Self {
        Self {
            id: d.id.to_string(),
            project_id: d.project_id.to_string(),
            release_id: d.release_id.map(|r| r.to_string()),
            version: d.version,
            strategy: d.strategy,
            trigger: d.trigger,
            status: d.status,
            stage: d.stage,
            error: d.error,
            created_at: d.created_at,
            completed_at: d.completed_at,
        }
    }
}

impl From<Event> for EventDto {
    fn from(e: Event) -> Self {
        Self {
            payload: serde_json::from_str(&e.payload_json)
                .unwrap_or(serde_json::Value::String(e.payload_json)),
            id: e.id.to_string(),
            event_type: e.event_type,
            created_at: e.created_at,
        }
    }
}

impl From<aegis_process::LogLine> for LogLineDto {
    fn from(l: aegis_process::LogLine) -> Self {
        Self {
            timestamp: l.timestamp.to_rfc3339(),
            stream: l.stream.as_str().to_string(),
            line: l.line,
        }
    }
}

// ----------------------------------------------------------------------
// Authentication
// ----------------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SessionDto {
    /// "user" (session cookie) or "token" (API token).
    pub kind: String,
    /// Username or token name.
    pub name: String,
    /// Send as `X-CSRF-Token` on requests that change state (sessions only).
    pub csrf_token: Option<String>,
    /// When the session expires if left idle (unix seconds; sessions only).
    pub expires_at: Option<i64>,
    /// "read" or "deploy" (tokens only).
    pub scope: Option<String>,
    /// "viewer", "deployer" or "admin" (users only).
    pub role: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    /// At least 12 characters.
    pub new_password: String,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateTokenRequest {
    pub name: String,
    /// "read" (GET requests only) or "deploy" (everything except managing tokens and passwords).
    pub scope: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TokenDto {
    pub id: String,
    pub name: String,
    pub scope: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct NewTokenDto {
    /// Shown once. Send as `Authorization: Bearer <token>`.
    pub token: String,
    pub info: TokenDto,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct WebhookSecretDto {
    /// Payload URL path; prefix it with the API's public base URL.
    pub path: String,
    /// Shown once. Paste into the GitHub webhook's "Secret" field.
    pub secret: String,
    /// "application/json"
    pub content_type: String,
    /// Events Aegis acts on ("push"; "ping" is acknowledged).
    pub events: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct WebhookResult {
    /// "queued", "ignored", "duplicate" or "pong".
    pub status: String,
    pub deployment_id: Option<String>,
    pub reason: Option<String>,
}

impl From<aegis_auth::TokenInfo> for TokenDto {
    fn from(t: aegis_auth::TokenInfo) -> Self {
        Self {
            id: t.id,
            name: t.name,
            scope: t.scope.as_str().to_string(),
            created_at: t.created_at,
            last_used_at: t.last_used_at,
            revoked: t.revoked,
        }
    }
}

// ----------------------------------------------------------------------
// Dashboard support
// ----------------------------------------------------------------------

/// Settings that override aegis.toml for future deployments. Omitted
/// fields fall back to aegis.toml and runtime detection. An empty
/// `health_check_url` means "healthy while the process stays up".
#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectSettingsDto {
    pub install_command: Option<String>,
    pub build_command: Option<String>,
    pub test_command: Option<String>,
    pub start_command: Option<String>,
    pub port: Option<u16>,
    pub health_check_url: Option<String>,
    /// Default deployment strategy: GracefulSwitch, Immediate, Rolling or
    /// BlueGreen (the last two need a domain on the built-in proxy).
    pub strategy: Option<String>,
    /// Instances to run, 1-16 (more than 1 needs a domain on the built-in
    /// proxy). Takes effect with the next deployment.
    pub instances: Option<u32>,
    /// Seconds a new release may take to become healthy (1-3600; aegis.toml
    /// `[deploy] health_check_timeout_secs`, default 30). Raise it for apps
    /// that start slowly.
    pub health_check_timeout_secs: Option<u64>,
    /// Seconds a stopping release gets to finish in-flight requests and
    /// exit after SIGTERM (0-3600; aegis.toml `[deploy] drain_timeout_secs`,
    /// default 10).
    pub drain_timeout_secs: Option<u64>,
    /// Plain environment variables, merged over aegis.toml's `[env]` and
    /// written into each release. Use secrets for sensitive values.
    pub env: std::collections::BTreeMap<String, String>,
    /// Absolute path of an env file on the server (dotenv syntax), read at
    /// every build and every process start. It must be inside a directory
    /// listed in the daemon's `[apps] env_file_dirs`. Its values are never
    /// stored; Variables and Secrets override them. Omit (or send `null`)
    /// for none: like every field here, an omitted value is cleared.
    pub env_file: Option<String>,
}

impl From<aegis_config::ProjectOverrides> for ProjectSettingsDto {
    fn from(o: aegis_config::ProjectOverrides) -> Self {
        Self {
            install_command: o.install_command,
            build_command: o.build_command,
            test_command: o.test_command,
            start_command: o.start_command,
            port: o.port,
            health_check_url: o.health_check_url,
            strategy: o.strategy,
            instances: o.instances,
            health_check_timeout_secs: o.health_check_timeout_secs,
            drain_timeout_secs: o.drain_timeout_secs,
            env: o.env,
            env_file: o.env_file,
        }
    }
}

impl From<ProjectSettingsDto> for aegis_config::ProjectOverrides {
    fn from(d: ProjectSettingsDto) -> Self {
        Self {
            install_command: d.install_command,
            build_command: d.build_command,
            test_command: d.test_command,
            start_command: d.start_command,
            port: d.port,
            health_check_url: d.health_check_url,
            strategy: d.strategy,
            instances: d.instances,
            health_check_timeout_secs: d.health_check_timeout_secs,
            drain_timeout_secs: d.drain_timeout_secs,
            env: d.env,
            env_file: d.env_file,
        }
    }
}

/// A new source for an existing app: a git repository (`repository_url`,
/// optional `branch` and `source_subdir`) or a folder on the server
/// (`source_dir`, optional `source_subdir`). Give exactly one of
/// `repository_url` and `source_dir`. Validated like registration; takes
/// effect at the next deployment.
#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ChangeSourceRequest {
    pub repository_url: Option<String>,
    /// Branch to deploy (default: the app's current branch, else main).
    pub branch: Option<String>,
    /// Absolute path of a folder on the server.
    pub source_dir: Option<String>,
    /// Build and run only this folder of the source (monorepos).
    pub source_subdir: Option<String>,
}

/// What deleting an app did.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DeletedProjectDto {
    pub project_id: String,
    pub name: String,
    /// Processes (instances, standby) that were stopped.
    pub stopped_processes: usize,
    /// Domains the built-in proxy no longer serves; they can be mapped
    /// to another app right away.
    pub domains: Vec<String>,
    /// Secrets that were deleted.
    pub secrets_removed: u64,
    /// The release directories and logs were kept on disk.
    pub releases_kept: bool,
    /// Where they are, with `keep_releases`.
    pub kept_dir: Option<String>,
}

impl From<aegis_control::DeletedApp> for DeletedProjectDto {
    fn from(d: aegis_control::DeletedApp) -> Self {
        Self {
            project_id: d.project_id,
            name: d.name,
            stopped_processes: d.stopped_processes,
            domains: d.domains,
            secrets_removed: d.secrets_removed,
            releases_kept: d.releases_kept,
            kept_dir: d.kept_dir,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct DetectRequest {
    /// A directory on the server, or...
    pub source_dir: Option<String>,
    /// ...a git repository to clone (shallow) for detection.
    pub repository_url: Option<String>,
    pub branch: Option<String>,
    /// Look inside this folder of the source (monorepo apps).
    pub source_subdir: Option<String>,
    /// A registered app (id or name) whose deploy key is used to clone,
    /// and whose source folder is the default.
    pub project: Option<String>,
}

/// The settings a deployment from this source would use.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DetectedDto {
    pub name: String,
    pub runtime: String,
    pub install_command: Option<String>,
    pub build_command: Option<String>,
    pub test_command: Option<String>,
    pub start_command: String,
    pub port: Option<u16>,
    pub health_check_url: Option<String>,
    pub strategy: String,
    /// The package.json start script hard-codes its port: use
    /// `suggested_start` (reads `$PORT`) as the start command, and `port`.
    pub start_hint: Option<crate::sources::StartHintDto>,
}

impl From<aegis_engine::ResolvedProjectConfig> for DetectedDto {
    fn from(c: aegis_engine::ResolvedProjectConfig) -> Self {
        Self {
            name: c.name,
            runtime: c.runtime,
            install_command: c.install_command,
            build_command: c.build_command,
            test_command: c.test_command,
            start_command: c.start_command,
            port: c.port,
            health_check_url: c.health_check_url,
            strategy: c.strategy,
            start_hint: c.start_hint.map(Into::into),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HostDto {
    pub hostname: String,
    pub os: String,
    pub uptime_secs: u64,
    pub cpu_count: usize,
    /// 0-100, averaged over all cores.
    pub cpu_percent: f32,
    /// 1, 5 and 15 minute load averages.
    pub load_average: [f64; 3],
    pub memory_total_bytes: u64,
    pub memory_used_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
    /// Filesystem holding the data directory.
    pub disk_mount_point: String,
    pub disk_total_bytes: u64,
    pub disk_available_bytes: u64,
    pub sampled_at: String,
}

impl From<aegis_metrics::HostSnapshot> for HostDto {
    fn from(h: aegis_metrics::HostSnapshot) -> Self {
        Self {
            hostname: h.hostname,
            os: h.os,
            uptime_secs: h.uptime_secs,
            cpu_count: h.cpu_count,
            cpu_percent: h.cpu_percent,
            load_average: h.load_average,
            memory_total_bytes: h.memory_total_bytes,
            memory_used_bytes: h.memory_used_bytes,
            swap_total_bytes: h.swap_total_bytes,
            swap_used_bytes: h.swap_used_bytes,
            disk_mount_point: h.disk_mount_point,
            disk_total_bytes: h.disk_total_bytes,
            disk_available_bytes: h.disk_available_bytes,
            sampled_at: h.sampled_at,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BuildLogDto {
    pub deployment_id: String,
    pub lines: Vec<String>,
}

// ----------------------------------------------------------------------
// Resources and metrics
// ----------------------------------------------------------------------

/// Limits for one app; `null` means unlimited.
#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
pub struct ResourceLimitsDto {
    /// CPU time in cores (0.5 = half of one core).
    pub cpu_cores: Option<f64>,
    /// Hard memory limit in bytes; the soft limit is 90% of it.
    pub memory_bytes: Option<u64>,
    /// Maximum number of processes and threads.
    pub pids_max: Option<u64>,
}

impl From<aegis_config::ResourceLimits> for ResourceLimitsDto {
    fn from(l: aegis_config::ResourceLimits) -> Self {
        Self {
            cpu_cores: l.cpu_cores,
            memory_bytes: l.memory_bytes,
            pids_max: l.pids_max,
        }
    }
}

/// New limits. Omitted or `null` fields become unlimited.
#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SetResourcesRequest {
    pub cpu_cores: Option<f64>,
    pub memory_bytes: Option<u64>,
    pub pids_max: Option<u64>,
    /// Apply even if the memory limit is below current use or more memory
    /// is promised to apps than the host has (otherwise 409
    /// `confirmation_required`).
    pub force: bool,
}

/// One measurement.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SampleDto {
    /// Unix milliseconds.
    pub at: i64,
    /// CPU in use, in cores.
    pub cpu_cores: f64,
    pub memory_bytes: u64,
    pub cpu_limit_cores: Option<f64>,
    pub memory_limit_bytes: Option<u64>,
    /// Share of CPU periods held back by the CPU limit, 0-1.
    pub throttled_ratio: f64,
    pub threads: u64,
    /// Open file descriptors.
    pub fds: u64,
    pub procs: u64,
    pub restarts: u32,
    /// Out-of-memory kills in the current process's cgroup.
    pub oom_kills: u64,
}

impl From<aegis_metrics::history::Sample> for SampleDto {
    fn from(s: aegis_metrics::history::Sample) -> Self {
        Self {
            at: s.at,
            cpu_cores: s.cpu_cores,
            memory_bytes: s.memory_bytes,
            cpu_limit_cores: s.cpu_limit_cores,
            memory_limit_bytes: s.memory_limit_bytes,
            throttled_ratio: s.throttled_ratio,
            threads: s.threads,
            fds: s.fds,
            procs: s.procs,
            restarts: s.restarts,
            oom_kills: s.oom_kills,
        }
    }
}

/// A chart point: average and peak over the bucket starting at `at`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PointDto {
    pub at: i64,
    pub cpu_cores: f64,
    pub cpu_cores_max: f64,
    pub memory_bytes: u64,
    pub memory_bytes_max: u64,
    pub cpu_limit_cores: Option<f64>,
    pub memory_limit_bytes: Option<u64>,
    pub throttled_ratio: f64,
}

impl From<aegis_metrics::history::Point> for PointDto {
    fn from(p: aegis_metrics::history::Point) -> Self {
        Self {
            at: p.at,
            cpu_cores: p.cpu_cores,
            cpu_cores_max: p.cpu_cores_max,
            memory_bytes: p.memory_bytes,
            memory_bytes_max: p.memory_bytes_max,
            cpu_limit_cores: p.cpu_limit_cores,
            memory_limit_bytes: p.memory_limit_bytes,
            throttled_ratio: p.throttled_ratio,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct MetricsDto {
    /// A project id, or "host".
    pub series: String,
    pub range_secs: u64,
    /// Oldest first.
    pub points: Vec<PointDto>,
}

/// Whether container apps (`[run] runtime = "docker"`) can run here.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DockerStatusDto {
    pub available: bool,
    /// The Docker Engine version.
    pub version: Option<String>,
    /// Why Docker can't be used (not installed, daemon not running, no permission).
    pub reason: Option<String>,
}

impl From<aegis_control::DockerStatus> for DockerStatusDto {
    fn from(s: aegis_control::DockerStatus) -> Self {
        Self {
            available: s.available,
            version: s.version,
            reason: s.reason,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ResourceCapabilitiesDto {
    /// "cgroup2", "cgroup1", "docker" (a container app's limits, applied by
    /// Docker) or "none".
    pub backend: String,
    pub cpu: bool,
    pub memory: bool,
    pub pids: bool,
    /// Where app cgroups are created.
    pub location: Option<String>,
    /// Why limits are unavailable, or partly so.
    pub reason: Option<String>,
    pub host_cpu_cores: usize,
    pub host_memory_bytes: u64,
    /// Sums of all apps' limits.
    pub committed_memory_bytes: u64,
    pub committed_cpu_cores: f64,
    /// Docker's status, once checked.
    pub docker: Option<DockerStatusDto>,
}

impl From<aegis_control::ResourceOverview> for ResourceCapabilitiesDto {
    fn from(o: aegis_control::ResourceOverview) -> Self {
        let c = o.capabilities;
        Self {
            backend: c.backend.as_str().into(),
            cpu: c.cpu,
            memory: c.memory,
            pids: c.pids,
            location: c.location.map(|p| p.display().to_string()),
            reason: c.reason,
            host_cpu_cores: o.host_cpu_cores,
            host_memory_bytes: o.host_memory_bytes,
            committed_memory_bytes: o.committed_memory_bytes,
            committed_cpu_cores: o.committed_cpu_cores,
            docker: o.docker.map(Into::into),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProjectResourcesDto {
    pub project_id: String,
    pub limits: ResourceLimitsDto,
    pub usage: Option<SampleDto>,
    pub capabilities: ResourceCapabilitiesDto,
}

// ----------------------------------------------------------------------
// App configuration: secrets, aegis.toml preview, webhook setup
// ----------------------------------------------------------------------

/// A secret's name and when it last changed. Values are write-only.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SecretDto {
    pub name: String,
    pub updated_at: String,
}

impl From<aegis_control::SecretInfo> for SecretDto {
    fn from(s: aegis_control::SecretInfo) -> Self {
        Self {
            name: s.name,
            updated_at: s.updated_at,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetSecretRequest {
    /// The value; stored encrypted and never returned.
    pub value: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SecretSaved {
    pub name: String,
    /// False when an existing secret was replaced.
    pub created: bool,
    /// The running process still has the old value; restart it to apply.
    pub restart_required: bool,
}

/// An app's env file as the daemon sees it right now.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EnvFileStatusDto {
    pub path: String,
    /// The file was read and parsed just now.
    pub readable: bool,
    /// Why it can't be used, e.g. permission denied (never contains values).
    pub error: Option<String>,
    /// Variable names it defines (values are never returned).
    pub names: Vec<String>,
}

/// One variable the app gets and where its value comes from.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EnvSourceDto {
    pub name: String,
    /// `env_file`, `aegis_toml`, `variable`, `secret`, or `aegis` (`PORT`,
    /// which Aegis sets; an env file's `PORT` is ignored).
    pub source: String,
    /// Lower-precedence sources that also define it and are overridden.
    pub overrides: Vec<String>,
    /// Build commands see it (env file, `[env]` and Variables; not Secrets).
    pub build: bool,
}

/// Names and sources of an app's environment variables. Values are never
/// included. Precedence, lowest first: env file, `[env]` in aegis.toml,
/// Variables, Secrets, then `PORT`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EnvOverviewDto {
    /// Directories env files may be in (`[apps] env_file_dirs`); empty
    /// means env files are turned off on this server.
    pub env_file_dirs: Vec<String>,
    pub env_file: Option<EnvFileStatusDto>,
    pub variables: Vec<EnvSourceDto>,
}

impl From<aegis_control::EnvOverview> for EnvOverviewDto {
    fn from(o: aegis_control::EnvOverview) -> Self {
        Self {
            env_file_dirs: o
                .env_file_dirs
                .iter()
                .map(|d| d.display().to_string())
                .collect(),
            env_file: o.env_file.map(|f| EnvFileStatusDto {
                path: f.path,
                readable: f.readable,
                error: f.error,
                names: f.names,
            }),
            variables: o
                .variables
                .into_iter()
                .map(|v| EnvSourceDto {
                    name: v.name,
                    source: v.source.to_string(),
                    overrides: v.overrides.iter().map(|s| s.to_string()).collect(),
                    build: v.build,
                })
                .collect(),
        }
    }
}

/// The contents of a `.env` file, pasted or read in the browser.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvImportPreviewRequest {
    /// dotenv text, at most 1 MiB. Never stored or logged.
    pub content: String,
}

/// A variable found in the pasted file. Its value is not returned.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EnvImportCandidateDto {
    pub name: String,
    /// `variable`, `secret` or `skip`.
    pub suggested: String,
    /// Why this is suggested.
    pub reason: String,
    /// The value spans several lines (only possible as a secret).
    pub multiline: bool,
    /// `variable` or `secret` when the app already has this name.
    pub exists_as: Option<String>,
}

impl From<aegis_control::ImportCandidate> for EnvImportCandidateDto {
    fn from(c: aegis_control::ImportCandidate) -> Self {
        Self {
            name: c.name,
            suggested: c.suggested.to_string(),
            reason: c.reason.to_string(),
            multiline: c.multiline,
            exists_as: c.exists_as.map(str::to_string),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EnvImportPreviewDto {
    pub variables: Vec<EnvImportCandidateDto>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvImportRequest {
    /// The same dotenv text as for the preview; parsed again on the server.
    pub content: String,
    /// Name -> `variable`, `secret` or `skip`. Names not listed are skipped.
    pub choices: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EnvImportResult {
    /// Saved as Variables (written into the next release's aegis.toml).
    pub variables: Vec<String>,
    /// Saved as Secrets (encrypted). A Variable of the same name was removed.
    pub secrets: Vec<String>,
    pub skipped: Vec<String>,
    /// The running process still has the old environment; restart it to apply secrets.
    pub restart_required: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DiffLineDto {
    /// " " unchanged, "-" removed, "+" added.
    pub op: String,
    pub text: String,
}

/// The project's aegis.toml with current and proposed settings applied.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ConfigPreviewDto {
    /// "source" (the project's directory), "release" (the live release) or
    /// "none" (no aegis.toml yet).
    pub base: String,
    pub current: String,
    pub proposed: String,
    pub diff: Vec<DiffLineDto>,
    pub changed: bool,
}

impl From<aegis_control::ConfigPreview> for ConfigPreviewDto {
    fn from(p: aegis_control::ConfigPreview) -> Self {
        Self {
            base: p.base.to_string(),
            current: p.current,
            proposed: p.proposed,
            diff: p
                .diff
                .into_iter()
                .map(|l| DiffLineDto {
                    op: l.op.to_string(),
                    text: l.text,
                })
                .collect(),
            changed: p.changed,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct WebhookDeliveryDto {
    pub received_at: String,
    /// GitHub's X-GitHub-Delivery id.
    pub delivery: String,
    /// GitHub event name, e.g. "push" or "ping".
    pub event: String,
    /// "queued", "ignored", "duplicate", "pong" or "error".
    pub status: String,
    pub reason: Option<String>,
    pub deployment_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct WebhookStatusDto {
    /// A secret exists, so deliveries are accepted.
    pub configured: bool,
    /// Payload URL path; prefix it with the API's public base URL.
    pub path: String,
    pub content_type: String,
    pub events: Vec<String>,
    /// Branch whose pushes deploy.
    pub branch: String,
    /// Pushes need a repository to clone.
    pub repository_url: String,
    /// Most recent signed deliveries, newest first.
    pub recent: Vec<WebhookDeliveryDto>,
}
