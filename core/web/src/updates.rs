//! "New changes available": how far an app's (or a group's) live releases
//! are behind their source. Reading is for every role; "Check now" (which
//! contacts the git server) is for deployers.

use crate::dto::ErrorBody;
use crate::{ApiResult, AppState};
use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CommitDto {
    pub sha: String,
    pub author: String,
    /// ISO 8601.
    pub date: String,
    pub message: String,
}

/// How an app's live release compares with its source.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpdatesDto {
    pub project_id: String,
    pub name: String,
    pub checked_at: String,
    /// "git" (a repository branch) or "folder" (a server folder's git HEAD).
    pub kind: String,
    pub branch: Option<String>,
    /// The branch head, or the folder's HEAD.
    pub head: Option<String>,
    /// The commit of the live release.
    pub live_commit: Option<String>,
    /// The app has a live release.
    pub deployed: bool,
    /// Commits after the live one (`null`: unknown).
    pub behind: Option<u32>,
    /// Newest first, at most 50.
    pub commits: Vec<CommitDto>,
    /// Files changed since the live commit (only those matching the app's
    /// watch paths when it is in a group and has some; at most 500).
    pub changed_files: Vec<String>,
    /// Some change concerns this app, or it was never deployed.
    pub relevant: bool,
    /// Folder apps: files with uncommitted changes.
    pub dirty: Option<u32>,
    pub up_to_date: bool,
    /// Why the check couldn't be completed (it never blocks anything).
    pub error: Option<String>,
}

/// A short form for lists (the Overview).
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpdateSummaryDto {
    pub checked_at: String,
    pub behind: Option<u32>,
    pub relevant: bool,
    pub up_to_date: bool,
    pub dirty: Option<u32>,
    pub head: Option<String>,
    pub error: Option<String>,
}

impl From<aegis_control::UpdateStatus> for UpdateSummaryDto {
    fn from(s: aegis_control::UpdateStatus) -> Self {
        Self {
            checked_at: s.checked_at,
            behind: s.behind,
            relevant: s.relevant,
            up_to_date: s.up_to_date,
            dirty: s.dirty,
            head: s.head,
            error: s.error,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct GroupUpdatesDto {
    pub group_id: String,
    pub checked_at: String,
    pub head: Option<String>,
    /// Apps with changes that concern them (or never deployed).
    pub changed_apps: usize,
    /// In deploy order.
    pub members: Vec<UpdatesDto>,
    pub error: Option<String>,
}

fn dto(st: &AppState, id: &aegis_types::ProjectId, s: aegis_control::UpdateStatus) -> UpdatesDto {
    UpdatesDto {
        project_id: id.to_string(),
        name: st
            .control
            .projection()
            .get_project(id)
            .map(|p| p.name)
            .unwrap_or_default(),
        checked_at: s.checked_at,
        kind: s.kind.to_string(),
        branch: s.branch,
        head: s.head,
        live_commit: s.live_commit,
        deployed: s.deployed,
        behind: s.behind,
        commits: s
            .commits
            .into_iter()
            .map(|c| CommitDto {
                sha: c.sha,
                author: c.author,
                date: c.date,
                message: c.message,
            })
            .collect(),
        changed_files: s.changed_files,
        relevant: s.relevant,
        dirty: s.dirty,
        up_to_date: s.up_to_date,
        error: s.error,
    }
}

fn group_dto(st: &AppState, group_id: String, u: aegis_control::GroupUpdates) -> GroupUpdatesDto {
    GroupUpdatesDto {
        group_id,
        checked_at: u.checked_at,
        head: u.head,
        changed_apps: u.changed_apps,
        members: u
            .members
            .into_iter()
            .map(|(id, s)| dto(st, &id, s))
            .collect(),
        error: u.error,
    }
}

#[utoipa::path(get, path = "/api/v1/projects/{project}/updates", tag = "projects",
    params(("project" = String, Path, description = "Project id or name")),
    responses((status = 200, description = "The latest check (one is run if there is none yet)", body = UpdatesDto),
              (status = 404, body = ErrorBody)))]
pub(crate) async fn project_updates(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<UpdatesDto>> {
    let project = st.control.resolve_project(&key)?;
    let status = st.control.updates(&project.id).await?;
    Ok(Json(dto(&st, &project.id, status)))
}

#[utoipa::path(post, path = "/api/v1/projects/{project}/updates/check", tag = "projects",
    params(("project" = String, Path, description = "Project id or name")),
    request_body(content = Object, description = "Empty object `{}`"),
    responses((status = 200, description = "Checked now (a repository is fetched at most every 3 s; apps of one repository share the fetch)", body = UpdatesDto)))]
pub(crate) async fn check_project_updates(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<UpdatesDto>> {
    let project = st.control.resolve_project(&key)?;
    let status = st.control.check_updates(&project.id).await?;
    Ok(Json(dto(&st, &project.id, status)))
}

#[utoipa::path(get, path = "/api/v1/groups/{group}/updates", tag = "groups",
    params(("group" = String, Path, description = "Group id or name")),
    responses((status = 200, description = "Every app of the group compared with the group's branch", body = GroupUpdatesDto)))]
pub(crate) async fn group_updates(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<GroupUpdatesDto>> {
    let group = st.control.resolve_group(&key)?;
    let updates = st.control.group_updates(&group.id).await?;
    Ok(Json(group_dto(&st, group.id, updates)))
}

#[utoipa::path(post, path = "/api/v1/groups/{group}/updates/check", tag = "groups",
    params(("group" = String, Path, description = "Group id or name")),
    request_body(content = Object, description = "Empty object `{}`"),
    responses((status = 200, description = "Checked now (one fetch for the whole group)", body = GroupUpdatesDto)))]
pub(crate) async fn check_group_updates(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<GroupUpdatesDto>> {
    let group = st.control.resolve_group(&key)?;
    let updates = st.control.check_group_updates(&group.id).await?;
    Ok(Json(group_dto(&st, group.id, updates)))
}
