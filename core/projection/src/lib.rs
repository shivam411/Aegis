//! In-memory read models folded from the event stream (see docs/event_catalog.md).
//!
//! The projection is rebuilt from the event store on startup and then kept
//! current by applying each new event in sequence order.

use aegis_types::{ArtifactId, DeploymentId, Event, ProcessId, ProjectId, ReleaseId, ServerId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectState {
    pub id: ProjectId,
    pub name: String,
    pub repository_url: String,
    pub branch: String,
    pub status: String,
    pub created_at: String,
    pub runtime: Option<String>,
    /// Local directory deployments copy from when no other source is given.
    pub source_dir: Option<String>,
    /// Version of the release that is currently live.
    pub current_release: Option<String>,
    /// The process serving the current release.
    pub current_process: Option<ProcessId>,
    /// Daily auto-deployment, if configured.
    pub schedule: Option<ScheduleState>,
    /// Settings edited in the dashboard, applied on top of aegis.toml.
    #[serde(default)]
    pub settings: aegis_config::ProjectOverrides,
    /// CPU, memory and process limits (unlimited by default).
    #[serde(default)]
    pub resources: aegis_config::ResourceLimits,
    /// Domains the built-in proxy serves this app on.
    #[serde(default)]
    pub domains: Vec<DomainState>,
    /// The processes serving the live release, one per instance (in
    /// instance order). `current_process` is the first of them. Empty for
    /// projects recorded before instances existed; see
    /// [`ProjectState::live_processes`].
    #[serde(default)]
    pub instances: Vec<ProcessId>,
}

impl ProjectState {
    /// The processes serving the live release (all instances).
    pub fn live_processes(&self) -> Vec<ProcessId> {
        if self.instances.is_empty() {
            self.current_process.into_iter().collect()
        } else {
            self.instances.clone()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DomainState {
    /// Lowercase hostname.
    pub domain: String,
    /// "acme", "files" or "off".
    pub tls: String,
    pub cert_path: Option<String>,
    pub key_path: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScheduleState {
    /// UTC hour, 0-23.
    pub hour: u32,
    pub minute: u32,
    pub branch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReleaseState {
    pub id: ReleaseId,
    pub project_id: ProjectId,
    pub version: String,
    pub commit_sha: String,
    pub commit_message: String,
    pub artifacts: Vec<ArtifactId>,
    pub checksum: Option<String>,
    pub path: Option<String>,
    pub deployment_id: Option<DeploymentId>,
    /// Built, Active, Inactive, Failed (never went live) or Archived.
    pub status: String,
    pub created_at: String,
    pub seq: u64,
    /// When this release last went live (event sequence); orders releases
    /// by "most recently live", which differs from build order after a rollback.
    #[serde(default)]
    pub promoted_seq: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeploymentState {
    pub id: DeploymentId,
    pub project_id: ProjectId,
    pub release_id: Option<ReleaseId>,
    pub version: Option<String>,
    pub strategy: String,
    pub trigger: Option<String>,
    /// Queued, InProgress, Success, Failed or RolledBack.
    pub status: String,
    /// Last pipeline stage reported, e.g. "Build".
    pub stage: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessState {
    pub id: ProcessId,
    pub project_id: ProjectId,
    pub pid: Option<u32>,
    /// Running, Backoff, Stopped, Exited or Failed.
    pub status: String,
    /// "running" or "stopped": what the operator wants, used on daemon boot.
    pub desired: String,
    pub restart_count: u32,
    pub last_start: Option<String>,
    pub last_exit_code: Option<i32>,
    pub release_version: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    /// The port the process was told to listen on (`PORT`).
    #[serde(default)]
    pub port: Option<u16>,
    /// The port was allocated for the proxy (rather than the configured one).
    #[serde(default)]
    pub proxied: bool,
    /// Instance number (1-based) when the app runs several instances.
    #[serde(default)]
    pub instance: Option<u32>,
    /// "docker" for container apps; `None` for plain processes.
    #[serde(default)]
    pub runtime: Option<String>,
    /// The container's name, for container apps.
    #[serde(default)]
    pub container: Option<String>,
    /// The image the container runs, for container apps.
    #[serde(default)]
    pub image: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerState {
    pub id: ServerId,
    pub hostname: String,
    pub ip_address: String,
    pub status: String,
    pub last_heartbeat: String,
}

pub trait Projection: Send + Sync {
    fn apply_event(&mut self, event: &Event);
}

#[derive(Default)]
struct State {
    projects: HashMap<ProjectId, ProjectState>,
    releases: HashMap<ReleaseId, ReleaseState>,
    deployments: HashMap<DeploymentId, DeploymentState>,
    processes: HashMap<ProcessId, ProcessState>,
    servers: HashMap<ServerId, ServerState>,
    seq: u64,
}

#[derive(Clone, Default)]
pub struct ProjectionEngine {
    state: Arc<RwLock<State>>,
}

fn str_field<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(|v| v.as_str())
}

fn id_field<T: std::str::FromStr>(payload: &Value, key: &str) -> Option<T> {
    str_field(payload, key).and_then(|s| s.parse().ok())
}

fn string_field(payload: &Value, key: &str) -> Option<String> {
    str_field(payload, key).map(str::to_string)
}

/// A list of ids, e.g. `process_ids`; invalid entries are skipped.
fn id_list<T: std::str::FromStr>(payload: &Value, key: &str) -> Vec<T> {
    payload
        .get(key)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()?.parse().ok()).collect())
        .unwrap_or_default()
}

/// The deployment an event refers to. Events written before deployment ids
/// were included fall back to the project's latest unfinished deployment.
fn target_deployment(st: &State, payload: &Value) -> Option<DeploymentId> {
    if let Some(id) = id_field::<DeploymentId>(payload, "deployment_id") {
        return Some(id);
    }
    let project_id = id_field::<ProjectId>(payload, "project_id")?;
    st.deployments
        .values()
        .filter(|d| d.project_id == project_id)
        .filter(|d| matches!(d.status.as_str(), "Queued" | "InProgress"))
        .max_by_key(|d| d.seq)
        .map(|d| d.id)
}

impl ProjectionEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replays a historic sequence of events to reconstruct current state caches.
    pub fn replay_from_store(&self, events: &[Event]) {
        for event in events {
            self.apply_event(event);
        }
    }

    /// Processes an individual domain event and updates the relevant in-memory projection models.
    pub fn apply_event(&self, event: &Event) {
        let payload: Value = match serde_json::from_str(&event.payload_json) {
            Ok(val) => val,
            Err(e) => {
                tracing::warn!(event_id = %event.id, error = %e, "Failed to parse event JSON for projection");
                return;
            }
        };
        let mut st = self.state.write().unwrap();
        st.seq += 1;
        let seq = st.seq;
        let at = event.created_at.clone();

        match event.event_type.as_str() {
            "ProjectCreated" => {
                let (Some(id), Some(name)) = (
                    id_field::<ProjectId>(&payload, "project_id"),
                    str_field(&payload, "name"),
                ) else {
                    return;
                };
                let previous = st.projects.get(&id).cloned();
                st.projects.insert(
                    id,
                    ProjectState {
                        id,
                        name: name.to_string(),
                        repository_url: string_field(&payload, "repository_url")
                            .unwrap_or_default(),
                        branch: string_field(&payload, "branch")
                            .unwrap_or_else(|| "main".to_string()),
                        status: "Active".to_string(),
                        created_at: previous
                            .as_ref()
                            .map(|p| p.created_at.clone())
                            .unwrap_or(at),
                        runtime: string_field(&payload, "runtime"),
                        source_dir: string_field(&payload, "source_dir"),
                        current_release: previous.as_ref().and_then(|p| p.current_release.clone()),
                        current_process: previous.as_ref().and_then(|p| p.current_process),
                        schedule: previous.as_ref().and_then(|p| p.schedule.clone()),
                        settings: previous
                            .as_ref()
                            .map(|p| p.settings.clone())
                            .unwrap_or_default(),
                        resources: previous
                            .as_ref()
                            .map(|p| p.resources.clone())
                            .unwrap_or_default(),
                        domains: previous
                            .as_ref()
                            .map(|p| p.domains.clone())
                            .unwrap_or_default(),
                        instances: previous.map(|p| p.instances).unwrap_or_default(),
                    },
                );
            }
            "ProjectSettingsUpdated" => {
                if let (Some(project), Some(settings)) = (
                    id_field::<ProjectId>(&payload, "project_id")
                        .and_then(|id| st.projects.get_mut(&id)),
                    payload
                        .get("settings")
                        .and_then(|v| serde_json::from_value(v.clone()).ok()),
                ) {
                    project.settings = settings;
                }
            }
            "ResourceLimitsChanged" => {
                if let (Some(project), Some(limits)) = (
                    id_field::<ProjectId>(&payload, "project_id")
                        .and_then(|id| st.projects.get_mut(&id)),
                    payload
                        .get("limits")
                        .and_then(|v| serde_json::from_value(v.clone()).ok()),
                ) {
                    project.resources = limits;
                }
            }
            "DomainMapped" => {
                let (Some(project_id), Some(domain)) = (
                    id_field::<ProjectId>(&payload, "project_id"),
                    string_field(&payload, "domain"),
                ) else {
                    return;
                };
                // A domain belongs to one project; re-mapping replaces it.
                for project in st.projects.values_mut() {
                    project.domains.retain(|d| d.domain != domain);
                }
                if let Some(project) = st.projects.get_mut(&project_id) {
                    project.domains.push(DomainState {
                        domain,
                        tls: string_field(&payload, "tls").unwrap_or_else(|| "off".into()),
                        cert_path: string_field(&payload, "cert_path"),
                        key_path: string_field(&payload, "key_path"),
                        created_at: at,
                    });
                    project.domains.sort_by(|a, b| a.domain.cmp(&b.domain));
                }
            }
            "DomainUnmapped" => {
                if let (Some(project), Some(domain)) = (
                    id_field::<ProjectId>(&payload, "project_id")
                        .and_then(|id| st.projects.get_mut(&id)),
                    str_field(&payload, "domain"),
                ) {
                    project.domains.retain(|d| d.domain != domain);
                }
            }
            "ScheduleConfigured" => {
                if let Some(project) = id_field::<ProjectId>(&payload, "project_id")
                    .and_then(|id| st.projects.get_mut(&id))
                {
                    project.schedule = Some(ScheduleState {
                        hour: payload.get("hour").and_then(|v| v.as_u64()).unwrap_or(0) as u32 % 24,
                        minute: payload.get("minute").and_then(|v| v.as_u64()).unwrap_or(0) as u32
                            % 60,
                        branch: string_field(&payload, "branch")
                            .unwrap_or_else(|| "main".to_string()),
                    });
                }
            }
            "DeploymentQueued" => {
                let (Some(id), Some(project_id)) = (
                    id_field::<DeploymentId>(&payload, "deployment_id"),
                    id_field::<ProjectId>(&payload, "project_id"),
                ) else {
                    return;
                };
                st.deployments.insert(
                    id,
                    DeploymentState {
                        id,
                        project_id,
                        release_id: id_field(&payload, "release_id"),
                        version: string_field(&payload, "version"),
                        strategy: string_field(&payload, "strategy")
                            .unwrap_or_else(|| "GracefulSwitch".to_string()),
                        trigger: string_field(&payload, "trigger_source"),
                        status: "Queued".to_string(),
                        stage: None,
                        error: None,
                        created_at: at,
                        completed_at: None,
                        seq,
                    },
                );
            }
            "DeploymentStarted" => {
                if let Some(dep) =
                    target_deployment(&st, &payload).and_then(|id| st.deployments.get_mut(&id))
                {
                    dep.status = "InProgress".to_string();
                    if let Some(v) = string_field(&payload, "version") {
                        dep.version = Some(v);
                    }
                }
            }
            t if t.starts_with("BuildStage") => {
                if let Some(dep) = id_field::<DeploymentId>(&payload, "deployment_id")
                    .and_then(|id| st.deployments.get_mut(&id))
                {
                    dep.stage = string_field(&payload, "stage");
                }
            }
            "DeploymentCompleted" | "DeploymentSucceeded" => {
                if let Some(dep) =
                    target_deployment(&st, &payload).and_then(|id| st.deployments.get_mut(&id))
                {
                    dep.status = "Success".to_string();
                    dep.completed_at = Some(at);
                    if let Some(r) = id_field(&payload, "release_id") {
                        dep.release_id = Some(r);
                    }
                }
            }
            "DeploymentFailed" => {
                let release_id = target_deployment(&st, &payload)
                    .and_then(|id| st.deployments.get_mut(&id))
                    .and_then(|dep| {
                        dep.status = "Failed".to_string();
                        dep.error = string_field(&payload, "reason");
                        dep.completed_at = Some(at);
                        dep.release_id
                    });
                // A release that was built but never went live.
                if let Some(release) = release_id.and_then(|id| st.releases.get_mut(&id)) {
                    if release.status == "Built" {
                        release.status = "Failed".to_string();
                    }
                }
            }
            "DeploymentRolledBack" => {
                if let Some(dep) = id_field::<DeploymentId>(&payload, "deployment_id")
                    .and_then(|id| st.deployments.get_mut(&id))
                {
                    dep.status = "RolledBack".to_string();
                }
            }
            "ReleaseCreated" => {
                let (Some(id), Some(project_id), Some(version)) = (
                    id_field::<ReleaseId>(&payload, "release_id"),
                    id_field::<ProjectId>(&payload, "project_id"),
                    string_field(&payload, "version"),
                ) else {
                    return;
                };
                let deployment_id = id_field::<DeploymentId>(&payload, "deployment_id");
                if let Some(dep) = deployment_id.and_then(|d| st.deployments.get_mut(&d)) {
                    dep.release_id = Some(id);
                    dep.version = Some(version.clone());
                }
                st.releases.insert(
                    id,
                    ReleaseState {
                        id,
                        project_id,
                        version,
                        commit_sha: string_field(&payload, "commit_sha").unwrap_or_default(),
                        commit_message: string_field(&payload, "commit_message")
                            .unwrap_or_default(),
                        artifacts: payload
                            .get("artifacts")
                            .and_then(|a| a.as_array())
                            .map(|a| a.iter().filter_map(|v| v.as_str()?.parse().ok()).collect())
                            .unwrap_or_default(),
                        checksum: string_field(&payload, "checksum"),
                        path: string_field(&payload, "path"),
                        deployment_id,
                        status: "Built".to_string(),
                        created_at: at,
                        seq,
                        promoted_seq: None,
                    },
                );
            }
            "ReleasePromoted" => {
                let (Some(release_id), Some(project_id)) = (
                    id_field::<ReleaseId>(&payload, "release_id"),
                    id_field::<ProjectId>(&payload, "project_id"),
                ) else {
                    return;
                };
                for release in st.releases.values_mut() {
                    if release.project_id == project_id && release.status == "Active" {
                        release.status = "Inactive".to_string();
                    }
                }
                let version = st.releases.get_mut(&release_id).map(|r| {
                    r.status = "Active".to_string();
                    r.promoted_seq = Some(seq);
                    r.version.clone()
                });
                let process = id_field::<ProcessId>(&payload, "process_id");
                // Several instances: `process_ids` lists them all.
                let instances = id_list(&payload, "process_ids");
                if let Some(project) = st.projects.get_mut(&project_id) {
                    project.current_release = version.or_else(|| string_field(&payload, "version"));
                    if !instances.is_empty() {
                        project.current_process = instances.first().copied();
                        project.instances = instances;
                    } else if let Some(process) = process {
                        project.current_process = Some(process);
                        project.instances = vec![process];
                    }
                }
            }
            "ReleaseArchived" => {
                let (Some(project_id), Some(version)) = (
                    id_field::<ProjectId>(&payload, "project_id"),
                    str_field(&payload, "version"),
                ) else {
                    return;
                };
                for release in st.releases.values_mut() {
                    if release.project_id == project_id && release.version == version {
                        release.status = "Archived".to_string();
                    }
                }
            }
            "ProcessStarted" => {
                let (Some(id), Some(project_id)) = (
                    id_field::<ProcessId>(&payload, "process_id"),
                    id_field::<ProjectId>(&payload, "project_id"),
                ) else {
                    return;
                };
                let previous = st.processes.get(&id).cloned();
                let keep = |key: &str, old: Option<String>| string_field(&payload, key).or(old);
                st.processes.insert(
                    id,
                    ProcessState {
                        id,
                        project_id,
                        pid: payload
                            .get("pid")
                            .and_then(|v| v.as_u64())
                            .map(|p| p as u32),
                        status: "Running".to_string(),
                        desired: "running".to_string(),
                        restart_count: payload
                            .get("restart_count")
                            .and_then(|v| v.as_u64())
                            .map(|c| c as u32)
                            .or(previous.as_ref().map(|p| p.restart_count))
                            .unwrap_or(0),
                        last_start: Some(at),
                        last_exit_code: None,
                        release_version: keep(
                            "release_version",
                            previous.as_ref().and_then(|p| p.release_version.clone()),
                        ),
                        command: keep("command", previous.as_ref().and_then(|p| p.command.clone())),
                        cwd: keep("cwd", previous.as_ref().and_then(|p| p.cwd.clone())),
                        port: payload
                            .get("port")
                            .and_then(|v| v.as_u64())
                            .and_then(|p| u16::try_from(p).ok())
                            .or(previous.as_ref().and_then(|p| p.port)),
                        proxied: payload
                            .get("proxied")
                            .and_then(|v| v.as_bool())
                            .or(previous.as_ref().map(|p| p.proxied))
                            .unwrap_or(false),
                        instance: payload
                            .get("instance")
                            .and_then(|v| v.as_u64())
                            .map(|n| n as u32)
                            .or(previous.as_ref().and_then(|p| p.instance)),
                        // Each start says how it runs; older events say nothing
                        // (a plain process).
                        runtime: string_field(&payload, "runtime").filter(|r| r != "process"),
                        container: string_field(&payload, "container"),
                        image: string_field(&payload, "image"),
                    },
                );
                // A candidate is a new release starting next to the live one
                // (proxied cutover), or an instance outside the live set; it
                // becomes current when promoted. A restart of an instance
                // already in the live set changes nothing.
                let candidate = payload
                    .get("candidate")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if let (false, Some(project)) = (candidate, st.projects.get_mut(&project_id)) {
                    if !project.instances.contains(&id) {
                        project.current_process = Some(id);
                        project.instances = vec![id];
                    }
                }
            }
            "InstanceSetChanged" => {
                let Some(project_id) = id_field::<ProjectId>(&payload, "project_id") else {
                    return;
                };
                let instances = id_list(&payload, "process_ids");
                if let (Some(project), false) =
                    (st.projects.get_mut(&project_id), instances.is_empty())
                {
                    project.current_process = instances.first().copied();
                    project.instances = instances;
                }
            }
            "ProcessCrashed" => {
                if let Some(proc) = id_field::<ProcessId>(&payload, "process_id")
                    .and_then(|id| st.processes.get_mut(&id))
                {
                    let will_restart = payload
                        .get("will_restart")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    proc.status = if will_restart { "Backoff" } else { "Exited" }.to_string();
                    proc.pid = None;
                    proc.last_exit_code = payload
                        .get("exit_code")
                        .and_then(|v| v.as_i64())
                        .map(|c| c as i32);
                }
            }
            "ProcessFailed" => {
                if let Some(proc) = id_field::<ProcessId>(&payload, "process_id")
                    .and_then(|id| st.processes.get_mut(&id))
                {
                    proc.status = "Failed".to_string();
                    proc.pid = None;
                }
            }
            "ProcessStopped" => {
                let id = id_field::<ProcessId>(&payload, "process_id");
                // An instance replaced by a rollout that hasn't finished is
                // still in the live set: if the daemon stops before the new
                // set is promoted, it is started again on boot.
                let in_live_set = id.is_some_and(|id| {
                    st.projects
                        .values()
                        .any(|p| p.instances.len() > 1 && p.instances.contains(&id))
                });
                if let Some(proc) = id.and_then(|id| st.processes.get_mut(&id)) {
                    proc.status = "Stopped".to_string();
                    proc.pid = None;
                    // Shutdown and restart stops are not operator intent to keep it down.
                    match str_field(&payload, "reason") {
                        Some("shutdown") | Some("restart") => {}
                        Some("replaced") if in_live_set => {}
                        _ => proc.desired = "stopped".to_string(),
                    }
                    if let Some(code) = payload.get("exit_code").and_then(|v| v.as_i64()) {
                        proc.last_exit_code = Some(code as i32);
                    }
                }
            }
            _ => {}
        }
    }

    // Read Queries for CQRS Read Path
    pub fn get_projects(&self) -> Vec<ProjectState> {
        let mut projects: Vec<_> = self
            .state
            .read()
            .unwrap()
            .projects
            .values()
            .cloned()
            .collect();
        projects.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        projects
    }

    pub fn get_project(&self, id: &ProjectId) -> Option<ProjectState> {
        self.state.read().unwrap().projects.get(id).cloned()
    }

    /// Deployments, oldest first.
    pub fn get_deployments(&self) -> Vec<DeploymentState> {
        let mut deps: Vec<_> = self
            .state
            .read()
            .unwrap()
            .deployments
            .values()
            .cloned()
            .collect();
        deps.sort_by_key(|d| d.seq);
        deps
    }

    pub fn get_deployment(&self, id: &DeploymentId) -> Option<DeploymentState> {
        self.state.read().unwrap().deployments.get(id).cloned()
    }

    pub fn get_processes(&self) -> Vec<ProcessState> {
        self.state
            .read()
            .unwrap()
            .processes
            .values()
            .cloned()
            .collect()
    }

    pub fn get_process(&self, id: &ProcessId) -> Option<ProcessState> {
        self.state.read().unwrap().processes.get(id).cloned()
    }

    /// Releases, oldest first.
    pub fn get_releases(&self) -> Vec<ReleaseState> {
        let mut releases: Vec<_> = self
            .state
            .read()
            .unwrap()
            .releases
            .values()
            .cloned()
            .collect();
        releases.sort_by_key(|r| r.seq);
        releases
    }

    /// A project's releases, oldest first.
    pub fn get_project_releases(&self, project_id: &ProjectId) -> Vec<ReleaseState> {
        self.get_releases()
            .into_iter()
            .filter(|r| &r.project_id == project_id)
            .collect()
    }

    /// Releases that have been live, most recently live first.
    pub fn releases_by_recency(&self, project_id: &ProjectId) -> Vec<ReleaseState> {
        let mut releases: Vec<_> = self
            .get_project_releases(project_id)
            .into_iter()
            .filter(|r| r.promoted_seq.is_some())
            .collect();
        releases.sort_by_key(|r| std::cmp::Reverse(r.promoted_seq));
        releases
    }

    pub fn find_release(&self, project_id: &ProjectId, version: &str) -> Option<ReleaseState> {
        self.get_project_releases(project_id)
            .into_iter()
            .rev()
            .find(|r| r.version == version)
    }

    pub fn get_servers(&self) -> Vec<ServerState> {
        self.state
            .read()
            .unwrap()
            .servers
            .values()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(event_type: &str, payload: Value) -> Event {
        Event::new(event_type, payload.to_string())
    }

    #[test]
    fn test_projection_engine_event_folding() {
        let engine = ProjectionEngine::new();
        let proj_id = ProjectId::new();

        let mut event = ev(
            "ProjectCreated",
            serde_json::json!({
                "project_id": proj_id.to_string(),
                "name": "aegis-app",
                "repository_url": "https://github.com/aegis/app",
                "branch": "main",
                "source_dir": "/srv/app"
            }),
        );
        event.created_at = "2026-08-02T12:00:00Z".to_string();
        engine.apply_event(&event);

        let projects = engine.get_projects();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "aegis-app");
        assert_eq!(projects[0].id, proj_id);
        assert_eq!(projects[0].source_dir.as_deref(), Some("/srv/app"));

        engine.apply_event(&ev(
            "ScheduleConfigured",
            serde_json::json!({"project_id": proj_id.to_string(), "hour": 2, "minute": 30, "branch": "prod"}),
        ));
        assert_eq!(
            engine.get_project(&proj_id).unwrap().schedule,
            Some(ScheduleState {
                hour: 2,
                minute: 30,
                branch: "prod".to_string()
            })
        );

        engine.apply_event(&ev(
            "ProjectSettingsUpdated",
            serde_json::json!({"project_id": proj_id.to_string(), "settings": {"start_command": "node server.js", "port": 4000}}),
        ));
        engine.apply_event(&ev(
            "ResourceLimitsChanged",
            serde_json::json!({"project_id": proj_id.to_string(), "limits": {"cpu_cores": 0.5, "memory_bytes": 1048576}}),
        ));
        let resources = engine.get_project(&proj_id).unwrap().resources;
        assert_eq!(resources.cpu_cores, Some(0.5));
        assert_eq!(resources.memory_bytes, Some(1048576));
        assert_eq!(resources.pids_max, None);
        let settings = engine.get_project(&proj_id).unwrap().settings;
        assert_eq!(settings.start_command.as_deref(), Some("node server.js"));
        assert_eq!(settings.port, Some(4000));

        let single_proj = engine.get_project(&proj_id);
        assert!(single_proj.is_some());
        assert_eq!(single_proj.unwrap().name, "aegis-app");
    }

    #[test]
    fn test_full_deployment_lifecycle() {
        let engine = ProjectionEngine::new();
        let proj = ProjectId::new();
        let dep = DeploymentId::new();
        let rel = ReleaseId::new();
        let proc_id = ProcessId::new();
        let p = proj.to_string();

        engine.apply_event(&ev(
            "ProjectCreated",
            serde_json::json!({"project_id": p, "name": "app"}),
        ));
        engine.apply_event(&ev(
            "DeploymentQueued",
            serde_json::json!({"deployment_id": dep.to_string(), "project_id": p, "strategy": "GracefulSwitch"}),
        ));
        assert_eq!(engine.get_deployments()[0].status, "Queued");

        engine.apply_event(&ev(
            "DeploymentStarted",
            serde_json::json!({"deployment_id": dep.to_string(), "project_id": p, "version": "r1"}),
        ));
        engine.apply_event(&ev(
            "BuildStageBuild",
            serde_json::json!({"deployment_id": dep.to_string(), "stage": "Build", "status": "Started"}),
        ));
        let d = engine.get_deployment(&dep).unwrap();
        assert_eq!(d.status, "InProgress");
        assert_eq!(d.stage.as_deref(), Some("Build"));

        engine.apply_event(&ev(
            "ReleaseCreated",
            serde_json::json!({"release_id": rel.to_string(), "project_id": p, "deployment_id": dep.to_string(), "version": "r1", "commit_sha": "abc"}),
        ));
        engine.apply_event(&ev(
            "ProcessStarted",
            serde_json::json!({"process_id": proc_id.to_string(), "project_id": p, "pid": 1234, "release_version": "r1", "cwd": "/x", "command": "npm start"}),
        ));
        engine.apply_event(&ev(
            "ReleasePromoted",
            serde_json::json!({"release_id": rel.to_string(), "project_id": p}),
        ));
        engine.apply_event(&ev(
            "DeploymentCompleted",
            serde_json::json!({"deployment_id": dep.to_string(), "project_id": p, "release_id": rel.to_string()}),
        ));

        let project = engine.get_project(&proj).unwrap();
        assert_eq!(project.current_release.as_deref(), Some("r1"));
        assert_eq!(project.current_process, Some(proc_id));
        assert_eq!(engine.get_deployment(&dep).unwrap().status, "Success");
        assert_eq!(engine.get_deployment(&dep).unwrap().release_id, Some(rel));
        assert_eq!(engine.find_release(&proj, "r1").unwrap().status, "Active");

        let process = engine.get_process(&proc_id).unwrap();
        assert_eq!(process.status, "Running");
        assert_eq!(process.pid, Some(1234));
        assert_eq!(process.cwd.as_deref(), Some("/x"));

        // Crash with restart, then shutdown stop keeps desired=running.
        engine.apply_event(&ev(
            "ProcessCrashed",
            serde_json::json!({"process_id": proc_id.to_string(), "exit_code": 1, "will_restart": true}),
        ));
        assert_eq!(engine.get_process(&proc_id).unwrap().status, "Backoff");
        engine.apply_event(&ev(
            "ProcessStopped",
            serde_json::json!({"process_id": proc_id.to_string(), "reason": "shutdown"}),
        ));
        let process = engine.get_process(&proc_id).unwrap();
        assert_eq!(process.status, "Stopped");
        assert_eq!(process.desired, "running");

        // Restart keeps the recorded command/cwd when a later event omits them.
        engine.apply_event(&ev(
            "ProcessStarted",
            serde_json::json!({"process_id": proc_id.to_string(), "project_id": p, "pid": 99, "restart_count": 1}),
        ));
        let process = engine.get_process(&proc_id).unwrap();
        assert_eq!(process.command.as_deref(), Some("npm start"));
        assert_eq!(process.restart_count, 1);

        engine.apply_event(&ev(
            "ProcessStopped",
            serde_json::json!({"process_id": proc_id.to_string(), "reason": "requested"}),
        ));
        assert_eq!(engine.get_process(&proc_id).unwrap().desired, "stopped");

        engine.apply_event(&ev(
            "ReleaseArchived",
            serde_json::json!({"project_id": p, "version": "r1"}),
        ));
        assert_eq!(engine.find_release(&proj, "r1").unwrap().status, "Archived");
    }

    #[test]
    fn test_promotion_deactivates_previous_and_failed_deploys() {
        let engine = ProjectionEngine::new();
        let proj = ProjectId::new();
        let p = proj.to_string();
        let (r1, r2) = (ReleaseId::new(), ReleaseId::new());
        for (rel, v) in [(r1, "r1"), (r2, "r2")] {
            engine.apply_event(&ev(
                "ReleaseCreated",
                serde_json::json!({"release_id": rel.to_string(), "project_id": p, "version": v}),
            ));
        }
        engine.apply_event(&ev(
            "ReleasePromoted",
            serde_json::json!({"release_id": r1.to_string(), "project_id": p}),
        ));
        engine.apply_event(&ev(
            "ReleasePromoted",
            serde_json::json!({"release_id": r2.to_string(), "project_id": p}),
        ));
        // Rolling back to r1 makes it the most recently live release.
        engine.apply_event(&ev(
            "ReleasePromoted",
            serde_json::json!({"release_id": r1.to_string(), "project_id": p}),
        ));
        let recency: Vec<_> = engine
            .releases_by_recency(&proj)
            .into_iter()
            .map(|r| r.version)
            .collect();
        assert_eq!(recency, vec!["r1".to_string(), "r2".to_string()]);
        engine.apply_event(&ev(
            "ReleasePromoted",
            serde_json::json!({"release_id": r2.to_string(), "project_id": p}),
        ));

        let versions: Vec<_> = engine
            .get_project_releases(&proj)
            .into_iter()
            .map(|r| (r.version, r.status))
            .collect();
        assert_eq!(
            versions,
            vec![
                ("r1".to_string(), "Inactive".to_string()),
                ("r2".to_string(), "Active".to_string())
            ]
        );

        let dep = DeploymentId::new();
        engine.apply_event(&ev(
            "DeploymentQueued",
            serde_json::json!({"deployment_id": dep.to_string(), "project_id": p}),
        ));
        engine.apply_event(&ev(
            "DeploymentFailed",
            serde_json::json!({"deployment_id": dep.to_string(), "reason": "health check failed"}),
        ));
        engine.apply_event(&ev(
            "DeploymentRolledBack",
            serde_json::json!({"deployment_id": dep.to_string()}),
        ));
        let d = engine.get_deployment(&dep).unwrap();
        assert_eq!(d.status, "RolledBack");
        assert_eq!(d.error.as_deref(), Some("health check failed"));
    }

    #[test]
    fn test_domains_and_proxied_cutover() {
        let engine = ProjectionEngine::new();
        let (p1, p2) = (ProjectId::new(), ProjectId::new());
        for (id, name) in [(p1, "a"), (p2, "b")] {
            engine.apply_event(&ev(
                "ProjectCreated",
                serde_json::json!({"project_id": id.to_string(), "name": name}),
            ));
        }
        engine.apply_event(&ev(
            "DomainMapped",
            serde_json::json!({"project_id": p1.to_string(), "domain": "b.example.com", "tls": "acme"}),
        ));
        engine.apply_event(&ev(
            "DomainMapped",
            serde_json::json!({"project_id": p1.to_string(), "domain": "a.example.com", "tls": "off"}),
        ));
        let domains: Vec<_> = engine
            .get_project(&p1)
            .unwrap()
            .domains
            .into_iter()
            .map(|d| (d.domain, d.tls))
            .collect();
        assert_eq!(
            domains,
            vec![
                ("a.example.com".to_string(), "off".to_string()),
                ("b.example.com".to_string(), "acme".to_string())
            ]
        );
        // Re-registering keeps domains; moving a domain takes it away from p1.
        engine.apply_event(&ev(
            "ProjectCreated",
            serde_json::json!({"project_id": p1.to_string(), "name": "a"}),
        ));
        engine.apply_event(&ev(
            "DomainMapped",
            serde_json::json!({"project_id": p2.to_string(), "domain": "b.example.com", "tls": "files", "cert_path": "/c", "key_path": "/k"}),
        ));
        assert_eq!(engine.get_project(&p1).unwrap().domains.len(), 1);
        let moved = &engine.get_project(&p2).unwrap().domains[0];
        assert_eq!(moved.cert_path.as_deref(), Some("/c"));
        engine.apply_event(&ev(
            "DomainUnmapped",
            serde_json::json!({"project_id": p1.to_string(), "domain": "a.example.com"}),
        ));
        assert!(engine.get_project(&p1).unwrap().domains.is_empty());

        // A candidate process doesn't become current until it is promoted.
        let (old, new) = (ProcessId::new(), ProcessId::new());
        let rel = ReleaseId::new();
        engine.apply_event(&ev(
            "ProcessStarted",
            serde_json::json!({"process_id": old.to_string(), "project_id": p1.to_string(), "port": 3000}),
        ));
        engine.apply_event(&ev(
            "ProcessStarted",
            serde_json::json!({"process_id": new.to_string(), "project_id": p1.to_string(), "port": 41234, "proxied": true, "candidate": true}),
        ));
        assert_eq!(engine.get_project(&p1).unwrap().current_process, Some(old));
        let started = engine.get_process(&new).unwrap();
        assert_eq!((started.port, started.proxied), (Some(41234), true));
        assert_eq!(engine.get_process(&old).unwrap().port, Some(3000));
        engine.apply_event(&ev(
            "ReleaseCreated",
            serde_json::json!({"release_id": rel.to_string(), "project_id": p1.to_string(), "version": "r2"}),
        ));
        engine.apply_event(&ev(
            "ReleasePromoted",
            serde_json::json!({"release_id": rel.to_string(), "project_id": p1.to_string(), "process_id": new.to_string(), "port": 41234}),
        ));
        let project = engine.get_project(&p1).unwrap();
        assert_eq!(project.current_process, Some(new));
        assert_eq!(project.current_release.as_deref(), Some("r2"));
        // A restart event without a port keeps the recorded one.
        engine.apply_event(&ev(
            "ProcessStarted",
            serde_json::json!({"process_id": new.to_string(), "project_id": p1.to_string(), "restart_count": 1}),
        ));
        let restarted = engine.get_process(&new).unwrap();
        assert_eq!((restarted.port, restarted.proxied), (Some(41234), true));
    }

    #[test]
    fn test_legacy_events_without_deployment_id() {
        let engine = ProjectionEngine::new();
        let proj = ProjectId::new();
        let dep = DeploymentId::new();
        engine.apply_event(&ev(
            "DeploymentQueued",
            serde_json::json!({"deployment_id": dep.to_string(), "project_id": proj.to_string()}),
        ));
        // Older daemons omitted deployment_id from these.
        engine.apply_event(&ev(
            "DeploymentStarted",
            serde_json::json!({"project_id": proj.to_string(), "status": "In Progress"}),
        ));
        assert_eq!(engine.get_deployment(&dep).unwrap().status, "InProgress");
        engine.apply_event(&ev(
            "DeploymentCompleted",
            serde_json::json!({"project_id": proj.to_string(), "version": "v1.0.0"}),
        ));
        assert_eq!(engine.get_deployment(&dep).unwrap().status, "Success");
        // Legacy process events without a release are kept but not restartable.
        let proc_id = ProcessId::new();
        engine.apply_event(&ev(
            "ProcessStarted",
            serde_json::json!({"process_id": proc_id.to_string(), "project_id": proj.to_string(), "pid": 9000}),
        ));
        assert_eq!(engine.get_process(&proc_id).unwrap().release_version, None);
    }

    #[test]
    fn instance_sets_follow_promotions_restarts_and_recovery() {
        let engine = ProjectionEngine::new();
        let proj = ProjectId::new();
        let p = proj.to_string();
        engine.apply_event(&ev(
            "ProjectCreated",
            serde_json::json!({"project_id": p, "name": "web"}),
        ));
        let started = |id: ProcessId, candidate: bool, instance: u32| {
            ev(
                "ProcessStarted",
                serde_json::json!({"process_id": id.to_string(), "project_id": p, "pid": 1,
                    "release_version": "r1", "candidate": candidate, "instance": instance}),
            )
        };
        // A single process, as before instances existed.
        let single = ProcessId::new();
        engine.apply_event(&started(single, false, 1));
        let project = engine.get_project(&proj).unwrap();
        assert_eq!(project.current_process, Some(single));
        assert_eq!(project.live_processes(), vec![single]);

        // Three candidates, then one promotion makes them the live set.
        let set: Vec<ProcessId> = (0..3).map(|_| ProcessId::new()).collect();
        for (i, id) in set.iter().enumerate() {
            engine.apply_event(&started(*id, true, i as u32 + 1));
        }
        assert_eq!(
            engine.get_project(&proj).unwrap().current_process,
            Some(single)
        );
        engine.apply_event(&ev(
            "ReleasePromoted",
            serde_json::json!({"project_id": p, "release_id": ReleaseId::new().to_string(), "version": "r2", "process_id": set[0].to_string(),
                "process_ids": set.iter().map(|i| i.to_string()).collect::<Vec<_>>()}),
        ));
        let project = engine.get_project(&proj).unwrap();
        assert_eq!(project.instances, set);
        assert_eq!(project.current_process, Some(set[0]));
        assert_eq!(engine.get_process(&set[2]).unwrap().instance, Some(3));

        // An automatic restart of instance 2 doesn't make it the only one.
        engine.apply_event(&started(set[1], false, 2));
        assert_eq!(engine.get_project(&proj).unwrap().instances, set);

        // An instance replaced by an unfinished rollout stays wanted...
        engine.apply_event(&ev(
            "ProcessStopped",
            serde_json::json!({"process_id": set[1].to_string(), "project_id": p, "reason": "replaced"}),
        ));
        assert_eq!(engine.get_process(&set[1]).unwrap().desired, "running");
        // ...one the operator stopped does not.
        engine.apply_event(&ev(
            "ProcessStopped",
            serde_json::json!({"process_id": set[2].to_string(), "project_id": p, "reason": "requested"}),
        ));
        assert_eq!(engine.get_process(&set[2]).unwrap().desired, "stopped");

        // Recovery records a new set.
        let fresh = ProcessId::new();
        engine.apply_event(&ev(
            "InstanceSetChanged",
            serde_json::json!({"project_id": p,
                "process_ids": [set[0].to_string(), set[1].to_string(), fresh.to_string()]}),
        ));
        assert_eq!(
            engine.get_project(&proj).unwrap().instances,
            vec![set[0], set[1], fresh]
        );

        // Back to a single-instance promotion.
        engine.apply_event(&ev(
            "ReleasePromoted",
            serde_json::json!({"project_id": p, "release_id": ReleaseId::new().to_string(), "version": "r3", "process_id": single.to_string()}),
        ));
        let project = engine.get_project(&proj).unwrap();
        assert_eq!(project.instances, vec![single]);
        assert_eq!(project.current_process, Some(single));
    }

    #[test]
    fn test_projection_engine_replay_and_invalid_json() {
        let engine = ProjectionEngine::new();
        let invalid_event = Event::new("ProjectCreated", "{ invalid_json }");
        engine.apply_event(&invalid_event);
        assert!(engine.get_projects().is_empty());

        let proj_id = ProjectId::new();
        let event = ev(
            "ProjectCreated",
            serde_json::json!({
                "project_id": proj_id.to_string(),
                "name": "replayed-app",
                "repository_url": "https://github.com/aegis/replayed"
            }),
        );

        engine.replay_from_store(&[event]);
        assert_eq!(engine.get_projects().len(), 1);
        assert_eq!(engine.get_projects()[0].name, "replayed-app");
        assert!(engine.get_releases().is_empty());
        assert!(engine.get_servers().is_empty());
    }
}
