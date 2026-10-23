//! App configuration endpoints: secret environment variables, the app's
//! environment overview and `.env` import, the aegis.toml preview for
//! settings changes, and webhook setup (status, test delivery). Secrets,
//! imports and webhooks are managed by signed-in users only, not API tokens.

use crate::dto::*;
use crate::{ApiError, ApiJson, ApiResult, AppState};
use aegis_types::ProjectId;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::Json;
use serde_json::Value;

/// Delivery events listed in the webhook status.
const RECENT_DELIVERIES: usize = 10;

fn is_running(st: &AppState, id: &ProjectId) -> bool {
    st.control
        .list_projects()
        .into_iter()
        .find(|v| &v.project.id == id)
        .and_then(|v| v.process)
        .is_some_and(|p| matches!(p.status.as_str(), "Running" | "Backoff"))
}

#[utoipa::path(get, path = "/api/v1/projects/{project}/secrets", tag = "secrets",
    params(("project" = String, Path, description = "Project id or name")),
    responses((status = 200, description = "Secret names (values are never returned)", body = [SecretDto])))]
pub(crate) async fn list_secrets(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Vec<SecretDto>>> {
    let project = st.control.resolve_project(&key)?;
    Ok(Json(
        st.control
            .list_secrets(&project.id)
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

#[utoipa::path(put, path = "/api/v1/projects/{project}/secrets/{name}", tag = "secrets",
    params(("project" = String, Path, description = "Project id or name"),
           ("name" = String, Path, description = "Variable name, e.g. DATABASE_URL")),
    request_body = SetSecretRequest,
    responses((status = 200, description = "Stored encrypted; applies when the app next starts or restarts", body = SecretSaved),
              (status = 400, body = ErrorBody)))]
pub(crate) async fn set_secret(
    State(st): State<AppState>,
    Path((key, name)): Path<(String, String)>,
    ApiJson(req): ApiJson<SetSecretRequest>,
) -> ApiResult<Json<SecretSaved>> {
    let project = st.control.resolve_project(&key)?;
    let created = st.control.set_secret(project.id, &name, &req.value).await?;
    Ok(Json(SecretSaved {
        name: name.trim().to_string(),
        created,
        restart_required: is_running(&st, &project.id),
    }))
}

#[utoipa::path(delete, path = "/api/v1/projects/{project}/secrets/{name}", tag = "secrets",
    params(("project" = String, Path, description = "Project id or name"),
           ("name" = String, Path, description = "Variable name")),
    responses((status = 204, description = "Deleted; the running process keeps it until restarted"),
              (status = 404, body = ErrorBody)))]
pub(crate) async fn delete_secret(
    State(st): State<AppState>,
    Path((key, name)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let project = st.control.resolve_project(&key)?;
    st.control.delete_secret(project.id, &name).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/api/v1/projects/{project}/env", tag = "secrets",
    params(("project" = String, Path, description = "Project id or name")),
    responses((status = 200, description = "Variable names and where each comes from: the env file (read now), aegis.toml, Variables, Secrets. Values are never returned", body = EnvOverviewDto)))]
pub(crate) async fn env_overview(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<EnvOverviewDto>> {
    let project = st.control.resolve_project(&key)?;
    // Reads a file: keep it off the async workers.
    let control = st.control.clone();
    let overview = tokio::task::spawn_blocking(move || control.env_overview(&project.id))
        .await
        .map_err(ApiError::internal)??;
    Ok(Json(overview.into()))
}

#[utoipa::path(post, path = "/api/v1/projects/{project}/env/import/preview", tag = "secrets",
    params(("project" = String, Path, description = "Project id or name")),
    request_body = EnvImportPreviewRequest,
    responses((status = 200, description = "The variables in the file with a suggested type (values are not returned)", body = EnvImportPreviewDto),
              (status = 400, description = "Invalid syntax; the message names the line, never a value", body = ErrorBody)))]
pub(crate) async fn preview_env_import(
    State(st): State<AppState>,
    Path(key): Path<String>,
    ApiJson(req): ApiJson<EnvImportPreviewRequest>,
) -> ApiResult<Json<EnvImportPreviewDto>> {
    let project = st.control.resolve_project(&key)?;
    let variables = st
        .control
        .preview_env_import(&project.id, &req.content)?
        .into_iter()
        .map(Into::into)
        .collect();
    Ok(Json(EnvImportPreviewDto { variables }))
}

#[utoipa::path(post, path = "/api/v1/projects/{project}/env/import", tag = "secrets",
    params(("project" = String, Path, description = "Project id or name")),
    request_body = EnvImportRequest,
    responses((status = 200, description = "Imported as Variables (settings) and Secrets; applies to the next deployment (Variables) or restart (Secrets)", body = EnvImportResult),
              (status = 400, body = ErrorBody)))]
pub(crate) async fn import_env(
    State(st): State<AppState>,
    Path(key): Path<String>,
    ApiJson(req): ApiJson<EnvImportRequest>,
) -> ApiResult<Json<EnvImportResult>> {
    let project = st.control.resolve_project(&key)?;
    let mut choices = std::collections::BTreeMap::new();
    for (name, how) in req.choices {
        let how = aegis_control::ImportAs::parse(&how).ok_or_else(|| {
            ApiError::bad_request(format!("{}: choose variable, secret or skip", name))
        })?;
        choices.insert(name, how);
    }
    let result = st
        .control
        .import_env(project.id, &req.content, &choices)
        .await?;
    Ok(Json(EnvImportResult {
        restart_required: !result.secrets.is_empty() && is_running(&st, &project.id),
        variables: result.variables,
        secrets: result.secrets,
        skipped: result.skipped,
    }))
}

#[utoipa::path(post, path = "/api/v1/projects/{project}/settings/preview", tag = "projects",
    params(("project" = String, Path, description = "Project id or name")),
    request_body = ProjectSettingsDto,
    responses((status = 200, description = "aegis.toml before and after the proposed settings, with a line diff", body = ConfigPreviewDto),
              (status = 400, body = ErrorBody)))]
pub(crate) async fn preview_settings(
    State(st): State<AppState>,
    Path(key): Path<String>,
    ApiJson(req): ApiJson<ProjectSettingsDto>,
) -> ApiResult<Json<ConfigPreviewDto>> {
    let project = st.control.resolve_project(&key)?;
    Ok(Json(
        st.control.preview_settings(project.id, req.into())?.into(),
    ))
}

#[utoipa::path(get, path = "/api/v1/projects/{project}/webhook", tag = "webhooks",
    params(("project" = String, Path, description = "Project id or name")),
    responses((status = 200, description = "Webhook setup and recent deliveries", body = WebhookStatusDto)))]
pub(crate) async fn webhook_status(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<WebhookStatusDto>> {
    let project = st.control.resolve_project(&key)?;
    let configured = st
        .auth
        .webhook_secret(&project.id.to_string())
        .await
        .map_err(ApiError::internal)?
        .is_some();
    let recent = st
        .control
        .list_events(Some(&project.id), usize::MAX)
        .await?
        .into_iter()
        .rev()
        .filter(|e| e.event_type == "WebhookReceived")
        .take(RECENT_DELIVERIES)
        .map(|e| {
            let p: Value = serde_json::from_str(&e.payload_json).unwrap_or_default();
            let text = |k: &str| p.get(k).and_then(|v| v.as_str()).map(str::to_string);
            WebhookDeliveryDto {
                received_at: e.created_at,
                delivery: text("delivery").unwrap_or_default(),
                event: text("event").unwrap_or_default(),
                status: text("status").unwrap_or_default(),
                reason: text("reason"),
                deployment_id: text("deployment_id"),
            }
        })
        .collect();
    Ok(Json(WebhookStatusDto {
        configured,
        path: format!("/hooks/github/{}", project.id),
        content_type: "application/json".into(),
        events: vec!["push".into()],
        branch: project.branch,
        repository_url: project.repository_url,
        recent,
    }))
}

#[utoipa::path(post, path = "/api/v1/projects/{project}/webhook/test", tag = "webhooks",
    params(("project" = String, Path, description = "Project id or name")),
    request_body(content = Object, description = "Empty object `{}`"),
    responses((status = 200, description = "A signed ping went through the webhook endpoint", body = WebhookResult),
              (status = 409, description = "No webhook secret yet", body = ErrorBody)))]
pub(crate) async fn test_webhook(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<WebhookResult>> {
    let project = st.control.resolve_project(&key)?;
    let secret = st
        .auth
        .webhook_secret(&project.id.to_string())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "failed_precondition",
                "Generate a webhook secret first",
            )
        })?;
    // The same path GitHub's own ping takes: signature check, delivery
    // de-duplication and recording.
    let body = serde_json::to_vec(&serde_json::json!({
        "zen": "Test delivery from the Aegis dashboard",
        "hook_id": 0,
    }))
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
    let (_, Json(result)) = crate::auth::github_webhook(
        State(st),
        Path(project.id.to_string()),
        headers,
        Bytes::from(body),
    )
    .await?;
    Ok(Json(result))
}
