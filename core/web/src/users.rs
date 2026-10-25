//! User accounts: list, create, change role or password, delete. Admins
//! only (enforced in `auth::require_auth`); API tokens can't use these.

use crate::auth::Principal;
use crate::dto::ErrorBody;
use crate::{ApiError, ApiJson, ApiResult, AppState};
use aegis_auth::{Role, UserError, UserInfo};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UserDto {
    pub username: String,
    /// "viewer", "deployer" or "admin".
    pub role: String,
    pub created_at: String,
    pub password_changed_at: String,
    /// True: the user sees every app (always the case for admins).
    pub all_apps: bool,
    /// With `all_apps` false, the names of the only apps the user sees.
    pub apps: Vec<String>,
}

fn user_dto(st: &AppState, u: UserInfo) -> UserDto {
    let projects = st.control.list_projects();
    UserDto {
        all_apps: u.apps.is_none(),
        apps: u
            .apps
            .unwrap_or_default()
            .iter()
            .filter_map(|id| {
                projects
                    .iter()
                    .find(|p| p.project.id.to_string() == *id)
                    .map(|p| p.project.name.clone())
            })
            .collect(),
        username: u.username,
        role: u.role.as_str().to_string(),
        created_at: u.created_at,
        password_changed_at: u.password_changed_at,
    }
}

/// App ids for names or ids, or the error for the first unknown one.
fn resolve_apps(st: &AppState, apps: &[String]) -> ApiResult<Vec<String>> {
    let mut ids: Vec<String> = Vec::new();
    for key in apps {
        let id = st.control.resolve_project(key.trim())?.id.to_string();
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// `Some(ids)`: limited to those apps; `None`: all apps.
fn access_change(
    st: &AppState,
    role: Role,
    all_apps: Option<bool>,
    apps: Option<&[String]>,
) -> ApiResult<Option<Option<Vec<String>>>> {
    match (all_apps, apps) {
        (None, None) => Ok(None),
        (Some(true), Some(list)) if !list.is_empty() => Err(ApiError::bad_request(
            "Give either all_apps: true or a list of apps, not both",
        )),
        (Some(true), _) => Ok(Some(None)),
        (_, _) if role == Role::Admin => Err(ApiError::bad_request(
            "Admins always see every app; use the deployer or viewer role to limit apps",
        )),
        (Some(false), None) => Err(ApiError::bad_request(
            "all_apps: false needs `apps`, the list of apps the user may see",
        )),
        (_, Some(list)) => Ok(Some(Some(resolve_apps(st, list)?))),
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateUserRequest {
    pub username: String,
    /// At least 12 characters. Give it to the person out of band; they can
    /// change it after signing in.
    pub password: String,
    /// "viewer", "deployer" or "admin".
    pub role: String,
    /// `true` (the default) lets the user see every app.
    #[serde(default)]
    pub all_apps: Option<bool>,
    /// Names or ids of the only apps the user may see (not for admins).
    /// Apps added later are not included until you add them.
    #[serde(default)]
    pub apps: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct UpdateUserRequest {
    /// New role.
    pub role: Option<String>,
    /// New password; signs the user out everywhere.
    pub password: Option<String>,
    /// `true`: the user sees every app again.
    pub all_apps: Option<bool>,
    /// Replaces the list of apps the user may see (empty: none).
    pub apps: Option<Vec<String>>,
}

fn parse_role(value: &str) -> ApiResult<Role> {
    Role::parse(value)
        .ok_or_else(|| ApiError::bad_request("role must be \"viewer\", \"deployer\" or \"admin\""))
}

fn refused(e: UserError) -> ApiError {
    match e {
        UserError::Invalid(m) => ApiError::bad_request(m),
        UserError::NotFound => ApiError::new(StatusCode::NOT_FOUND, "not_found", e.to_string()),
        UserError::Exists | UserError::LastAdmin => {
            ApiError::new(StatusCode::CONFLICT, "failed_precondition", e.to_string())
        }
    }
}

#[utoipa::path(get, path = "/api/v1/users", tag = "users",
    responses((status = 200, body = [UserDto])))]
pub(crate) async fn list_users(State(st): State<AppState>) -> ApiResult<Json<Vec<UserDto>>> {
    Ok(Json(
        st.auth
            .list_users()
            .await
            .map_err(ApiError::internal)?
            .into_iter()
            .map(|u| user_dto(&st, u))
            .collect(),
    ))
}

#[utoipa::path(post, path = "/api/v1/users", tag = "users",
    request_body = CreateUserRequest,
    responses((status = 201, body = UserDto),
              (status = 400, body = ErrorBody),
              (status = 409, description = "The username is taken", body = ErrorBody)))]
pub(crate) async fn create_user(
    State(st): State<AppState>,
    ApiJson(req): ApiJson<CreateUserRequest>,
) -> ApiResult<(StatusCode, Json<UserDto>)> {
    let role = parse_role(&req.role)?;
    let username = req.username.trim();
    // Checked before the account exists, so a bad app name leaves nothing behind.
    let access = access_change(&st, role, req.all_apps, req.apps.as_deref())?;
    let user = st
        .auth
        .create_user(username, &req.password, role)
        .await
        .map_err(ApiError::internal)?
        .map_err(refused)?;
    if let Some(Some(ids)) = &access {
        st.auth
            .set_user_apps(username, Some(ids))
            .await
            .map_err(ApiError::internal)?;
    }
    st.control
        .record(
            "UserCreated",
            json!({"username": user.username, "role": role.as_str(), "apps": access.as_ref().map(|a| a.clone().map_or(json!("all"), |ids| json!(ids)))}),
        )
        .await?;
    let user = st
        .auth
        .user(username)
        .await
        .map_err(ApiError::internal)?
        .unwrap_or(user);
    Ok((StatusCode::CREATED, Json(user_dto(&st, user))))
}

#[utoipa::path(put, path = "/api/v1/users/{username}", tag = "users",
    params(("username" = String, Path)),
    request_body = UpdateUserRequest,
    responses((status = 200, body = UserDto),
              (status = 400, body = ErrorBody),
              (status = 404, body = ErrorBody),
              (status = 409, description = "Would leave no admin", body = ErrorBody)))]
pub(crate) async fn update_user(
    State(st): State<AppState>,
    Path(username): Path<String>,
    ApiJson(req): ApiJson<UpdateUserRequest>,
) -> ApiResult<Json<UserDto>> {
    let before = st
        .auth
        .user(&username)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| refused(UserError::NotFound))?;
    let role = req.role.as_deref().map(parse_role).transpose()?;
    if let Some(password) = &req.password {
        if password.chars().count() < aegis_auth::MIN_PASSWORD_LEN {
            return Err(ApiError::bad_request(format!(
                "Password must be at least {} characters",
                aegis_auth::MIN_PASSWORD_LEN
            )));
        }
    }
    if let Some(role) = role.filter(|r| *r != before.role) {
        st.auth
            .set_role(&username, role)
            .await
            .map_err(ApiError::internal)?
            .map_err(refused)?;
        st.control
            .record(
                "UserRoleChanged",
                json!({"username": username, "from": before.role.as_str(), "to": role.as_str()}),
            )
            .await?;
    }
    let access = access_change(
        &st,
        role.unwrap_or(before.role),
        req.all_apps,
        req.apps.as_deref(),
    )?;
    if let Some(apps) = &access {
        st.auth
            .set_user_apps(&username, apps.as_deref())
            .await
            .map_err(ApiError::internal)?;
        st.control
            .record(
                "UserAppsChanged",
                json!({"username": username, "apps": apps.as_ref().map_or(json!("all"), |ids| json!(ids))}),
            )
            .await?;
    }
    if let Some(password) = &req.password {
        st.auth
            .set_password(&username, password)
            .await
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        st.control
            .record("PasswordReset", json!({"username": username}))
            .await?;
    }
    let user = st
        .auth
        .user(&username)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| refused(UserError::NotFound))?;
    Ok(Json(user_dto(&st, user)))
}

#[utoipa::path(delete, path = "/api/v1/users/{username}", tag = "users",
    params(("username" = String, Path)),
    responses((status = 204, description = "Deleted; their sessions end"),
              (status = 404, body = ErrorBody),
              (status = 409, description = "Would leave no admin, or deleting yourself", body = ErrorBody)))]
pub(crate) async fn delete_user(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(username): Path<String>,
) -> ApiResult<StatusCode> {
    if matches!(&principal, Principal::User { username: me, .. } if *me == username) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "failed_precondition",
            "You can't delete your own account; ask another admin",
        ));
    }
    st.auth
        .delete_user(&username)
        .await
        .map_err(ApiError::internal)?
        .map_err(refused)?;
    st.control
        .record("UserDeleted", json!({"username": username}))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
