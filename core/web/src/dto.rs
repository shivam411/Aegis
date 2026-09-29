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
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProjectDto {
    pub id: String,
    pub name: String,
    pub repository_url: String,
    pub branch: String,
    pub runtime: Option<String>,
    pub source_dir: Option<String>,
    /// Version of the live release.
    pub current_release: Option<String>,
    pub created_at: String,
    pub schedule: Option<ScheduleDto>,
    /// Settings edited in the dashboard (override aegis.toml).
    pub settings: ProjectSettingsDto,
    /// The process serving the live release.
    pub process: Option<ProcessDto>,
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
}

#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct DeployRequest {
    pub branch: Option<String>,
    /// GracefulSwitch (default) or Immediate.
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
            current_release: p.current_release,
            created_at: p.created_at,
            schedule: p.schedule.map(Into::into),
            settings: p.settings.into(),
            process: view.process.map(Into::into),
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
