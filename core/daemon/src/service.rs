//! gRPC adapter: translates requests into [`ControlPlane`] calls.

use aegis_api::aegis::aegis_daemon_server::AegisDaemon;
use aegis_api::aegis::{
    self as pb, ControlProcessRequest, DeployRequest, DeployResponse, DeploymentInfo,
    EmitEventRequest, EmitEventResponse, EventResponse, GetDeploymentRequest, GetLogsRequest,
    GetLogsResponse, ListDeploymentsRequest, ListDeploymentsResponse, ListEventsRequest,
    ListEventsResponse, ListProcessesRequest, ListProcessesResponse, ListProjectsRequest,
    ListProjectsResponse, ListReleasesRequest, ListReleasesResponse, ProcessInfo, ProjectInfo,
    RegisterProjectRequest, ReleaseInfo, RollbackRequest, RollbackResponse, StatusRequest,
    StatusResponse, StreamEventsRequest, StreamLogsRequest,
};
use aegis_control::{ControlError, ControlPlane, ProcessAction, ProjectView, RegisterProject};
use aegis_plugins::PluginManager;
use aegis_projection::{DeploymentState, ProcessState, ReleaseState};
use aegis_types::{DeploymentId, Event, ProjectId};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

pub const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");

type ResponseStream<T> = Pin<Box<dyn futures_core::Stream<Item = Result<T, Status>> + Send>>;

pub struct DaemonService {
    pub control: ControlPlane,
    pub plugin_manager: Arc<PluginManager>,
}

fn status_from(e: ControlError) -> Status {
    match e {
        ControlError::NotFound(m) => Status::not_found(m),
        ControlError::InvalidArgument(m) => Status::invalid_argument(m),
        ControlError::FailedPrecondition(m) => Status::failed_precondition(m),
        ControlError::Internal(e) => Status::internal(e.to_string()),
    }
}

fn opt_path(s: &str) -> Option<PathBuf> {
    (!s.is_empty()).then(|| PathBuf::from(s))
}

fn opt_string(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

fn event_response(event: Event) -> EventResponse {
    EventResponse {
        id: event.id.to_string(),
        event_type: event.event_type,
        payload_json: event.payload_json,
        created_at: event.created_at,
    }
}

fn process_info(p: ProcessState) -> ProcessInfo {
    ProcessInfo {
        id: p.id.to_string(),
        project_id: p.project_id.to_string(),
        pid: p.pid,
        status: p.status,
        desired: p.desired,
        restart_count: p.restart_count,
        release_version: p.release_version.unwrap_or_default(),
        command: p.command.unwrap_or_default(),
        last_start: p.last_start.unwrap_or_default(),
        last_exit_code: p.last_exit_code,
    }
}

fn project_info(view: ProjectView) -> ProjectInfo {
    let p = view.project;
    ProjectInfo {
        id: p.id.to_string(),
        name: p.name,
        repository_url: p.repository_url,
        branch: p.branch,
        runtime: p.runtime.unwrap_or_default(),
        source_dir: p.source_dir.unwrap_or_default(),
        current_release: p.current_release.unwrap_or_default(),
        created_at: p.created_at,
        process: view.process.map(process_info),
    }
}

fn release_info(r: ReleaseState) -> ReleaseInfo {
    ReleaseInfo {
        id: r.id.to_string(),
        version: r.version,
        status: r.status,
        commit_sha: r.commit_sha,
        commit_message: r.commit_message,
        checksum: r.checksum.unwrap_or_default(),
        created_at: r.created_at,
    }
}

fn deployment_info(d: DeploymentState) -> DeploymentInfo {
    DeploymentInfo {
        id: d.id.to_string(),
        project_id: d.project_id.to_string(),
        version: d.version.unwrap_or_default(),
        strategy: d.strategy,
        status: d.status,
        stage: d.stage.unwrap_or_default(),
        error: d.error.unwrap_or_default(),
        trigger: d.trigger.unwrap_or_default(),
        created_at: d.created_at,
        completed_at: d.completed_at.unwrap_or_default(),
    }
}

fn log_line(l: aegis_process::LogLine) -> pb::LogLine {
    pb::LogLine {
        timestamp: l.timestamp.to_rfc3339(),
        stream: l.stream.as_str().to_string(),
        line: l.line,
    }
}

impl DaemonService {
    fn project_id(&self, key: &str) -> Result<ProjectId, ControlError> {
        self.control.resolve_project(key).map(|p| p.id)
    }

    /// Like `project_id`, but an empty key means "all projects".
    fn optional_project(&self, key: &str) -> Result<Option<ProjectId>, ControlError> {
        if key.is_empty() {
            return Ok(None);
        }
        self.project_id(key).map(Some)
    }
}

#[tonic::async_trait]
impl AegisDaemon for DaemonService {
    type StreamEventsStream = ResponseStream<EventResponse>;
    type StreamLogsStream = ResponseStream<pb::LogLine>;

    async fn get_status(
        &self,
        _request: Request<StatusRequest>,
    ) -> Result<Response<StatusResponse>, Status> {
        let event_count = self
            .control
            .store()
            .get_event_count()
            .await
            .map_err(|e| Status::internal(format!("Failed to count events: {}", e)))?;
        let projects = self.control.list_projects();
        let running = projects
            .iter()
            .filter(|p| p.process.as_ref().is_some_and(|pr| pr.status == "Running"))
            .count();
        Ok(Response::new(StatusResponse {
            initialized: true,
            version: DAEMON_VERSION.to_string(),
            loaded_plugins: self.plugin_manager.get_loaded_plugins(),
            event_count,
            project_count: projects.len() as u32,
            running_processes: running as u32,
            data_dir: self.control.data_dir().to_string_lossy().to_string(),
        }))
    }

    async fn emit_event(
        &self,
        request: Request<EmitEventRequest>,
    ) -> Result<Response<EmitEventResponse>, Status> {
        let req = request.into_inner();
        tracing::info!(event_type = %req.event_type, "Handling emit_event gRPC request");
        let payload: serde_json::Value = serde_json::from_str(&req.payload_json).map_err(|e| {
            Status::invalid_argument(format!("Failed to parse payload_json as valid JSON: {}", e))
        })?;
        let event = self
            .control
            .record(&req.event_type, payload)
            .await
            .map_err(status_from)?;
        Ok(Response::new(EmitEventResponse {
            success: true,
            event_id: event.id.to_string(),
        }))
    }

    async fn stream_events(
        &self,
        _request: Request<StreamEventsRequest>,
    ) -> Result<Response<Self::StreamEventsStream>, Status> {
        let mut rx = self.control.store().subscribe();
        let (tx, response_rx) = tokio::sync::mpsc::channel(256);
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if tx.send(Ok(event_response(event))).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped = %skipped, "Event stream lagged; some events skipped");
                    }
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(response_rx))))
    }

    async fn register_project(
        &self,
        request: Request<RegisterProjectRequest>,
    ) -> Result<Response<ProjectInfo>, Status> {
        let req = request.into_inner();
        let project_id = match req.project_id.as_str() {
            "" => None,
            id => Some(
                id.parse::<ProjectId>()
                    .map_err(|e| Status::invalid_argument(format!("Invalid project id: {}", e)))?,
            ),
        };
        let project = self
            .control
            .register_project(RegisterProject {
                project_id,
                name: req.name,
                repository_url: req.repository_url,
                branch: req.branch,
                runtime: opt_string(req.runtime),
                source_dir: opt_path(&req.source_dir),
            })
            .await
            .map_err(status_from)?;
        Ok(Response::new(project_info(ProjectView {
            project,
            process: None,
        })))
    }

    async fn deploy(
        &self,
        request: Request<DeployRequest>,
    ) -> Result<Response<DeployResponse>, Status> {
        let req = request.into_inner();
        // An unknown id is allowed: the control plane can register it from
        // the source directory's aegis.toml.
        let project_id = match self.control.resolve_project(&req.project) {
            Ok(p) => p.id,
            Err(e) => req
                .project
                .parse::<ProjectId>()
                .map_err(|_| status_from(e))?,
        };
        let deployment_id = self
            .control
            .queue_deployment(aegis_control::DeployRequest {
                project_id,
                branch: opt_string(req.branch),
                strategy: opt_string(req.strategy),
                source_dir: opt_path(&req.source_dir),
                repository_url: opt_string(req.repository_url),
                commit: opt_string(req.commit),
                version: opt_string(req.version),
                trigger: "cli".to_string(),
            })
            .await
            .map_err(status_from)?;
        let project_name = self
            .control
            .projection()
            .get_project(&project_id)
            .map(|p| p.name)
            .unwrap_or_default();
        Ok(Response::new(DeployResponse {
            deployment_id: deployment_id.to_string(),
            project_id: project_id.to_string(),
            project_name,
        }))
    }

    async fn rollback(
        &self,
        request: Request<RollbackRequest>,
    ) -> Result<Response<RollbackResponse>, Status> {
        let req = request.into_inner();
        let project_id = self.project_id(&req.project).map_err(status_from)?;
        let version = self
            .control
            .rollback(project_id, opt_string(req.version))
            .await
            .map_err(status_from)?;
        Ok(Response::new(RollbackResponse { version }))
    }

    async fn control_process(
        &self,
        request: Request<ControlProcessRequest>,
    ) -> Result<Response<ProcessInfo>, Status> {
        let req = request.into_inner();
        let action = match pb::ProcessAction::try_from(req.action) {
            Ok(pb::ProcessAction::Start) => ProcessAction::Start,
            Ok(pb::ProcessAction::Stop) => ProcessAction::Stop,
            Ok(pb::ProcessAction::Restart) => ProcessAction::Restart,
            _ => return Err(Status::invalid_argument("Unknown process action")),
        };
        let process = self
            .control
            .process_action(&req.target, action)
            .await
            .map_err(status_from)?;
        Ok(Response::new(process_info(process)))
    }

    async fn list_projects(
        &self,
        _request: Request<ListProjectsRequest>,
    ) -> Result<Response<ListProjectsResponse>, Status> {
        Ok(Response::new(ListProjectsResponse {
            projects: self
                .control
                .list_projects()
                .into_iter()
                .map(project_info)
                .collect(),
        }))
    }

    async fn list_releases(
        &self,
        request: Request<ListReleasesRequest>,
    ) -> Result<Response<ListReleasesResponse>, Status> {
        let project_id = self
            .project_id(&request.into_inner().project)
            .map_err(status_from)?;
        Ok(Response::new(ListReleasesResponse {
            releases: self
                .control
                .list_releases(&project_id)
                .into_iter()
                .map(release_info)
                .collect(),
        }))
    }

    async fn list_deployments(
        &self,
        request: Request<ListDeploymentsRequest>,
    ) -> Result<Response<ListDeploymentsResponse>, Status> {
        let project = self
            .optional_project(&request.into_inner().project)
            .map_err(status_from)?;
        Ok(Response::new(ListDeploymentsResponse {
            deployments: self
                .control
                .list_deployments(project.as_ref())
                .into_iter()
                .map(deployment_info)
                .collect(),
        }))
    }

    async fn get_deployment(
        &self,
        request: Request<GetDeploymentRequest>,
    ) -> Result<Response<DeploymentInfo>, Status> {
        let id: DeploymentId = request
            .into_inner()
            .deployment_id
            .parse()
            .map_err(|e| Status::invalid_argument(format!("Invalid deployment id: {}", e)))?;
        self.control
            .get_deployment(&id)
            .map(|d| Response::new(deployment_info(d)))
            .ok_or_else(|| Status::not_found(format!("Unknown deployment {}", id)))
    }

    async fn list_processes(
        &self,
        request: Request<ListProcessesRequest>,
    ) -> Result<Response<ListProcessesResponse>, Status> {
        let project = self
            .optional_project(&request.into_inner().project)
            .map_err(status_from)?;
        Ok(Response::new(ListProcessesResponse {
            processes: self
                .control
                .list_processes(project.as_ref())
                .into_iter()
                .map(process_info)
                .collect(),
        }))
    }

    async fn get_logs(
        &self,
        request: Request<GetLogsRequest>,
    ) -> Result<Response<GetLogsResponse>, Status> {
        let req = request.into_inner();
        let process = self
            .control
            .resolve_process(&req.target)
            .map_err(status_from)?;
        let lines = if req.lines == 0 {
            100
        } else {
            req.lines as usize
        };
        Ok(Response::new(GetLogsResponse {
            process_id: process.id.to_string(),
            lines: self
                .control
                .process_logs(&process, lines)
                .into_iter()
                .map(log_line)
                .collect(),
        }))
    }

    async fn stream_logs(
        &self,
        request: Request<StreamLogsRequest>,
    ) -> Result<Response<Self::StreamLogsStream>, Status> {
        let req = request.into_inner();
        let process = self
            .control
            .resolve_process(&req.target)
            .map_err(status_from)?;
        // Subscribe before reading history so no line falls in between.
        let live = self.control.subscribe_logs(&process.id);
        let history = self.control.process_logs(&process, req.lines as usize);
        let (tx, rx) = tokio::sync::mpsc::channel(256);
        tokio::spawn(async move {
            for line in history {
                if tx.send(Ok(log_line(line))).await.is_err() {
                    return;
                }
            }
            let Some(mut live) = live else { return };
            loop {
                match live.recv().await {
                    Ok(line) => {
                        if tx.send(Ok(log_line(line))).await.is_err() {
                            return;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn list_events(
        &self,
        request: Request<ListEventsRequest>,
    ) -> Result<Response<ListEventsResponse>, Status> {
        let req = request.into_inner();
        let project = self.optional_project(&req.project).map_err(status_from)?;
        let limit = if req.limit == 0 {
            50
        } else {
            req.limit as usize
        };
        let events = self
            .control
            .list_events(project.as_ref(), limit)
            .await
            .map_err(status_from)?;
        Ok(Response::new(ListEventsResponse {
            events: events.into_iter().map(event_response).collect(),
        }))
    }
}
