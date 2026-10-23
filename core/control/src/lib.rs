//! The control plane: the single place where Aegis turns commands into work.
//!
//! Every transport (gRPC today, HTTP and webhooks later) calls into
//! [`ControlPlane`]. Commands are recorded as events for the audit trail and
//! then executed; state is read back from the projection, which is updated
//! synchronously as each event is persisted.

pub mod backup;
mod config_file;
mod deploy;
mod docker;
mod domains;
mod env;
pub mod ports;
mod process;
mod resources;
mod rollout;
pub mod scan;
mod secrets;
mod sources;

use aegis_artifact_store::ArtifactStore;
use aegis_event_store::EventStore;
use aegis_process::ProcessSupervisor;
use aegis_projection::{
    DeploymentState, ProcessState, ProjectState, ProjectionEngine, ReleaseState,
};
use aegis_release::ReleaseSwitcher;
use aegis_types::{DeploymentId, Event, ProcessId, ProjectId};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

pub use aegis_docker::DockerStatus;
pub use aegis_secrets::SecretInfo;
pub use config_file::ConfigPreview;
pub use deploy::DeployRequest;
pub use domains::DomainView;
pub use env::{EnvFileStatus, EnvOverview, EnvSource, ImportAs, ImportCandidate, ImportResult};
pub use process::ProcessAction;
pub use resources::{MetricsUpdate, ResourceOverview, HOST_SERIES};
pub use rollout::{InstanceView, StandbyView};
pub use scan::{ImportApp, ImportedApp, ScanCandidate, ScanResult};
pub use sources::{AccessCheck, DeployKeyInfo};

tokio::task_local! {
    static ACTOR: String;
}

/// Runs `f` on behalf of `actor` (e.g. "user:admin", "token:ci", "cli").
/// Events recorded inside get an `actor` field, which makes the event
/// store an audit log of who did what.
pub async fn with_actor<F: std::future::Future>(actor: String, f: F) -> F::Output {
    ACTOR.scope(actor, f).await
}

/// The actor set by [`with_actor`] for the current task, if any.
pub fn current_actor() -> Option<String> {
    ACTOR.try_with(|a| a.clone()).ok()
}

/// Deployment strategies. `GracefulSwitch` (the default) is zero-downtime
/// for apps served by the built-in proxy; `Rolling` and `BlueGreen` need the
/// built-in proxy (see `rollout.rs`).
pub const SUPPORTED_STRATEGIES: &[&str] = aegis_config::STRATEGIES;

#[derive(Debug)]
pub enum ControlError {
    NotFound(String),
    InvalidArgument(String),
    FailedPrecondition(String),
    /// The request is allowed but risky; repeat it with `force` to proceed.
    NeedsConfirmation(String),
    Internal(anyhow::Error),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(m)
            | Self::InvalidArgument(m)
            | Self::FailedPrecondition(m)
            | Self::NeedsConfirmation(m) => {
                write!(f, "{}", m)
            }
            Self::Internal(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for ControlError {}

impl From<anyhow::Error> for ControlError {
    fn from(e: anyhow::Error) -> Self {
        Self::Internal(e)
    }
}

pub type ControlResult<T> = Result<T, ControlError>;

#[derive(Debug, Clone)]
pub struct ControlSettings {
    /// Root for releases, logs and artifact metadata.
    pub data_dir: PathBuf,
    /// Used when a project doesn't set `deploy.max_retained_versions`.
    pub max_retained_versions: usize,
    pub health_poll_interval: Duration,
    /// How long a health-URL-less release must stay up to count as healthy.
    pub liveness_window: Duration,
    /// Where app cgroups live (resource limits).
    pub resources: aegis_resources::Mode,
    /// How often apps and the host are measured.
    pub sample_interval: Duration,
    /// How long per-minute metric rollups are kept.
    pub metrics_retention: Duration,
    /// Backup retention and defaults.
    pub backup: backup::BackupSettings,
    /// Directories apps' env files may be read from (`[apps]
    /// env_file_dirs`); empty turns env files off.
    pub env_file_dirs: Vec<PathBuf>,
    /// Directories "Scan a folder" may look in (`[apps] import_dirs`, plus
    /// `env_file_dirs`); empty turns scanning off.
    pub import_dirs: Vec<PathBuf>,
    /// Host keys for git servers other than GitHub (`[git] known_hosts_file`).
    pub known_hosts_file: Option<PathBuf>,
}

impl ControlSettings {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            max_retained_versions: 2,
            health_poll_interval: Duration::from_millis(500),
            liveness_window: Duration::from_secs(3),
            resources: aegis_resources::Mode::Auto,
            sample_interval: Duration::from_secs(2),
            metrics_retention: Duration::from_secs(7 * 24 * 3600),
            backup: backup::BackupSettings::default(),
            env_file_dirs: Vec::new(),
            import_dirs: Vec::new(),
            known_hosts_file: None,
        }
    }
}

/// Input for registering (or re-registering) a project.
#[derive(Debug, Clone, Default)]
pub struct RegisterProject {
    pub project_id: Option<ProjectId>,
    pub name: String,
    pub repository_url: String,
    pub branch: String,
    pub runtime: Option<String>,
    pub source_dir: Option<PathBuf>,
    /// Build and run only this subdirectory of the source (monorepos).
    pub source_subdir: Option<String>,
}

/// A project together with the live state of its current process.
#[derive(Debug, Clone)]
pub struct ProjectView {
    pub project: ProjectState,
    pub process: Option<ProcessState>,
    /// The latest resource sample, while the app runs.
    pub usage: Option<aegis_metrics::history::Sample>,
    /// Every instance serving the live release (just `process` for
    /// single-instance apps).
    pub instances: Vec<InstanceView>,
    /// The previous release's instances, kept running after a BlueGreen
    /// switch as an instant rollback target.
    pub standby: Option<StandbyView>,
}

struct Inner {
    store: EventStore,
    projection: ProjectionEngine,
    supervisor: ProcessSupervisor,
    artifact_store: ArtifactStore,
    settings: ControlSettings,
    project_locks: Mutex<HashMap<ProjectId, Arc<tokio::sync::Mutex<()>>>>,
    /// Release version each supervised process was started from.
    process_release: Mutex<HashMap<ProcessId, String>>,
    shutting_down: AtomicBool,
    resources: Arc<aegis_resources::ResourceManager>,
    host: aegis_metrics::HostMonitor,
    history: aegis_metrics::history::History,
    metrics_tx: tokio::sync::broadcast::Sender<MetricsUpdate>,
    /// The built-in reverse proxy, when `[proxy]` is enabled.
    ingress: OnceLock<aegis_ingress::Ingress>,
    /// Port each supervised process was told to listen on, and whether it
    /// was allocated for the proxy.
    process_ports: Mutex<HashMap<ProcessId, ProcessPort>>,
    /// New releases starting next to the live one (proxied cutover), not
    /// yet promoted.
    candidates: Mutex<HashSet<ProcessId>>,
    /// Instance number of each process of a multi-instance app.
    process_instance: Mutex<HashMap<ProcessId, u32>>,
    /// BlueGreen: the previous release's instances, kept warm per project.
    standby: Mutex<HashMap<ProjectId, rollout::Standby>>,
    /// Latest CPU (cores) and memory (bytes) of each process.
    process_usage: Mutex<HashMap<ProcessId, (f64, u64)>>,
    /// Serializes domain changes so uniqueness checks can't race.
    domain_lock: tokio::sync::Mutex<()>,
    secrets: aegis_secrets::SecretStore,
    /// One backup at a time.
    backup_lock: tokio::sync::Mutex<()>,
    /// Container apps (`[run] runtime = "docker"`).
    docker: Arc<aegis_docker::Docker>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessPort {
    pub port: Option<u16>,
    pub proxied: bool,
}

#[derive(Clone)]
pub struct ControlPlane {
    inner: Arc<Inner>,
}

impl ControlPlane {
    /// Rebuilds the projection from the store and keeps it in sync from now on.
    pub async fn new(
        store: EventStore,
        supervisor: ProcessSupervisor,
        settings: ControlSettings,
    ) -> ControlResult<Self> {
        let projection = ProjectionEngine::new();
        let history = store.get_events().await?;
        projection.replay_from_store(&history);
        let listener_projection = projection.clone();
        store.add_sync_listener(Arc::new(move |event: &Event| {
            listener_projection.apply_event(event)
        }));
        tracing::info!(
            replayed = history.len(),
            "Projection rebuilt from event store"
        );

        std::fs::create_dir_all(&settings.data_dir).map_err(anyhow::Error::from)?;
        // Deploy key files left behind by a crash.
        aegis_ssh::clean_run_dir(&sources::ssh_run_dir(&settings.data_dir));
        // The key lives in the data directory, apart from the database.
        let secrets = aegis_secrets::SecretStore::open(
            store.pool().clone(),
            &settings.data_dir.join("secrets.key"),
        )
        .await?;
        let resources = Arc::new(aegis_resources::ResourceManager::new(&settings.resources));
        for project in projection.get_projects() {
            resources.load_limits(project.id, project.resources.clone());
        }
        supervisor.set_spawn_hook(resources.clone());
        let host = aegis_metrics::HostMonitor::new(settings.data_dir.clone());
        let history = aegis_metrics::history::History::new(
            Some(store.pool().clone()),
            Duration::from_secs(3600),
            settings.metrics_retention,
        );
        let (metrics_tx, _) = tokio::sync::broadcast::channel(256);
        let docker = Arc::new(aegis_docker::Docker::new(
            settings.data_dir.join("run").join("docker"),
            aegis_docker::Docker::instance_for(&settings.data_dir),
        ));
        Ok(Self {
            inner: Arc::new(Inner {
                store,
                projection,
                supervisor,
                artifact_store: ArtifactStore::new(settings.data_dir.join("artifacts")),
                settings,
                project_locks: Mutex::new(HashMap::new()),
                process_release: Mutex::new(HashMap::new()),
                shutting_down: AtomicBool::new(false),
                resources,
                host,
                history,
                metrics_tx,
                ingress: OnceLock::new(),
                process_ports: Mutex::new(HashMap::new()),
                candidates: Mutex::new(HashSet::new()),
                process_instance: Mutex::new(HashMap::new()),
                standby: Mutex::new(HashMap::new()),
                process_usage: Mutex::new(HashMap::new()),
                domain_lock: tokio::sync::Mutex::new(()),
                secrets,
                backup_lock: tokio::sync::Mutex::new(()),
                docker,
            }),
        })
    }

    /// Attaches the reverse proxy and loads its routes from the recorded
    /// domains and process ports. Call before [`ControlPlane::recover`].
    /// Can be called once; later calls are ignored.
    pub fn set_ingress(&self, ingress: aegis_ingress::Ingress) {
        if self.inner.ingress.set(ingress.clone()).is_err() {
            tracing::warn!("The reverse proxy is already attached");
            return;
        }
        for project in self.inner.projection.get_projects() {
            for domain in &project.domains {
                self.apply_domain_route(&ingress, project.id, domain);
            }
            let ports = project
                .live_processes()
                .iter()
                .filter_map(|id| self.inner.projection.get_process(id))
                .filter_map(|p| p.port)
                .collect();
            ingress.set_upstreams(&project.id.to_string(), ports);
        }
    }

    /// The reverse proxy, when enabled.
    pub fn ingress(&self) -> Option<&aegis_ingress::Ingress> {
        self.inner.ingress.get()
    }

    /// Starts background work: the deployment dispatcher and the recorder for
    /// process lifecycle events. Call before [`ControlPlane::recover`] so the
    /// restored processes' start events are recorded.
    pub fn start(&self) {
        self.spawn_dispatcher();
        self.spawn_process_event_recorder();
        self.spawn_sampler();
        self.spawn_docker_janitor();
    }

    pub fn store(&self) -> &EventStore {
        &self.inner.store
    }

    pub fn projection(&self) -> &ProjectionEngine {
        &self.inner.projection
    }

    pub fn supervisor(&self) -> &ProcessSupervisor {
        &self.inner.supervisor
    }

    pub fn data_dir(&self) -> &PathBuf {
        &self.inner.settings.data_dir
    }

    /// Persists an event; the projection is updated before this returns.
    pub async fn record(&self, event_type: &str, mut payload: Value) -> ControlResult<Event> {
        if let (Some(actor), Some(obj)) = (current_actor(), payload.as_object_mut()) {
            obj.entry("actor").or_insert(Value::String(actor));
        }
        Ok(self.inner.store.append_event(event_type, payload).await?)
    }

    fn project_lock(&self, id: ProjectId) -> Arc<tokio::sync::Mutex<()>> {
        self.inner
            .project_locks
            .lock()
            .unwrap()
            .entry(id)
            .or_default()
            .clone()
    }

    fn switcher(&self, project_id: ProjectId) -> ReleaseSwitcher {
        ReleaseSwitcher::new(self.inner.settings.data_dir.join("projects"), project_id)
    }

    fn project_dir(&self, project_id: ProjectId) -> PathBuf {
        self.inner
            .settings
            .data_dir
            .join("projects")
            .join(project_id.to_string())
    }

    fn process_log_path(&self, project_id: ProjectId, process_id: ProcessId) -> PathBuf {
        self.project_dir(project_id)
            .join("logs")
            .join(format!("{}.log", process_id))
    }

    fn deployment_log_path(&self, project_id: ProjectId, deployment_id: DeploymentId) -> PathBuf {
        self.project_dir(project_id)
            .join("logs")
            .join(format!("deploy-{}.log", deployment_id))
    }

    // ------------------------------------------------------------------
    // Projects
    // ------------------------------------------------------------------

    pub async fn register_project(&self, req: RegisterProject) -> ControlResult<ProjectState> {
        if req.name.trim().is_empty() {
            return Err(ControlError::InvalidArgument(
                "Project name must not be empty".into(),
            ));
        }
        let id = req.project_id.unwrap_or_default();
        let source_dir = match req.source_dir {
            Some(dir) => Some(
                dir.canonicalize()
                    .map_err(|e| {
                        ControlError::InvalidArgument(format!(
                            "Source directory {}: {}",
                            dir.display(),
                            e
                        ))
                    })?
                    .to_string_lossy()
                    .to_string(),
            ),
            None => None,
        };
        if !req.repository_url.is_empty() {
            aegis_builder::validate_repository_url(&req.repository_url)
                .map_err(|e| ControlError::InvalidArgument(e.to_string()))?;
        }
        if !req.branch.is_empty() {
            aegis_builder::validate_git_ref(&req.branch)
                .map_err(|e| ControlError::InvalidArgument(e.to_string()))?;
        }
        let source_subdir = match req.source_subdir.as_deref().map(str::trim) {
            Some(sub) if !sub.is_empty() => {
                aegis_builder::validate_subdir(sub)
                    .map_err(|e| ControlError::InvalidArgument(e.to_string()))?;
                Some(sub.trim_matches('/').to_string())
            }
            _ => None,
        };
        let branch = if req.branch.is_empty() {
            "main".to_string()
        } else {
            req.branch
        };
        self.record(
            "ProjectCreated",
            json!({
                "project_id": id.to_string(),
                "name": req.name,
                "repository_url": req.repository_url,
                "branch": branch,
                "runtime": req.runtime,
                "source_dir": source_dir,
                "source_subdir": source_subdir,
            }),
        )
        .await?;
        self.inner
            .projection
            .get_project(&id)
            .ok_or_else(|| ControlError::Internal(anyhow::anyhow!("Project was not recorded")))
    }

    /// Sets a project's daily auto-deployment time (UTC).
    pub async fn configure_schedule(
        &self,
        project_id: ProjectId,
        hour: u32,
        minute: u32,
        branch: Option<String>,
    ) -> ControlResult<ProjectState> {
        if hour > 23 || minute > 59 {
            return Err(ControlError::InvalidArgument(format!(
                "Invalid schedule time {:02}:{:02}; use hour 0-23 and minute 0-59",
                hour, minute
            )));
        }
        let project = self
            .inner
            .projection
            .get_project(&project_id)
            .ok_or_else(|| ControlError::NotFound(format!("Unknown project {}", project_id)))?;
        self.record(
            "ScheduleConfigured",
            json!({
                "project_id": project_id.to_string(),
                "hour": hour,
                "minute": minute,
                "branch": branch.filter(|b| !b.is_empty()).unwrap_or(project.branch),
            }),
        )
        .await?;
        self.inner
            .projection
            .get_project(&project_id)
            .ok_or_else(|| ControlError::Internal(anyhow::anyhow!("Project vanished")))
    }

    /// Replaces a project's dashboard settings. They take effect on the next
    /// deployment (each release keeps the settings it was built with).
    pub async fn update_project_settings(
        &self,
        project_id: ProjectId,
        settings: aegis_config::ProjectOverrides,
    ) -> ControlResult<ProjectState> {
        let Some(project) = self.inner.projection.get_project(&project_id) else {
            return Err(ControlError::NotFound(format!(
                "Unknown project {}",
                project_id
            )));
        };
        let settings = validate_settings(settings)?;
        // Checked when it changes, so other settings can still be saved if
        // the daemon configuration changed since.
        if let Some(path) = settings
            .env_file
            .as_ref()
            .filter(|p| project.settings.env_file.as_ref() != Some(*p))
        {
            aegis_config::envfile::validate_setting(path, &self.inner.settings.env_file_dirs)
                .map_err(|e| ControlError::InvalidArgument(e.to_string()))?;
        }
        if let Some(strategy) = &settings.strategy {
            self.check_strategy_for(&project, strategy)
                .map_err(ControlError::FailedPrecondition)?;
        }
        if let Some(instances) = settings.instances.filter(|n| *n > 1) {
            self.check_instances_for(&project, instances)
                .map_err(ControlError::FailedPrecondition)?;
        }
        self.record(
            "ProjectSettingsUpdated",
            json!({"project_id": project_id.to_string(), "settings": settings}),
        )
        .await?;
        self.inner
            .projection
            .get_project(&project_id)
            .ok_or_else(|| ControlError::Internal(anyhow::anyhow!("Project vanished")))
    }

    /// Previews the settings a deployment from this source would use:
    /// aegis.toml (if present) plus runtime detection. Git sources are
    /// cloned into a scratch directory that is removed afterwards.
    pub async fn detect_source(
        &self,
        source_dir: Option<PathBuf>,
        repository_url: Option<String>,
        branch: Option<String>,
    ) -> ControlResult<aegis_engine::ResolvedProjectConfig> {
        self.detect_source_for(None, source_dir, repository_url, branch, None)
            .await
    }

    /// [`ControlPlane::detect_source`] for a registered project: clones with
    /// its deploy key and looks inside `subdir` (default: the project's).
    pub async fn detect_source_for(
        &self,
        project: Option<&ProjectState>,
        source_dir: Option<PathBuf>,
        repository_url: Option<String>,
        branch: Option<String>,
        subdir: Option<String>,
    ) -> ControlResult<aegis_engine::ResolvedProjectConfig> {
        let subdir = subdir
            .filter(|s| !s.trim().is_empty())
            .or_else(|| project.and_then(|p| p.source_subdir.clone()));
        if let Some(sub) = &subdir {
            aegis_builder::validate_subdir(sub)
                .map_err(|e| ControlError::InvalidArgument(e.to_string()))?;
        }
        let ssh = project.and_then(|p| self.ssh_auth(&p.id));
        let source = match (source_dir, repository_url) {
            (Some(_), Some(_)) => {
                return Err(ControlError::InvalidArgument(
                    "Give a source_dir or a repository_url, not both".into(),
                ))
            }
            (Some(dir), None) => aegis_builder::SourceSpec::LocalDir(dir),
            (None, Some(url)) => {
                aegis_builder::validate_repository_url(&url)
                    .map_err(|e| ControlError::InvalidArgument(e.to_string()))?;
                let reference = branch.unwrap_or_else(|| "main".into());
                aegis_builder::validate_git_ref(&reference)
                    .map_err(|e| ControlError::InvalidArgument(e.to_string()))?;
                sources::with_deploy_key_url(
                    aegis_builder::SourceSpec::Git { url, reference },
                    ssh.is_some(),
                )
            }
            (None, None) => {
                return Err(ControlError::InvalidArgument(
                    "Give a source_dir or a repository_url".into(),
                ))
            }
        };
        let scratch = self
            .inner
            .settings
            .data_dir
            .join("tmp")
            .join(format!("detect-{}", aegis_types::EventId::new()));
        aegis_builder::detect_source(&source, &scratch, ssh.as_ref(), subdir.as_deref())
            .await
            .map_err(|e| {
                ControlError::FailedPrecondition(format!("Could not read the source: {}", e))
            })
    }

    /// The last `lines` lines of a deployment's build log.
    pub fn deployment_log(&self, deployment: &DeploymentState, lines: usize) -> Vec<String> {
        let path = self.deployment_log_path(deployment.project_id, deployment.id);
        let Ok(content) = std::fs::read(&path) else {
            return Vec::new();
        };
        let text = String::from_utf8_lossy(&content);
        let all: Vec<&str> = text.lines().collect();
        let skip = all.len().saturating_sub(lines);
        all[skip..].iter().map(|l| l.to_string()).collect()
    }

    /// Events recorded for one deployment (stages, outcome), oldest first.
    pub async fn deployment_events(
        &self,
        deployment: &DeploymentState,
    ) -> ControlResult<Vec<Event>> {
        let id = deployment.id.to_string();
        Ok(self
            .list_events(Some(&deployment.project_id), usize::MAX)
            .await?
            .into_iter()
            .filter(|e| {
                serde_json::from_str::<Value>(&e.payload_json)
                    .ok()
                    .and_then(|v| v.get("deployment_id")?.as_str().map(|d| d == id))
                    .unwrap_or(false)
            })
            .collect())
    }

    /// Finds a project by id or (unique) name.
    pub fn resolve_project(&self, key: &str) -> ControlResult<ProjectState> {
        if let Ok(id) = key.parse::<ProjectId>() {
            if let Some(p) = self.inner.projection.get_project(&id) {
                return Ok(p);
            }
        }
        let matches: Vec<_> = self
            .inner
            .projection
            .get_projects()
            .into_iter()
            .filter(|p| p.name == key)
            .collect();
        match matches.len() {
            1 => Ok(matches.into_iter().next().unwrap()),
            0 => Err(ControlError::NotFound(format!("Unknown project '{}'", key))),
            _ => Err(ControlError::InvalidArgument(format!(
                "More than one project is named '{}'; use its id",
                key
            ))),
        }
    }

    /// Finds a process by process id, or a project's current process by
    /// project id or name.
    pub fn resolve_process(&self, key: &str) -> ControlResult<ProcessState> {
        if let Ok(id) = key.parse::<ProcessId>() {
            if let Some(p) = self.process_view(&id) {
                return Ok(p);
            }
        }
        let project = self.resolve_project(key).map_err(|_| {
            ControlError::NotFound(format!("No process or project matches '{}'", key))
        })?;
        project
            .current_process
            .and_then(|id| self.process_view(&id))
            .ok_or_else(|| {
                ControlError::FailedPrecondition(format!(
                    "Project '{}' has no process yet; deploy it first",
                    project.name
                ))
            })
    }

    /// Projection state overlaid with what the supervisor knows right now.
    fn process_view(&self, id: &ProcessId) -> Option<ProcessState> {
        let mut state = self.inner.projection.get_process(id)?;
        if let Some(live) = self.inner.supervisor.get(id) {
            state.pid = live.pid;
            state.status = live.status.as_str().to_string();
            state.restart_count = live.restart_count;
            state.last_exit_code = live.last_exit_code.or(state.last_exit_code);
        } else if matches!(state.status.as_str(), "Running" | "Backoff") {
            // Recorded as running but not supervised (e.g. not recovered).
            state.status = "Stopped".to_string();
            state.pid = None;
        }
        Some(state)
    }

    // ------------------------------------------------------------------
    // Queries
    // ------------------------------------------------------------------

    pub fn list_projects(&self) -> Vec<ProjectView> {
        self.inner
            .projection
            .get_projects()
            .into_iter()
            .map(|project| {
                let process = project
                    .current_process
                    .and_then(|id| self.process_view(&id));
                let usage = process
                    .as_ref()
                    .filter(|p| p.status == "Running")
                    .and_then(|_| self.inner.history.latest(&project.id.to_string()));
                ProjectView {
                    process,
                    usage,
                    instances: self.instance_views(&project),
                    standby: self.standby_view(&project.id),
                    project,
                }
            })
            .collect()
    }

    /// A project's releases, oldest first.
    pub fn list_releases(&self, project_id: &ProjectId) -> Vec<ReleaseState> {
        self.inner.projection.get_project_releases(project_id)
    }

    /// Deployments, oldest first, optionally for one project.
    pub fn list_deployments(&self, project_id: Option<&ProjectId>) -> Vec<DeploymentState> {
        self.inner
            .projection
            .get_deployments()
            .into_iter()
            .filter(|d| project_id.is_none_or(|p| &d.project_id == p))
            .collect()
    }

    pub fn get_deployment(&self, id: &DeploymentId) -> Option<DeploymentState> {
        self.inner.projection.get_deployment(id)
    }

    pub fn list_processes(&self, project_id: Option<&ProjectId>) -> Vec<ProcessState> {
        let mut processes: Vec<_> = self
            .inner
            .projection
            .get_processes()
            .into_iter()
            .filter(|p| project_id.is_none_or(|id| &p.project_id == id))
            .filter_map(|p| self.process_view(&p.id))
            .collect();
        processes.sort_by(|a, b| a.last_start.cmp(&b.last_start));
        processes
    }

    /// The last `n` log lines of a process, from its log file.
    pub fn process_logs(&self, process: &ProcessState, n: usize) -> Vec<aegis_process::LogLine> {
        if self.inner.supervisor.get(&process.id).is_some() {
            return self.inner.supervisor.logs(&process.id, n);
        }
        aegis_process::read_log_tail(&self.process_log_path(process.project_id, process.id), n)
    }

    pub fn subscribe_logs(
        &self,
        process_id: &ProcessId,
    ) -> Option<tokio::sync::broadcast::Receiver<aegis_process::LogLine>> {
        self.inner.supervisor.subscribe_logs(process_id)
    }

    /// The most recent `limit` events, optionally only those about one project.
    pub async fn list_events(
        &self,
        project_id: Option<&ProjectId>,
        limit: usize,
    ) -> ControlResult<Vec<Event>> {
        let events = self.inner.store.get_events().await?;
        let wanted = project_id.map(|p| p.to_string());
        let filtered: Vec<Event> = events
            .into_iter()
            .filter(|e| match &wanted {
                None => true,
                Some(id) => serde_json::from_str::<Value>(&e.payload_json)
                    .ok()
                    .and_then(|v| v.get("project_id")?.as_str().map(|s| s == id))
                    .unwrap_or(false),
            })
            .collect();
        let skip = filtered.len().saturating_sub(limit);
        Ok(filtered.into_iter().skip(skip).collect())
    }
}

/// Trims and validates dashboard settings.
pub(crate) fn validate_settings(
    settings: aegis_config::ProjectOverrides,
) -> ControlResult<aegis_config::ProjectOverrides> {
    let clean = |v: Option<String>| v.map(|s| s.trim().to_string());
    let settings = aegis_config::ProjectOverrides {
        install_command: clean(settings.install_command),
        build_command: clean(settings.build_command),
        test_command: clean(settings.test_command),
        start_command: clean(settings.start_command).filter(|s| !s.is_empty()),
        port: settings.port,
        health_check_url: clean(settings.health_check_url),
        strategy: clean(settings.strategy).filter(|s| !s.is_empty()),
        instances: settings.instances,
        env: settings
            .env
            .into_iter()
            .map(|(k, v)| (k.trim().to_string(), v))
            .collect(),
        env_file: clean(settings.env_file).filter(|s| !s.is_empty()),
    };
    for (name, value) in &settings.env {
        aegis_secrets::validate_name(name)
            .and_then(|_| aegis_secrets::validate_value(value))
            .map_err(ControlError::InvalidArgument)?;
    }
    if settings.port == Some(0) {
        return Err(ControlError::InvalidArgument("Port must be 1-65535".into()));
    }
    if let Some(strategy) = &settings.strategy {
        aegis_config::validate_strategy(strategy).map_err(ControlError::InvalidArgument)?;
    }
    if let Some(instances) = settings.instances {
        aegis_config::validate_instances(instances).map_err(ControlError::InvalidArgument)?;
    }
    if let Some(url) = settings
        .health_check_url
        .as_deref()
        .filter(|u| !u.is_empty())
    {
        if !(url.starts_with("http://") || url.starts_with("tcp://")) {
            return Err(ControlError::InvalidArgument(
                "The health check URL must start with http:// (or tcp:// for a port check)".into(),
            ));
        }
        let target = url.strip_prefix("tcp://").unwrap_or(url);
        aegis_health::parse_http_target(target).map_err(|e| {
            ControlError::InvalidArgument(format!("Invalid health check URL: {}", e))
        })?;
    }
    for command in [
        &settings.install_command,
        &settings.build_command,
        &settings.test_command,
        &settings.start_command,
    ]
    .into_iter()
    .flatten()
    {
        if command.contains('\n') || command.len() > 4096 {
            return Err(ControlError::InvalidArgument(
                "Commands must be a single line of at most 4096 characters".into(),
            ));
        }
    }
    Ok(settings)
}
