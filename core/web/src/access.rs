//! Which apps a request may see: the apps an account is limited to, and
//! apps hidden behind a password.
//!
//! The rules live in one place so no endpoint can forget them:
//! - `require_auth` computes a [`Visibility`] for every request and calls
//!   [`guard`], which answers "not found" for any app-specific path of an
//!   app the caller can't see (so hidden and nonexistent look the same);
//! - list endpoints filter with the same `Visibility` (an extension).
//!
//! API tokens are not limited: an admin issued them, and automation
//! (CI deploys) must keep working whatever dashboards hide. Admins are
//! never limited to a list but do need to unlock a hidden app.

use crate::auth::{audit, client_ip, Credential, Principal};
use crate::dto::{ErrorBody, EventDto};
use crate::{ApiError, ApiJson, ApiResult, AppState};
use aegis_auth::Visibility;
use aegis_projection::ProjectState;
use aegis_types::DeploymentId;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use utoipa::ToSchema;

fn unknown_project() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "not_found", "Unknown project")
}

/// Resolves an app by id or name, as "unknown" when the caller can't see it.
pub(crate) fn visible_project(
    st: &AppState,
    vis: &Visibility,
    key: &str,
) -> ApiResult<ProjectState> {
    let project = st.control.resolve_project(key)?;
    if vis.hides(&project.id.to_string()) {
        return Err(unknown_project());
    }
    Ok(project)
}

/// True when the event is about an app the caller can't see.
pub(crate) fn event_hidden(vis: &Visibility, event: &EventDto) -> bool {
    !vis.sees_all()
        && event
            .payload
            .get("project_id")
            .and_then(|v| v.as_str())
            .is_some_and(|id| vis.hides(id))
}

/// The app an app-specific `/api/v1/...` path is about, when it names one.
fn path_project(st: &AppState, path: &str) -> Option<String> {
    let parts: Vec<&str> = path.strip_prefix("/api/v1/")?.split('/').collect();
    match parts.as_slice() {
        ["projects", key, ..] if !key.is_empty() => st
            .control
            .resolve_project(key)
            .ok()
            .map(|p| p.id.to_string()),
        // A process id, or a project's id or name (even with no process yet).
        ["processes", target, ..] if !target.is_empty() => st
            .control
            .resolve_process(target)
            .ok()
            .map(|p| p.project_id.to_string())
            .or_else(|| {
                st.control
                    .resolve_project(target)
                    .ok()
                    .map(|p| p.id.to_string())
            }),
        ["deployments", id, ..] if !id.is_empty() => id
            .parse::<DeploymentId>()
            .ok()
            .and_then(|id| st.control.get_deployment(&id))
            .map(|d| d.project_id.to_string()),
        _ => None,
    }
}

/// True when a group has a member the caller can't see. Such groups are
/// hidden whole: a group deploys its members together.
pub(crate) fn group_hidden(vis: &Visibility, group: &aegis_projection::GroupState) -> bool {
    group
        .members
        .iter()
        .any(|m| vis.hides(&m.project_id.to_string()))
}

/// `Some(404)` when the request is about an app (or a group with an app)
/// the caller can't see. Unlocking is the one thing allowed on a locked app.
pub(crate) fn guard(
    st: &AppState,
    vis: &Visibility,
    method: &Method,
    path: &str,
) -> Option<ApiError> {
    if vis.sees_all() {
        return None;
    }
    if let Some(id) = path_project(st, path) {
        let unlock = *method == Method::POST && path.ends_with("/unlock");
        if vis.hides(&id) && !unlock {
            return Some(unknown_project());
        }
        return None;
    }
    let parts: Vec<&str> = path
        .strip_prefix("/api/v1/")
        .unwrap_or("")
        .split('/')
        .collect();
    if let ["groups", key, ..] = parts.as_slice() {
        if let Ok(group) = st.control.resolve_group(key) {
            if group_hidden(vis, &group) {
                return Some(ApiError::new(
                    StatusCode::NOT_FOUND,
                    "not_found",
                    format!("Unknown group '{}'", key),
                ));
            }
        }
    }
    None
}

/// Computes what the principal may see.
pub(crate) async fn visibility_for(
    st: &AppState,
    principal: &Principal,
    credential: &Credential,
) -> Result<Visibility, ApiError> {
    match (principal, credential) {
        (Principal::User { username, role, .. }, Credential::Session(cookie)) => st
            .auth
            .visibility(username, *role, cookie)
            .await
            .map_err(ApiError::internal),
        _ => Ok(Visibility::all()),
    }
}

// ----------------------------------------------------------------------
// Hide, unlock, lock
// ----------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HideRequest {
    /// At least 12 characters. Needed to see the app again after signing in.
    pub password: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UnlockRequest {
    pub password: String,
}

fn session_cookie(credential: &Credential) -> ApiResult<&str> {
    match credential {
        Credential::Session(cookie) => Ok(cookie),
        Credential::Token(_) => Err(ApiError::forbidden(
            "This endpoint requires a signed-in user, not an API token",
        )),
    }
}

#[utoipa::path(put, path = "/api/v1/projects/{project}/hide", tag = "access",
    params(("project" = String, Path, description = "Project id or name")),
    request_body = HideRequest,
    responses(
        (status = 204, description = "The app is hidden from everyone until their session unlocks it; this session stays unlocked"),
        (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub(crate) async fn hide_project(
    State(st): State<AppState>,
    Extension(credential): Extension<Credential>,
    Path(key): Path<String>,
    ApiJson(req): ApiJson<HideRequest>,
) -> ApiResult<StatusCode> {
    let cookie = session_cookie(&credential)?;
    let project = st.control.resolve_project(&key)?;
    st.auth
        .set_app_lock(&project.id.to_string(), &req.password)
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::bad_request)?;
    st.auth
        .unlock_app(cookie, &project.id.to_string())
        .await
        .map_err(ApiError::internal)?;
    st.control
        .record(
            "AppHidden",
            json!({"project_id": project.id.to_string(), "name": project.name}),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/api/v1/projects/{project}/hide", tag = "access",
    params(("project" = String, Path, description = "Project id or name")),
    responses(
        (status = 204, description = "The app is visible again (to whoever may see it)"),
        (status = 404, body = ErrorBody)))]
pub(crate) async fn unhide_project(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<StatusCode> {
    let project = st.control.resolve_project(&key)?;
    st.auth
        .remove_app_lock(&project.id.to_string())
        .await
        .map_err(ApiError::internal)?;
    st.control
        .record(
            "AppUnhidden",
            json!({"project_id": project.id.to_string(), "name": project.name}),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/v1/projects/{project}/unlock", tag = "access",
    params(("project" = String, Path, description = "Project id or name")),
    request_body = UnlockRequest,
    responses(
        (status = 204, description = "The app is visible to this session until it ends"),
        (status = 403, description = "Wrong app or password", body = ErrorBody),
        (status = 429, description = "Too many wrong passwords; see Retry-After", body = ErrorBody)))]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn unlock_project(
    State(st): State<AppState>,
    Extension(credential): Extension<Credential>,
    Extension(vis): Extension<Visibility>,
    Extension(principal): Extension<Principal>,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Path(key): Path<String>,
    ApiJson(req): ApiJson<UnlockRequest>,
) -> ApiResult<Response> {
    let cookie = session_cookie(&credential)?;
    let ip = client_ip(&st, peer.map(|c| c.0), &headers);
    // Unknown, not hidden and not allowed all answer like a wrong password.
    let wrong = || ApiError::forbidden("Wrong app name or password");
    let project = st
        .control
        .resolve_project(&key)
        .ok()
        .filter(|p| vis.may_unlock(&p.id.to_string()));
    let id = project.as_ref().map(|p| p.id.to_string());
    let attempt_key = format!("unlock:{}", id.as_deref().unwrap_or(&key));
    if let Err(wait) = st.limiter.begin(&ip, &attempt_key) {
        let mut res = ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Too many wrong passwords; try again later",
        )
        .into_response();
        res.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from(wait.as_secs().max(1)),
        );
        return Ok(res);
    }
    let ok = st
        .auth
        .verify_app_lock(id.as_deref(), &req.password)
        .await
        .map_err(ApiError::internal)?;
    let (Some(id), true) = (id, ok) else {
        audit(&st, principal.actor(), "AppUnlockFailed", json!({"ip": ip})).await;
        return Err(wrong());
    };
    st.limiter.record_success(&ip, &attempt_key);
    st.auth
        .unlock_app(cookie, &id)
        .await
        .map_err(ApiError::internal)?;
    audit(
        &st,
        principal.actor(),
        "AppUnlocked",
        json!({"project_id": id}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[utoipa::path(post, path = "/api/v1/projects/{project}/lock", tag = "access",
    params(("project" = String, Path, description = "Project id or name")),
    request_body(content = Object, description = "Empty object `{}`"),
    responses((status = 204, description = "The app is hidden again for this session")))]
pub(crate) async fn lock_project(
    State(st): State<AppState>,
    Extension(credential): Extension<Credential>,
    Path(key): Path<String>,
) -> ApiResult<StatusCode> {
    let cookie = session_cookie(&credential)?;
    let project = st.control.resolve_project(&key)?;
    st.auth
        .relock_app(cookie, &project.id.to_string())
        .await
        .map_err(ApiError::internal)?;
    Ok(StatusCode::NO_CONTENT)
}
