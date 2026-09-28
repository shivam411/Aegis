//! HTTP/JSON API for the Aegis control plane, with Server-Sent Events for
//! live events and logs. Every handler is a thin adapter over
//! [`aegis_control::ControlPlane`], the same layer the gRPC API uses.
//!
//! The API has no authentication yet (roadmap Phase 2), so it is served on
//! loopback only and guarded against browser-based attacks (see [`guard`]).

pub mod dto;
mod guard;

use aegis_control::{ControlError, ControlPlane, ProcessAction, RegisterProject};
use aegis_types::{DeploymentId, ProjectId};
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Path, Query, Request, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use dto::*;
use futures_util::stream::{self, BoxStream, StreamExt as _};
use serde::Deserialize;
use std::convert::Infallible;
use std::future::Future;
use std::path::PathBuf;
use tokio::sync::watch;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use utoipa::{IntoParams, OpenApi};

/// Shared state for all handlers.
#[derive(Clone)]
pub struct AppState {
    pub control: ControlPlane,
    pub version: String,
    /// Flips to `true` on daemon shutdown so long-lived streams end.
    pub shutdown: watch::Receiver<bool>,
}

// ----------------------------------------------------------------------
// Errors
// ----------------------------------------------------------------------

/// An error rendered as `{"error": {"code": ..., "message": ...}}`.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_argument", message)
    }
}

impl From<ControlError> for ApiError {
    fn from(e: ControlError) -> Self {
        match e {
            ControlError::NotFound(m) => Self::new(StatusCode::NOT_FOUND, "not_found", m),
            ControlError::InvalidArgument(m) => Self::bad_request(m),
            ControlError::FailedPrecondition(m) => {
                Self::new(StatusCode::CONFLICT, "failed_precondition", m)
            }
            ControlError::Internal(e) => {
                tracing::error!(error = %e, "Internal error in HTTP handler");
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string())
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: ErrorDetail {
                    code: self.code.to_string(),
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

/// `Json<T>` whose rejections use the API error format.
pub struct ApiJson<T>(pub T);

#[axum::async_trait]
impl<S, T> FromRequest<S> for ApiJson<T>
where
    Json<T>: FromRequest<S, Rejection = JsonRejection>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(ApiError::bad_request(rejection.body_text())),
        }
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

// ----------------------------------------------------------------------
// Query parameters
// ----------------------------------------------------------------------

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ProjectFilter {
    /// Project id or name; all projects when omitted.
    pub project: Option<String>,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EventsQuery {
    /// Project id or name; all projects when omitted.
    pub project: Option<String>,
    /// Most recent N events (default 50, max 1000).
    pub limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LogsQuery {
    /// Number of lines (default 100, max 5000).
    pub lines: Option<usize>,
}

// ----------------------------------------------------------------------
// Handlers
// ----------------------------------------------------------------------

#[utoipa::path(get, path = "/api/v1/status", tag = "system",
    responses((status = 200, body = StatusDto)))]
async fn status(State(st): State<AppState>) -> ApiResult<Json<StatusDto>> {
    let event_count = st
        .control
        .store()
        .get_event_count()
        .await
        .map_err(|e| ApiError::from(ControlError::Internal(e)))?;
    let projects = st.control.list_projects();
    Ok(Json(StatusDto {
        version: st.version.clone(),
        project_count: projects.len(),
        running_processes: projects
            .iter()
            .filter(|p| p.process.as_ref().is_some_and(|pr| pr.status == "Running"))
            .count(),
        event_count,
        data_dir: st.control.data_dir().to_string_lossy().to_string(),
    }))
}

#[utoipa::path(get, path = "/api/v1/projects", tag = "projects",
    responses((status = 200, body = [ProjectDto])))]
async fn list_projects(State(st): State<AppState>) -> Json<Vec<ProjectDto>> {
    Json(
        st.control
            .list_projects()
            .into_iter()
            .map(Into::into)
            .collect(),
    )
}

#[utoipa::path(post, path = "/api/v1/projects", tag = "projects",
    request_body = CreateProjectRequest,
    responses((status = 201, body = ProjectDto), (status = 400, body = ErrorBody)))]
async fn create_project(
    State(st): State<AppState>,
    ApiJson(req): ApiJson<CreateProjectRequest>,
) -> ApiResult<(StatusCode, Json<ProjectDto>)> {
    let project_id = match non_empty(req.project_id) {
        Some(id) => Some(
            id.parse::<ProjectId>()
                .map_err(|e| ApiError::bad_request(format!("Invalid project_id: {}", e)))?,
        ),
        None => None,
    };
    let project = st
        .control
        .register_project(RegisterProject {
            project_id,
            name: req.name,
            repository_url: req.repository_url.unwrap_or_default(),
            branch: req.branch.unwrap_or_default(),
            runtime: non_empty(req.runtime),
            source_dir: non_empty(req.source_dir).map(PathBuf::from),
        })
        .await?;
    Ok((StatusCode::CREATED, Json(project_view(&st, &project.id)?)))
}

fn project_view(st: &AppState, id: &ProjectId) -> ApiResult<ProjectDto> {
    st.control
        .list_projects()
        .into_iter()
        .find(|v| &v.project.id == id)
        .map(Into::into)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "Unknown project"))
}

#[utoipa::path(get, path = "/api/v1/projects/{project}", tag = "projects",
    params(("project" = String, Path, description = "Project id or name")),
    responses((status = 200, body = ProjectDto), (status = 404, body = ErrorBody)))]
async fn get_project(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<ProjectDto>> {
    let project = st.control.resolve_project(&key)?;
    Ok(Json(project_view(&st, &project.id)?))
}

#[utoipa::path(post, path = "/api/v1/projects/{project}/deploy", tag = "deployments",
    params(("project" = String, Path, description = "Project id or name")),
    request_body = DeployRequest,
    responses(
        (status = 202, description = "Queued; follow it via /deployments/{id} or the event stream", body = DeployAccepted),
        (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)))]
async fn deploy(
    State(st): State<AppState>,
    Path(key): Path<String>,
    ApiJson(req): ApiJson<DeployRequest>,
) -> ApiResult<(StatusCode, Json<DeployAccepted>)> {
    let project = st.control.resolve_project(&key)?;
    let deployment_id = st
        .control
        .queue_deployment(aegis_control::DeployRequest {
            project_id: project.id,
            branch: non_empty(req.branch),
            strategy: non_empty(req.strategy),
            source_dir: non_empty(req.source_dir).map(PathBuf::from),
            repository_url: non_empty(req.repository_url),
            commit: non_empty(req.commit),
            version: non_empty(req.version),
            trigger: "http".to_string(),
        })
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(DeployAccepted {
            deployment_id: deployment_id.to_string(),
            project_id: project.id.to_string(),
            project_name: project.name,
        }),
    ))
}

#[utoipa::path(post, path = "/api/v1/projects/{project}/rollback", tag = "deployments",
    params(("project" = String, Path, description = "Project id or name")),
    request_body = RollbackRequest,
    responses(
        (status = 200, description = "The earlier release is live", body = RollbackResult),
        (status = 409, description = "Nothing to roll back to, or the release didn't become healthy", body = ErrorBody)))]
async fn rollback(
    State(st): State<AppState>,
    Path(key): Path<String>,
    ApiJson(req): ApiJson<RollbackRequest>,
) -> ApiResult<Json<RollbackResult>> {
    let project = st.control.resolve_project(&key)?;
    let version = st
        .control
        .rollback(project.id, non_empty(req.version))
        .await?;
    Ok(Json(RollbackResult { version }))
}

#[utoipa::path(put, path = "/api/v1/projects/{project}/schedule", tag = "projects",
    params(("project" = String, Path, description = "Project id or name")),
    request_body = ScheduleRequest,
    responses((status = 200, body = ProjectDto), (status = 400, body = ErrorBody)))]
async fn set_schedule(
    State(st): State<AppState>,
    Path(key): Path<String>,
    ApiJson(req): ApiJson<ScheduleRequest>,
) -> ApiResult<Json<ProjectDto>> {
    let project = st.control.resolve_project(&key)?;
    st.control
        .configure_schedule(project.id, req.hour, req.minute, req.branch)
        .await?;
    Ok(Json(project_view(&st, &project.id)?))
}

#[utoipa::path(get, path = "/api/v1/projects/{project}/releases", tag = "deployments",
    params(("project" = String, Path, description = "Project id or name")),
    responses((status = 200, description = "Oldest first", body = [ReleaseDto])))]
async fn list_releases(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Vec<ReleaseDto>>> {
    let project = st.control.resolve_project(&key)?;
    Ok(Json(
        st.control
            .list_releases(&project.id)
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

#[utoipa::path(get, path = "/api/v1/deployments", tag = "deployments",
    params(ProjectFilter),
    responses((status = 200, description = "Oldest first", body = [DeploymentDto])))]
async fn list_deployments(
    State(st): State<AppState>,
    Query(q): Query<ProjectFilter>,
) -> ApiResult<Json<Vec<DeploymentDto>>> {
    let project = match non_empty(q.project) {
        Some(key) => Some(st.control.resolve_project(&key)?.id),
        None => None,
    };
    Ok(Json(
        st.control
            .list_deployments(project.as_ref())
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

#[utoipa::path(get, path = "/api/v1/deployments/{id}", tag = "deployments",
    params(("id" = String, Path, description = "Deployment id")),
    responses((status = 200, body = DeploymentDto), (status = 404, body = ErrorBody)))]
async fn get_deployment(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<DeploymentDto>> {
    let id: DeploymentId = id
        .parse()
        .map_err(|e| ApiError::bad_request(format!("Invalid deployment id: {}", e)))?;
    st.control
        .get_deployment(&id)
        .map(|d| Json(d.into()))
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("Unknown deployment {}", id),
            )
        })
}

#[utoipa::path(get, path = "/api/v1/processes", tag = "processes",
    params(ProjectFilter),
    responses((status = 200, body = [ProcessDto])))]
async fn list_processes(
    State(st): State<AppState>,
    Query(q): Query<ProjectFilter>,
) -> ApiResult<Json<Vec<ProcessDto>>> {
    let project = match non_empty(q.project) {
        Some(key) => Some(st.control.resolve_project(&key)?.id),
        None => None,
    };
    Ok(Json(
        st.control
            .list_processes(project.as_ref())
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

#[utoipa::path(post, path = "/api/v1/processes/{target}/{action}", tag = "processes",
    params(
        ("target" = String, Path, description = "Process id, or a project id/name for its current process"),
        ("action" = String, Path, description = "start, stop or restart")),
    request_body(content = Object, description = "Empty object `{}`"),
    responses(
        (status = 200, body = ProcessDto),
        (status = 404, body = ErrorBody),
        (status = 409, description = "E.g. already running, or not the live release", body = ErrorBody)))]
async fn process_action(
    State(st): State<AppState>,
    Path((target, action)): Path<(String, String)>,
) -> ApiResult<Json<ProcessDto>> {
    let action = match action.as_str() {
        "start" => ProcessAction::Start,
        "stop" => ProcessAction::Stop,
        "restart" => ProcessAction::Restart,
        other => {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("Unknown action '{}'; use start, stop or restart", other),
            ))
        }
    };
    Ok(Json(
        st.control.process_action(&target, action).await?.into(),
    ))
}

#[utoipa::path(get, path = "/api/v1/processes/{target}/logs", tag = "processes",
    params(
        ("target" = String, Path, description = "Process id, or a project id/name"),
        LogsQuery),
    responses((status = 200, body = LogsDto), (status = 404, body = ErrorBody)))]
async fn get_logs(
    State(st): State<AppState>,
    Path(target): Path<String>,
    Query(q): Query<LogsQuery>,
) -> ApiResult<Json<LogsDto>> {
    let process = st.control.resolve_process(&target)?;
    let lines = q.lines.unwrap_or(100).min(5000);
    Ok(Json(LogsDto {
        process_id: process.id.to_string(),
        lines: st
            .control
            .process_logs(&process, lines)
            .into_iter()
            .map(Into::into)
            .collect(),
    }))
}

#[utoipa::path(get, path = "/api/v1/events", tag = "events",
    params(EventsQuery),
    responses((status = 200, description = "Oldest first", body = [EventDto])))]
async fn list_events(
    State(st): State<AppState>,
    Query(q): Query<EventsQuery>,
) -> ApiResult<Json<Vec<EventDto>>> {
    let project = match non_empty(q.project) {
        Some(key) => Some(st.control.resolve_project(&key)?.id),
        None => None,
    };
    let limit = q.limit.unwrap_or(50).clamp(1, 1000);
    let events = st.control.list_events(project.as_ref(), limit).await?;
    Ok(Json(events.into_iter().map(Into::into).collect()))
}

// ----------------------------------------------------------------------
// Server-Sent Events
// ----------------------------------------------------------------------

type SseStream = BoxStream<'static, Result<SseEvent, Infallible>>;

/// Ends `stream` when the daemon starts shutting down, so graceful shutdown
/// isn't held open by long-lived connections.
fn until_shutdown(st: &AppState, stream: SseStream) -> Sse<SseStream> {
    let mut shutdown = st.shutdown.clone();
    let stop = async move {
        let _ = shutdown.wait_for(|stopping| *stopping).await;
    };
    Sse::new(stream.take_until(stop).boxed()).keep_alive(KeepAlive::default())
}

fn json_event(name: &str, id: Option<String>, data: &impl serde::Serialize) -> SseEvent {
    let event = SseEvent::default()
        .event(name)
        .data(serde_json::to_string(data).unwrap_or_default());
    match id {
        Some(id) => event.id(id),
        None => event,
    }
}

#[utoipa::path(get, path = "/api/v1/events/stream", tag = "events",
    params(ProjectFilter),
    responses((status = 200, content_type = "text/event-stream",
        description = "One SSE message per new event: `event:` is the event type, `data:` an EventDto. A `lagged` message means some events were dropped for this client.")))]
async fn stream_events(
    State(st): State<AppState>,
    Query(q): Query<ProjectFilter>,
) -> ApiResult<Sse<SseStream>> {
    let project = match non_empty(q.project) {
        Some(key) => Some(st.control.resolve_project(&key)?.id.to_string()),
        None => None,
    };
    let rx = st.control.store().subscribe();
    let events = BroadcastStream::new(rx)
        .filter_map(move |item| {
            let project = project.clone();
            async move {
                match item {
                    Ok(event) => {
                        let dto = EventDto::from(event);
                        let wanted = project.as_ref().is_none_or(|p| {
                            dto.payload.get("project_id").and_then(|v| v.as_str())
                                == Some(p.as_str())
                        });
                        wanted.then(|| Ok(json_event(&dto.event_type, Some(dto.id.clone()), &dto)))
                    }
                    Err(BroadcastStreamRecvError::Lagged(n)) => {
                        Some(Ok(SseEvent::default().event("lagged").data(n.to_string())))
                    }
                }
            }
        })
        .boxed();
    Ok(until_shutdown(&st, events))
}

#[utoipa::path(get, path = "/api/v1/processes/{target}/logs/stream", tag = "processes",
    params(
        ("target" = String, Path, description = "Process id, or a project id/name"),
        LogsQuery),
    responses((status = 200, content_type = "text/event-stream",
        description = "`log` messages (LogLineDto): first the requested history, then new lines as they are written (including after a later start). An `end` message follows the history when the daemon isn't supervising the process, so no new lines can arrive.")))]
async fn stream_logs(
    State(st): State<AppState>,
    Path(target): Path<String>,
    Query(q): Query<LogsQuery>,
) -> ApiResult<Sse<SseStream>> {
    let process = st.control.resolve_process(&target)?;
    // Subscribe before reading history so no line falls in between.
    let live = st.control.subscribe_logs(&process.id);
    let history: Vec<LogLineDto> = st
        .control
        .process_logs(&process, q.lines.unwrap_or(100).min(5000))
        .into_iter()
        .map(Into::into)
        .collect();
    let history = stream::iter(
        history
            .into_iter()
            .map(|line| Ok(json_event("log", None, &line))),
    );
    let tail: SseStream = match live {
        Some(rx) => BroadcastStream::new(rx)
            .filter_map(|item| async move {
                item.ok()
                    .map(|line| Ok(json_event("log", None, &LogLineDto::from(line))))
            })
            .boxed(),
        None => stream::once(async { Ok(SseEvent::default().event("end").data("")) }).boxed(),
    };
    Ok(until_shutdown(&st, history.chain(tail).boxed()))
}

// ----------------------------------------------------------------------
// OpenAPI, router, server
// ----------------------------------------------------------------------

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Aegis HTTP API",
        version = "1",
        description = "Control Aegis deployments over HTTP. Loopback-only and unauthenticated until roadmap Phase 2; requests that change state must send `Content-Type: application/json`."
    ),
    paths(
        status, list_projects, create_project, get_project, deploy, rollback, set_schedule,
        list_releases, list_deployments, get_deployment, list_processes, process_action,
        get_logs, stream_logs, list_events, stream_events
    ),
    components(schemas(
        StatusDto, ProjectDto, ScheduleDto, ProcessDto, ReleaseDto, DeploymentDto, EventDto,
        LogLineDto, LogsDto, CreateProjectRequest, DeployRequest, DeployAccepted,
        RollbackRequest, RollbackResult, ScheduleRequest, ErrorBody, ErrorDetail
    )),
    tags(
        (name = "system"), (name = "projects"), (name = "deployments"),
        (name = "processes"), (name = "events")
    )
)]
pub struct ApiDoc;

async fn openapi() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

async fn not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "not_found", "No such endpoint")
}

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/status", get(status))
        .route("/projects", get(list_projects).post(create_project))
        .route("/projects/:project", get(get_project))
        .route("/projects/:project/deploy", post(deploy))
        .route("/projects/:project/rollback", post(rollback))
        .route(
            "/projects/:project/schedule",
            axum::routing::put(set_schedule),
        )
        .route("/projects/:project/releases", get(list_releases))
        .route("/deployments", get(list_deployments))
        .route("/deployments/:id", get(get_deployment))
        .route("/processes", get(list_processes))
        .route("/processes/:target/logs", get(get_logs))
        .route("/processes/:target/logs/stream", get(stream_logs))
        .route("/processes/:target/:action", post(process_action))
        .route("/events", get(list_events))
        .route("/events/stream", get(stream_events))
        .route("/openapi.json", get(openapi))
        .fallback(not_found);
    Router::new()
        .nest("/api/v1", api)
        .fallback(not_found)
        .layer(axum::middleware::from_fn(guard::guard))
        .with_state(state)
}

/// Serves the API until `shutdown` resolves.
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: AppState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await
}
