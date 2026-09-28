//! The control plane: the single place where Aegis turns commands into work.
//!
//! Every transport (gRPC today, HTTP and webhooks later) calls into
//! [`ControlPlane`]. Commands are recorded as events for the audit trail and
//! then executed; state is read back from the projection, which is updated
//! synchronously as each event is persisted.

mod deploy;
mod process;

use aegis_artifact_store::ArtifactStore;
use aegis_event_store::EventStore;
use aegis_process::ProcessSupervisor;
use aegis_projection::{
    DeploymentState, ProcessState, ProjectState, ProjectionEngine, ReleaseState,
};
use aegis_release::ReleaseSwitcher;
use aegis_types::{DeploymentId, Event, ProcessId, ProjectId};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use deploy::DeployRequest;
pub use process::ProcessAction;

/// Deployment strategies that work with a single instance per app.
pub const SUPPORTED_STRATEGIES: &[&str] = &["GracefulSwitch", "Immediate"];

#[derive(Debug)]
pub enum ControlError {
    NotFound(String),
    InvalidArgument(String),
    FailedPrecondition(String),
    Internal(anyhow::Error),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(m) | Self::InvalidArgument(m) | Self::FailedPrecondition(m) => {
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
}

impl ControlSettings {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            max_retained_versions: 2,
            health_poll_interval: Duration::from_millis(500),
            liveness_window: Duration::from_secs(3),
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
}

/// A project together with the live state of its current process.
#[derive(Debug, Clone)]
pub struct ProjectView {
    pub project: ProjectState,
    pub process: Option<ProcessState>,
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
            }),
        })
    }

    /// Starts background work: the deployment dispatcher and the recorder for
    /// process lifecycle events. Call before [`ControlPlane::recover`] so the
    /// restored processes' start events are recorded.
    pub fn start(&self) {
        self.spawn_dispatcher();
        self.spawn_process_event_recorder();
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
    pub async fn record(&self, event_type: &str, payload: Value) -> ControlResult<Event> {
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
            .map(|project| ProjectView {
                process: project
                    .current_process
                    .and_then(|id| self.process_view(&id)),
                project,
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
