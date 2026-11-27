//! The optional Claude Code assistant over HTTP (see
//! `aegis_control::assistant`). Off until the owner turns it on, and only
//! the owner, signed in, can use any of it: answers draw on every app's
//! logs, and those are sent to Anthropic.

use crate::auth::Principal;
use crate::dto::ErrorBody;
use crate::{ApiError, ApiJson, ApiResult, AppState};
use aegis_auth::Visibility;
use axum::extract::State;
use axum::http::StatusCode;
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

const SETTING: &str = "assistant_enabled";

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AssistantDto {
    /// Turned on by the owner (default: off).
    pub enabled: bool,
    /// Claude Code can run on this platform.
    pub supported: bool,
    /// The installed Claude Code version.
    pub version: Option<String>,
    /// "api_key" or "oauth_token" when one is stored; the value is never returned.
    pub credential: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetAssistantRequest {
    pub enabled: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AssistantCredentialRequest {
    /// An Anthropic API key or a `claude setup-token` token; "" removes it.
    pub value: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AssistantChatRequest {
    pub message: String,
    /// From the previous reply, to continue that conversation.
    pub session_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AssistantReplyDto {
    pub answer: String,
    pub session_id: Option<String>,
    pub cost_usd: Option<f64>,
}

async fn require_owner(st: &AppState, principal: &Principal) -> ApiResult<()> {
    if let Principal::User { username, .. } = principal {
        let owner = st.auth.owner().await.map_err(ApiError::internal)?;
        if owner.as_deref() == Some(username) {
            return Ok(());
        }
    }
    Err(ApiError::forbidden(
        "Only the owner (the first admin) can use the assistant",
    ))
}

async fn enabled(st: &AppState) -> ApiResult<bool> {
    Ok(st
        .auth
        .setting(SETTING)
        .await
        .map_err(ApiError::internal)?
        .as_deref()
        == Some("1"))
}

async fn require_enabled(st: &AppState) -> ApiResult<()> {
    if enabled(st).await? {
        return Ok(());
    }
    Err(ApiError::new(
        StatusCode::CONFLICT,
        "failed_precondition",
        "The assistant is off; turn it on under Account",
    ))
}

async fn view(st: &AppState) -> ApiResult<AssistantDto> {
    let enabled = enabled(st).await?;
    // Nothing is run while the assistant is off.
    let status = if enabled {
        Some(st.control.assistant_status().await)
    } else {
        None
    };
    Ok(AssistantDto {
        enabled,
        supported: cfg!(unix),
        version: status.as_ref().and_then(|s| s.version.clone()),
        credential: status.and_then(|s| s.credential).map(str::to_string),
    })
}

#[utoipa::path(get, path = "/api/v1/system/assistant", tag = "system",
    responses((status = 200, body = AssistantDto),
              (status = 403, description = "Not the owner", body = ErrorBody)))]
pub(crate) async fn assistant_status(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Json<AssistantDto>> {
    require_owner(&st, &principal).await?;
    Ok(Json(view(&st).await?))
}

#[utoipa::path(put, path = "/api/v1/system/assistant", tag = "system",
    request_body = SetAssistantRequest,
    responses((status = 200, body = AssistantDto), (status = 403, body = ErrorBody)))]
pub(crate) async fn set_assistant(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(req): ApiJson<SetAssistantRequest>,
) -> ApiResult<Json<AssistantDto>> {
    require_owner(&st, &principal).await?;
    st.auth
        .set_setting(SETTING, if req.enabled { "1" } else { "0" })
        .await
        .map_err(ApiError::internal)?;
    st.control
        .record("AssistantToggled", json!({"enabled": req.enabled}))
        .await?;
    Ok(Json(view(&st).await?))
}

#[utoipa::path(post, path = "/api/v1/system/assistant/install", tag = "system",
    request_body(content = Object, description = "Empty object `{}`"),
    responses((status = 200, description = "Claude Code installed with Anthropic's installer", body = AssistantDto),
              (status = 409, description = "Off, busy or the installer failed", body = ErrorBody)))]
pub(crate) async fn install_assistant(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Json<AssistantDto>> {
    require_owner(&st, &principal).await?;
    require_enabled(&st).await?;
    st.control.install_assistant().await?;
    Ok(Json(view(&st).await?))
}

#[utoipa::path(put, path = "/api/v1/system/assistant/credential", tag = "system",
    request_body = AssistantCredentialRequest,
    responses((status = 200, body = AssistantDto), (status = 400, body = ErrorBody)))]
pub(crate) async fn set_assistant_credential(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(req): ApiJson<AssistantCredentialRequest>,
) -> ApiResult<Json<AssistantDto>> {
    require_owner(&st, &principal).await?;
    st.control.set_assistant_credential(&req.value).await?;
    Ok(Json(view(&st).await?))
}

#[utoipa::path(post, path = "/api/v1/system/assistant/chat", tag = "system",
    request_body = AssistantChatRequest,
    responses((status = 200, body = AssistantReplyDto),
              (status = 409, description = "Off, not set up, busy or Claude failed", body = ErrorBody)))]
pub(crate) async fn ask_assistant(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(vis): Extension<Visibility>,
    ApiJson(req): ApiJson<AssistantChatRequest>,
) -> ApiResult<Json<AssistantReplyDto>> {
    require_owner(&st, &principal).await?;
    require_enabled(&st).await?;
    // Apps hidden behind a password that this session hasn't unlocked.
    let hidden = st
        .control
        .list_projects()
        .into_iter()
        .map(|p| p.project.id.to_string())
        .filter(|id| vis.hides(id))
        .collect();
    let reply = st
        .control
        .ask_assistant(&req.message, req.session_id.as_deref(), &hidden)
        .await?;
    Ok(Json(AssistantReplyDto {
        answer: reply.answer,
        session_id: reply.session_id,
        cost_usd: reply.cost_usd,
    }))
}
