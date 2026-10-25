//! App groups over HTTP: apps deployed from one repository and branch,
//! deployed together at one commit (see `aegis_control::groups`), with one
//! GitHub webhook per group. Reading is for every role; deploying and
//! rolling back a group is for deployers; everything else is for admins,
//! and the group webhook secret for signed-in admins only.

use crate::auth::{header_str, webhook_result, MAX_REMEMBERED_DELIVERIES};
use crate::dto::{ErrorBody, WebhookDeliveryDto, WebhookResult, WebhookSecretDto};
use crate::{non_empty, ApiError, ApiJson, ApiResult, AppState};
use aegis_control::{with_actor, ControlError, GroupSpec, MemberSpec};
use aegis_projection::{GroupDeploymentState, GroupState};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use utoipa::ToSchema;

/// The webhook secret's key in the secrets table (apps use their id).
pub(crate) fn webhook_key(group_id: &str) -> String {
    format!("group:{}", group_id)
}

/// A member app of a group.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct GroupMemberDto {
    pub project_id: String,
    pub name: String,
    /// Repository paths (globs) that concern this app; empty: all of them.
    pub watch_paths: Vec<String>,
    /// 1-based place in the deploy order.
    pub position: usize,
    pub source_subdir: Option<String>,
    pub current_release: Option<String>,
    /// The commit its live release was built from.
    pub commit: Option<String>,
    /// Its process status (Running, Stopped, ...), when it has one.
    pub status: Option<String>,
}

/// One member's part in a group deployment.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct GroupMemberRunDto {
    pub project_id: String,
    pub name: String,
    /// pending, deploying, deployed, skipped, failed or rolled_back.
    pub status: String,
    pub deployment_id: Option<String>,
    pub previous_version: Option<String>,
    pub version: Option<String>,
    pub reason: Option<String>,
}

/// A deployment of a whole group at one commit.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct GroupDeploymentDto {
    pub id: String,
    pub group_id: String,
    pub commit: Option<String>,
    pub only_changed: bool,
    pub trigger: Option<String>,
    /// Running, Success, Failed or RolledBack.
    pub status: String,
    pub error: Option<String>,
    pub members: Vec<GroupMemberRunDto>,
    pub created_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct GroupDto {
    pub id: String,
    pub name: String,
    pub repository_url: String,
    pub branch: String,
    /// When a member fails, the members already switched in that group
    /// deployment go back to their previous release.
    pub rollback_on_failure: bool,
    /// In deploy order.
    pub members: Vec<GroupMemberDto>,
    pub created_at: String,
    pub last_deployment: Option<GroupDeploymentDto>,
    /// A webhook secret exists for the group.
    pub webhook_configured: bool,
}

#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct MemberRequest {
    /// Project id or name.
    pub project: String,
    /// Globs such as `frontend_v2/**`; empty or omitted: the whole repository.
    pub watch_paths: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct GroupRequest {
    pub name: String,
    pub repository_url: String,
    /// Default: main.
    pub branch: Option<String>,
    /// Default: true.
    pub rollback_on_failure: Option<bool>,
    /// Members in deploy order (create only; use the members endpoints later).
    pub members: Vec<MemberRequest>,
}

#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct MembersRequest {
    /// The full member list, in deploy order.
    pub members: Vec<MemberRequest>,
}

#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AddMemberRequest {
    /// Project id or name.
    pub project: String,
    pub watch_paths: Vec<String>,
    /// 1-based place in the deploy order; default: last.
    pub position: Option<usize>,
}

#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct UpdateMemberRequest {
    /// New watch paths (omitted: unchanged).
    pub watch_paths: Option<Vec<String>>,
    /// New 1-based place in the deploy order (omitted: unchanged).
    pub position: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct GroupDeployRequest {
    /// Commit to deploy every member at (default: the branch's latest).
    pub commit: Option<String>,
    /// Deploy only members whose watch paths match files changed since
    /// their live commit (members not deployed yet always deploy). Default true.
    pub only_changed: bool,
}

impl Default for GroupDeployRequest {
    fn default() -> Self {
        Self {
            commit: None,
            only_changed: true,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct GroupDeployAccepted {
    pub group_deployment_id: String,
    pub group_id: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct MemberRollbackDto {
    pub project_id: String,
    pub name: String,
    /// The release now live; `null` when the app was stopped (it had no
    /// release before) or the rollback failed.
    pub version: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct GroupRollbackDto {
    pub members: Vec<MemberRollbackDto>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct GroupWebhookStatusDto {
    pub configured: bool,
    /// Payload URL path; prefix it with the API's public base URL.
    pub path: String,
    pub content_type: String,
    pub events: Vec<String>,
    pub branch: String,
    pub repository_url: String,
    /// Most recent signed deliveries, newest first.
    pub recent: Vec<WebhookDeliveryDto>,
}

fn run_dto(st: &AppState, run: GroupDeploymentState) -> GroupDeploymentDto {
    let name = |id: &aegis_types::ProjectId| {
        st.control
            .projection()
            .get_project(id)
            .map(|p| p.name)
            .unwrap_or_default()
    };
    GroupDeploymentDto {
        members: run
            .members
            .into_iter()
            .map(|m| GroupMemberRunDto {
                name: name(&m.project_id),
                project_id: m.project_id.to_string(),
                status: m.status,
                deployment_id: m.deployment_id.map(|d| d.to_string()),
                previous_version: m.previous_version,
                version: m.version,
                reason: m.reason,
            })
            .collect(),
        id: run.id,
        group_id: run.group_id,
        commit: run.commit,
        only_changed: run.only_changed,
        trigger: run.trigger,
        status: run.status,
        error: run.error,
        created_at: run.created_at,
        completed_at: run.completed_at,
    }
}

pub(crate) async fn group_dto(st: &AppState, group: GroupState) -> ApiResult<GroupDto> {
    let views = st.control.list_projects();
    let members = group
        .members
        .iter()
        .enumerate()
        .filter_map(|(i, m)| {
            let view = views.iter().find(|v| v.project.id == m.project_id)?;
            Some(GroupMemberDto {
                project_id: m.project_id.to_string(),
                name: view.project.name.clone(),
                watch_paths: m.watch_paths.clone(),
                position: i + 1,
                source_subdir: view.project.source_subdir.clone(),
                current_release: view.project.current_release.clone(),
                commit: view.commit.clone(),
                status: view.process.as_ref().map(|p| p.status.clone()),
            })
        })
        .collect();
    let webhook_configured = st
        .auth
        .webhook_secret(&webhook_key(&group.id))
        .await
        .map_err(ApiError::internal)?
        .is_some();
    Ok(GroupDto {
        last_deployment: group
            .last_deployment
            .as_ref()
            .and_then(|id| st.control.get_group_deployment(id))
            .map(|r| run_dto(st, r)),
        id: group.id,
        name: group.name,
        repository_url: group.repository_url,
        branch: group.branch,
        rollback_on_failure: group.rollback_on_failure,
        members,
        created_at: group.created_at,
        webhook_configured,
    })
}

fn member_specs(st: &AppState, members: Vec<MemberRequest>) -> ApiResult<Vec<MemberSpec>> {
    members
        .into_iter()
        .map(|m| {
            Ok(MemberSpec {
                project_id: st.control.resolve_project(m.project.trim())?.id,
                watch_paths: m.watch_paths,
            })
        })
        .collect()
}

fn current_specs(group: &GroupState) -> Vec<MemberSpec> {
    group
        .members
        .iter()
        .map(|m| MemberSpec {
            project_id: m.project_id,
            watch_paths: m.watch_paths.clone(),
        })
        .collect()
}

#[utoipa::path(get, path = "/api/v1/groups", tag = "groups",
    responses((status = 200, body = [GroupDto])))]
pub(crate) async fn list_groups(
    State(st): State<AppState>,
    axum::Extension(vis): axum::Extension<aegis_auth::Visibility>,
) -> ApiResult<Json<Vec<GroupDto>>> {
    let mut out = Vec::new();
    for group in st.control.list_groups() {
        if crate::access::group_hidden(&vis, &group) {
            continue;
        }
        out.push(group_dto(&st, group).await?);
    }
    Ok(Json(out))
}

#[utoipa::path(post, path = "/api/v1/groups", tag = "groups",
    request_body = GroupRequest,
    responses((status = 201, body = GroupDto), (status = 400, body = ErrorBody)))]
pub(crate) async fn create_group(
    State(st): State<AppState>,
    ApiJson(req): ApiJson<GroupRequest>,
) -> ApiResult<(StatusCode, Json<GroupDto>)> {
    let members = member_specs(&st, req.members)?;
    let group = st
        .control
        .create_group(
            GroupSpec {
                name: req.name,
                repository_url: req.repository_url,
                branch: req.branch.unwrap_or_default(),
                rollback_on_failure: req.rollback_on_failure,
            },
            members,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(group_dto(&st, group).await?)))
}

#[utoipa::path(get, path = "/api/v1/groups/{group}", tag = "groups",
    params(("group" = String, Path, description = "Group id or name")),
    responses((status = 200, body = GroupDto), (status = 404, body = ErrorBody)))]
pub(crate) async fn get_group(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<GroupDto>> {
    let group = st.control.resolve_group(&key)?;
    Ok(Json(group_dto(&st, group).await?))
}

#[utoipa::path(put, path = "/api/v1/groups/{group}", tag = "groups",
    params(("group" = String, Path, description = "Group id or name")),
    request_body = GroupRequest,
    responses((status = 200, body = GroupDto), (status = 400, body = ErrorBody),
              (status = 409, description = "A member doesn't match the new repository or branch", body = ErrorBody)))]
pub(crate) async fn update_group(
    State(st): State<AppState>,
    Path(key): Path<String>,
    ApiJson(req): ApiJson<GroupRequest>,
) -> ApiResult<Json<GroupDto>> {
    if !req.members.is_empty() {
        return Err(ApiError::bad_request(
            "Change members with PUT /groups/{group}/members",
        ));
    }
    let group = st
        .control
        .update_group(
            &key,
            GroupSpec {
                name: req.name,
                repository_url: req.repository_url,
                branch: req.branch.unwrap_or_default(),
                rollback_on_failure: req.rollback_on_failure,
            },
        )
        .await?;
    Ok(Json(group_dto(&st, group).await?))
}

#[utoipa::path(delete, path = "/api/v1/groups/{group}", tag = "groups",
    params(("group" = String, Path, description = "Group id or name")),
    responses((status = 204, description = "Deleted; its apps stay, now outside any group"),
              (status = 409, description = "The group is being deployed", body = ErrorBody)))]
pub(crate) async fn delete_group(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<StatusCode> {
    let group = st.control.resolve_group(&key)?;
    st.control.delete_group(&group.id).await?;
    let _ = st.auth.delete_webhook_secret(&webhook_key(&group.id)).await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(put, path = "/api/v1/groups/{group}/members", tag = "groups",
    params(("group" = String, Path, description = "Group id or name")),
    request_body = MembersRequest,
    responses((status = 200, body = GroupDto), (status = 400, body = ErrorBody)))]
pub(crate) async fn set_members(
    State(st): State<AppState>,
    Path(key): Path<String>,
    ApiJson(req): ApiJson<MembersRequest>,
) -> ApiResult<Json<GroupDto>> {
    let members = member_specs(&st, req.members)?;
    let group = st.control.set_group_members(&key, members).await?;
    Ok(Json(group_dto(&st, group).await?))
}

#[utoipa::path(post, path = "/api/v1/groups/{group}/members", tag = "groups",
    params(("group" = String, Path, description = "Group id or name")),
    request_body = AddMemberRequest,
    responses((status = 200, body = GroupDto), (status = 400, body = ErrorBody)))]
pub(crate) async fn add_member(
    State(st): State<AppState>,
    Path(key): Path<String>,
    ApiJson(req): ApiJson<AddMemberRequest>,
) -> ApiResult<Json<GroupDto>> {
    let group = st.control.resolve_group(&key)?;
    let project = st.control.resolve_project(req.project.trim())?;
    let mut members = current_specs(&group);
    if members.iter().any(|m| m.project_id == project.id) {
        return Err(ApiError::bad_request(format!(
            "'{}' is already in this group",
            project.name
        )));
    }
    let at = req
        .position
        .map(|p| p.clamp(1, members.len() + 1) - 1)
        .unwrap_or(members.len());
    members.insert(
        at,
        MemberSpec {
            project_id: project.id,
            watch_paths: req.watch_paths,
        },
    );
    let group = st.control.set_group_members(&group.id, members).await?;
    Ok(Json(group_dto(&st, group).await?))
}

#[utoipa::path(put, path = "/api/v1/groups/{group}/members/{project}", tag = "groups",
    params(("group" = String, Path, description = "Group id or name"),
           ("project" = String, Path, description = "Member project id or name")),
    request_body = UpdateMemberRequest,
    responses((status = 200, body = GroupDto), (status = 404, body = ErrorBody)))]
pub(crate) async fn update_member(
    State(st): State<AppState>,
    Path((key, member)): Path<(String, String)>,
    ApiJson(req): ApiJson<UpdateMemberRequest>,
) -> ApiResult<Json<GroupDto>> {
    let group = st.control.resolve_group(&key)?;
    let project = st.control.resolve_project(&member)?;
    let mut members = current_specs(&group);
    let index = members
        .iter()
        .position(|m| m.project_id == project.id)
        .ok_or_else(|| {
            ApiError::from(ControlError::NotFound(format!(
                "'{}' is not in group '{}'",
                project.name, group.name
            )))
        })?;
    let mut spec = members.remove(index);
    if let Some(paths) = req.watch_paths {
        spec.watch_paths = paths;
    }
    let at = req
        .position
        .map(|p| p.clamp(1, members.len() + 1) - 1)
        .unwrap_or(index);
    members.insert(at, spec);
    let group = st.control.set_group_members(&group.id, members).await?;
    Ok(Json(group_dto(&st, group).await?))
}

#[utoipa::path(delete, path = "/api/v1/groups/{group}/members/{project}", tag = "groups",
    params(("group" = String, Path, description = "Group id or name"),
           ("project" = String, Path, description = "Member project id or name")),
    responses((status = 200, body = GroupDto), (status = 404, body = ErrorBody)))]
pub(crate) async fn remove_member(
    State(st): State<AppState>,
    Path((key, member)): Path<(String, String)>,
) -> ApiResult<Json<GroupDto>> {
    let group = st.control.resolve_group(&key)?;
    let project = st.control.resolve_project(&member)?;
    let mut members = current_specs(&group);
    let before = members.len();
    members.retain(|m| m.project_id != project.id);
    if members.len() == before {
        return Err(ControlError::NotFound(format!(
            "'{}' is not in group '{}'",
            project.name, group.name
        ))
        .into());
    }
    let group = st.control.set_group_members(&group.id, members).await?;
    Ok(Json(group_dto(&st, group).await?))
}

#[utoipa::path(post, path = "/api/v1/groups/{group}/deploy", tag = "groups",
    params(("group" = String, Path, description = "Group id or name")),
    request_body = GroupDeployRequest,
    responses(
        (status = 202, description = "Started; follow it via GET /groups/{group}/deployments or the event stream", body = GroupDeployAccepted),
        (status = 409, description = "Already running, no members, or a member is being deployed", body = ErrorBody)))]
pub(crate) async fn deploy_group(
    State(st): State<AppState>,
    Path(key): Path<String>,
    ApiJson(req): ApiJson<GroupDeployRequest>,
) -> ApiResult<(StatusCode, Json<GroupDeployAccepted>)> {
    let group = st.control.resolve_group(&key)?;
    let id = st
        .control
        .deploy_group(&group.id, non_empty(req.commit), req.only_changed, "http")
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(GroupDeployAccepted {
            group_deployment_id: id,
            group_id: group.id,
        }),
    ))
}

#[utoipa::path(post, path = "/api/v1/groups/{group}/rollback", tag = "groups",
    params(("group" = String, Path, description = "Group id or name")),
    request_body(content = Object, description = "Empty object `{}`"),
    responses(
        (status = 200, description = "Every member the latest group deployment switched is back on its previous release", body = GroupRollbackDto),
        (status = 409, description = "Nothing to roll back, or the group is busy", body = ErrorBody)))]
pub(crate) async fn rollback_group(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<GroupRollbackDto>> {
    let results = st.control.rollback_group(&key).await?;
    Ok(Json(GroupRollbackDto {
        members: results
            .into_iter()
            .map(|r| MemberRollbackDto {
                project_id: r.project_id.to_string(),
                name: r.name,
                version: r.version,
                error: r.error,
            })
            .collect(),
    }))
}

#[utoipa::path(get, path = "/api/v1/groups/{group}/deployments", tag = "groups",
    params(("group" = String, Path, description = "Group id or name")),
    responses((status = 200, description = "Oldest first", body = [GroupDeploymentDto])))]
pub(crate) async fn list_group_deployments(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Vec<GroupDeploymentDto>>> {
    let group = st.control.resolve_group(&key)?;
    Ok(Json(
        st.control
            .group_deployments(&group.id)
            .into_iter()
            .map(|r| run_dto(&st, r))
            .collect(),
    ))
}

// ----------------------------------------------------------------------
// Webhook
// ----------------------------------------------------------------------

#[utoipa::path(get, path = "/api/v1/groups/{group}/webhook", tag = "webhooks",
    params(("group" = String, Path, description = "Group id or name")),
    responses((status = 200, description = "Webhook setup and recent deliveries", body = GroupWebhookStatusDto)))]
pub(crate) async fn webhook_status(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<GroupWebhookStatusDto>> {
    let group = st.control.resolve_group(&key)?;
    let configured = st
        .auth
        .webhook_secret(&webhook_key(&group.id))
        .await
        .map_err(ApiError::internal)?
        .is_some();
    let recent = st
        .control
        .list_events(None, usize::MAX)
        .await?
        .into_iter()
        .rev()
        .filter(|e| e.event_type == "WebhookReceived")
        .filter_map(|e| {
            let p: Value = serde_json::from_str(&e.payload_json).ok()?;
            (p.get("group_id")?.as_str()? == group.id).then_some((e, p))
        })
        .take(crate::appconfig::RECENT_DELIVERIES)
        .map(|(e, p)| {
            let text = |k: &str| p.get(k).and_then(|v| v.as_str()).map(str::to_string);
            WebhookDeliveryDto {
                received_at: e.created_at,
                delivery: text("delivery").unwrap_or_default(),
                event: text("event").unwrap_or_default(),
                status: text("status").unwrap_or_default(),
                reason: text("reason"),
                deployment_id: text("group_deployment_id"),
            }
        })
        .collect();
    Ok(Json(GroupWebhookStatusDto {
        configured,
        path: format!("/hooks/github/groups/{}", group.id),
        content_type: "application/json".into(),
        events: vec!["push".into()],
        branch: group.branch,
        repository_url: group.repository_url,
        recent,
    }))
}

#[utoipa::path(post, path = "/api/v1/groups/{group}/webhook", tag = "webhooks",
    params(("group" = String, Path, description = "Group id or name")),
    request_body(content = Object, description = "Empty object `{}`"),
    responses((status = 200, description = "New secret, shown only in this response", body = WebhookSecretDto)))]
pub(crate) async fn rotate_webhook_secret(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<WebhookSecretDto>> {
    let group = st.control.resolve_group(&key)?;
    let secret = st
        .auth
        .rotate_webhook_secret(&webhook_key(&group.id))
        .await
        .map_err(ApiError::internal)?;
    st.control
        .record(
            "WebhookSecretRotated",
            json!({"group_id": group.id, "name": group.name}),
        )
        .await?;
    Ok(Json(WebhookSecretDto {
        path: format!("/hooks/github/groups/{}", group.id),
        secret,
        content_type: "application/json".into(),
        events: vec!["push".into()],
    }))
}

#[utoipa::path(post, path = "/api/v1/groups/{group}/webhook/test", tag = "webhooks",
    params(("group" = String, Path, description = "Group id or name")),
    request_body(content = Object, description = "Empty object `{}`"),
    responses((status = 200, description = "A signed ping went through the group's webhook endpoint", body = WebhookResult),
              (status = 409, description = "No webhook secret yet", body = ErrorBody)))]
pub(crate) async fn test_webhook(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<WebhookResult>> {
    let group = st.control.resolve_group(&key)?;
    let secret = st
        .auth
        .webhook_secret(&webhook_key(&group.id))
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "failed_precondition",
                "Generate a webhook secret first",
            )
        })?;
    let body =
        serde_json::to_vec(&json!({"zen": "Test delivery from the Aegis dashboard", "hook_id": 0}))
            .map_err(ApiError::internal)?;
    let header = |v: String| HeaderValue::from_str(&v).map_err(ApiError::internal);
    let mut headers = HeaderMap::new();
    headers.insert("x-github-event", HeaderValue::from_static("ping"));
    headers.insert(
        "x-github-delivery",
        header(format!("aegis-test-{}", aegis_types::EventId::new()))?,
    );
    headers.insert(
        "x-hub-signature-256",
        header(aegis_webhook::sign_github_payload(&secret, &body))?,
    );
    let (_, Json(result)) =
        github_group_webhook(State(st), Path(group.id), headers, Bytes::from(body)).await?;
    Ok(Json(result))
}

#[utoipa::path(post, path = "/hooks/github/groups/{group}", tag = "webhooks", security(()),
    params(("group" = String, Path, description = "Group id")),
    request_body(content = Object, description = "GitHub webhook payload, signed with X-Hub-Signature-256"),
    responses(
        (status = 202, description = "Push to the group's branch: group deployment of the changed apps started (or event ignored)", body = WebhookResult),
        (status = 200, description = "ping, or a redelivery that was already handled", body = WebhookResult),
        (status = 401, description = "Missing or invalid signature", body = ErrorBody),
        (status = 404, description = "Unknown group or no webhook configured", body = ErrorBody)))]
pub(crate) async fn github_group_webhook(
    State(st): State<AppState>,
    Path(key): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<WebhookResult>)> {
    let unknown = || ApiError::new(StatusCode::NOT_FOUND, "not_found", "Unknown webhook");
    let group = st
        .control
        .projection()
        .get_group(&key)
        .ok_or_else(unknown)?;
    let secret = st
        .auth
        .webhook_secret(&webhook_key(&group.id))
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(unknown)?;
    if !aegis_webhook::verify_github_signature(
        &secret,
        &body,
        header_str(&headers, "x-hub-signature-256"),
    ) {
        return Err(ApiError::unauthenticated(
            "Missing or invalid X-Hub-Signature-256",
        ));
    }
    let event = header_str(&headers, "x-github-event").to_string();
    let delivery = header_str(&headers, "x-github-delivery").to_string();
    let result = group_event(&st, &group, &headers, &body).await;
    if event == "push" {
        st.control.check_group_updates_soon(&group.id);
    }
    let (status, reason, run) = match &result {
        Ok((_, Json(r))) => (r.status.clone(), r.reason.clone(), r.deployment_id.clone()),
        Err(e) => ("error".to_string(), Some(e.message().to_string()), None),
    };
    let _ = with_actor(
        "webhook:github".into(),
        st.control.record(
            "WebhookReceived",
            json!({
                "group_id": group.id,
                "delivery": delivery,
                "event": event,
                "status": status,
                "reason": reason,
                "group_deployment_id": run,
            }),
        ),
    )
    .await;
    result
}

async fn group_event(
    st: &AppState,
    group: &GroupState,
    headers: &HeaderMap,
    body: &[u8],
) -> ApiResult<(StatusCode, Json<WebhookResult>)> {
    let delivery = header_str(headers, "x-github-delivery");
    if !delivery.is_empty() {
        let mut seen = st.deliveries.lock().unwrap();
        if seen.iter().any(|d| d == delivery) {
            return Ok(webhook_result(StatusCode::OK, "duplicate", None));
        }
        if seen.len() >= MAX_REMEMBERED_DELIVERIES {
            seen.pop_front();
        }
        seen.push_back(delivery.to_string());
    }
    match header_str(headers, "x-github-event") {
        "ping" => Ok(webhook_result(StatusCode::OK, "pong", None)),
        "push" => {
            let push = aegis_webhook::parse_push(body)
                .map_err(|e| ApiError::bad_request(e.to_string()))?;
            if push.deleted || push.branch.as_deref() != Some(group.branch.as_str()) {
                return Ok(webhook_result(
                    StatusCode::ACCEPTED,
                    "ignored",
                    Some(format!("Only pushes to '{}' deploy", group.branch)),
                ));
            }
            // The same commit already went out with this group (a replayed
            // push, or a redelivery under a new id).
            if st
                .control
                .group_deployments(&group.id)
                .iter()
                .any(|d| d.commit.as_deref() == Some(push.after.as_str()) && d.status != "Failed")
            {
                return Ok(webhook_result(
                    StatusCode::OK,
                    "ignored",
                    Some("This commit has already been deployed with the group".into()),
                ));
            }
            let id = with_actor(
                "webhook:github".into(),
                st.control
                    .deploy_group(&group.id, Some(push.after.clone()), true, "github-push"),
            )
            .await?;
            Ok((
                StatusCode::ACCEPTED,
                Json(WebhookResult {
                    status: "queued".into(),
                    deployment_id: Some(id),
                    reason: None,
                }),
            ))
        }
        other => Ok(webhook_result(
            StatusCode::ACCEPTED,
            "ignored",
            Some(format!("Event '{}' is not handled", other)),
        )),
    }
}
