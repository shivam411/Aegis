//! Jobs: scheduled and one-off commands of an app (see `aegis_control::jobs`).
//! Reading is for every role, running a job for deployers, and creating,
//! changing and deleting jobs for signed-in admins (a job is arbitrary code,
//! so API tokens can only run existing ones).

use crate::dto::ErrorBody;
use crate::{ApiJson, ApiResult, AppState};
use aegis_control::{JobSpec, JobUpdate};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct JobRunDto {
    pub id: String,
    pub job_id: String,
    /// "schedule" or "manual".
    pub trigger: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// running, success, failed, timeout or skipped.
    pub status: String,
    pub exit_code: Option<i32>,
    /// The last 32 KB of the combined output.
    pub output_tail: String,
}

impl From<aegis_control::JobRun> for JobRunDto {
    fn from(r: aegis_control::JobRun) -> Self {
        Self {
            id: r.id,
            job_id: r.job_id,
            trigger: r.trigger,
            started_at: r.started_at,
            finished_at: r.finished_at,
            status: r.status,
            exit_code: r.exit_code,
            output_tail: r.output_tail,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct JobDto {
    pub id: String,
    pub name: String,
    /// Five-field cron expression in UTC, or @hourly, @daily, @weekly, @monthly.
    pub schedule: String,
    pub command: String,
    pub timeout_secs: u64,
    pub enabled: bool,
    pub created_at: String,
    /// The next scheduled start (RFC 3339), while enabled.
    pub next_run: Option<String>,
    pub last_run: Option<JobRunDto>,
}

impl From<aegis_control::Job> for JobDto {
    fn from(j: aegis_control::Job) -> Self {
        Self {
            id: j.id,
            name: j.name,
            schedule: j.schedule,
            command: j.command,
            timeout_secs: j.timeout_secs,
            enabled: j.enabled,
            created_at: j.created_at,
            next_run: j.next_run,
            last_run: j.last_run.map(Into::into),
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateJobRequest {
    /// 1-64 letters, digits, spaces, `.`, `_`, `-`; unique within the app.
    pub name: String,
    /// `*/15 * * * *`, `0 3 * * 1-5`, `@daily`... (UTC).
    pub schedule: String,
    /// Run with `sh -c` in the live release's directory, with the app's
    /// variables and secrets. Output (stdout and stderr) is kept.
    pub command: String,
    /// Killed (with its child processes) after this many seconds (1-86400, default 3600).
    pub timeout_secs: Option<u64>,
    /// Default true.
    pub enabled: Option<bool>,
}

#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct UpdateJobRequest {
    pub name: Option<String>,
    pub schedule: Option<String>,
    pub command: Option<String>,
    pub timeout_secs: Option<u64>,
    pub enabled: Option<bool>,
}

#[utoipa::path(get, path = "/api/v1/projects/{project}/jobs", tag = "jobs",
    params(("project" = String, Path, description = "Project id or name")),
    responses((status = 200, body = [JobDto])))]
pub(crate) async fn list_jobs(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Vec<JobDto>>> {
    let project = st.control.resolve_project(&key)?;
    Ok(Json(
        st.control
            .list_jobs(project.id)
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

#[utoipa::path(post, path = "/api/v1/projects/{project}/jobs", tag = "jobs",
    params(("project" = String, Path, description = "Project id or name")),
    request_body = CreateJobRequest,
    responses((status = 201, body = JobDto), (status = 400, body = ErrorBody),
              (status = 409, description = "The app already has a job with that name", body = ErrorBody)))]
pub(crate) async fn create_job(
    State(st): State<AppState>,
    Path(key): Path<String>,
    ApiJson(req): ApiJson<CreateJobRequest>,
) -> ApiResult<(StatusCode, Json<JobDto>)> {
    let project = st.control.resolve_project(&key)?;
    let job = st
        .control
        .create_job(
            project.id,
            JobSpec {
                name: req.name,
                schedule: req.schedule,
                command: req.command,
                timeout_secs: req.timeout_secs,
                enabled: req.enabled,
            },
        )
        .await?;
    Ok((StatusCode::CREATED, Json(job.into())))
}

#[utoipa::path(put, path = "/api/v1/projects/{project}/jobs/{job}", tag = "jobs",
    params(("project" = String, Path), ("job" = String, Path, description = "Job id")),
    request_body = UpdateJobRequest,
    responses((status = 200, body = JobDto), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub(crate) async fn update_job(
    State(st): State<AppState>,
    Path((key, job)): Path<(String, String)>,
    ApiJson(req): ApiJson<UpdateJobRequest>,
) -> ApiResult<Json<JobDto>> {
    let project = st.control.resolve_project(&key)?;
    let job = st
        .control
        .update_job(
            project.id,
            &job,
            JobUpdate {
                name: req.name,
                schedule: req.schedule,
                command: req.command,
                timeout_secs: req.timeout_secs,
                enabled: req.enabled,
            },
        )
        .await?;
    Ok(Json(job.into()))
}

#[utoipa::path(delete, path = "/api/v1/projects/{project}/jobs/{job}", tag = "jobs",
    params(("project" = String, Path), ("job" = String, Path, description = "Job id")),
    responses((status = 204, description = "Deleted with its run history"), (status = 404, body = ErrorBody)))]
pub(crate) async fn delete_job(
    State(st): State<AppState>,
    Path((key, job)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let project = st.control.resolve_project(&key)?;
    st.control.delete_job(project.id, &job).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/v1/projects/{project}/jobs/{job}/run", tag = "jobs",
    params(("project" = String, Path), ("job" = String, Path, description = "Job id")),
    request_body(content = Object, description = "Empty object `{}`"),
    responses(
        (status = 202, description = "Started; follow it with GET .../runs", body = JobRunDto),
        (status = 409, description = "The job is already running, or the app has no live release", body = ErrorBody)))]
pub(crate) async fn run_job(
    State(st): State<AppState>,
    Path((key, job)): Path<(String, String)>,
) -> ApiResult<(StatusCode, Json<JobRunDto>)> {
    let project = st.control.resolve_project(&key)?;
    let run = st.control.run_job_now(project.id, &job).await?;
    Ok((StatusCode::ACCEPTED, Json(run.into())))
}

#[utoipa::path(get, path = "/api/v1/projects/{project}/jobs/{job}/runs", tag = "jobs",
    params(("project" = String, Path), ("job" = String, Path, description = "Job id")),
    responses((status = 200, description = "Newest first; the last 50 runs are kept", body = [JobRunDto]),
              (status = 404, body = ErrorBody)))]
pub(crate) async fn list_job_runs(
    State(st): State<AppState>,
    Path((key, job)): Path<(String, String)>,
) -> ApiResult<Json<Vec<JobRunDto>>> {
    let project = st.control.resolve_project(&key)?;
    Ok(Json(
        st.control
            .list_job_runs(project.id, &job, 50)
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}
