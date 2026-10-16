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
}

impl From<UserInfo> for UserDto {
    fn from(u: UserInfo) -> Self {
        Self {
            username: u.username,
            role: u.role.as_str().to_string(),
            created_at: u.created_at,
            password_changed_at: u.password_changed_at,
        }
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
}

#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct UpdateUserRequest {
    /// New role.
    pub role: Option<String>,
    /// New password; signs the user out everywhere.
    pub password: Option<String>,
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
            .map(Into::into)
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
    let user = st
        .auth
        .create_user(username, &req.password, role)
        .await
        .map_err(ApiError::internal)?
        .map_err(refused)?;
    st.control
        .record(
            "UserCreated",
            json!({"username": user.username, "role": role.as_str()}),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(user.into())))
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
    Ok(Json(user.into()))
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
