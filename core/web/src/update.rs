//! Self-update: whether a newer Aegis release exists, and installing it.
//!
//! Everyone signed in can see the status (the dashboard shows a banner).
//! Checking now and applying are for admins, and only signed-in users, never
//! API tokens (see `auth::user_only`).

use crate::auth::Principal;
use crate::dto::ErrorBody;
use crate::{ApiError, ApiJson, ApiResult, AppState};
use aegis_control::ControlPlane;
use aegis_update::{UpdateError, UpdateHooks, UpdateStatus};
use axum::extract::State;
use axum::http::StatusCode;
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The newest stable release found on GitHub.
#[derive(Debug, Serialize, ToSchema)]
pub struct LatestReleaseDto {
    /// The release tag, e.g. `v0.2.0`.
    pub version: String,
    pub name: String,
    /// The release page with its notes.
    pub url: String,
    /// RFC 3339.
    pub published_at: Option<String>,
}

/// The last update this data directory saw.
#[derive(Debug, Serialize, ToSchema)]
pub struct LastUpdateDto {
    pub from_version: String,
    pub to_version: String,
    /// RFC 3339: when the new version first started.
    pub at: String,
    /// `in_place`, `helper` or `external`.
    pub method: String,
}

/// Update status.
#[derive(Debug, Serialize, ToSchema)]
pub struct UpdateStatusDto {
    /// The running version.
    pub current_version: String,
    /// `[update] check`: when false, Aegis makes no update requests.
    pub check_enabled: bool,
    /// RFC 3339, the last successful check.
    pub checked_at: Option<String>,
    /// Why the last check failed (checks are retried later).
    pub check_error: Option<String>,
    pub latest: Option<LatestReleaseDto>,
    /// `latest` is newer than the running version.
    pub update_available: bool,
    /// How an update is installed here: `in_place` (Aegis replaces its own
    /// binary), `helper` (a root-owned helper service does), `manual` (an
    /// administrator runs `manual_command`) or `unsupported`.
    pub mode: String,
    /// What applying means on this machine.
    pub mode_detail: String,
    pub manual_command: Option<String>,
    /// Why the available update can't be applied from here.
    pub blocked_reason: Option<String>,
    /// `POST /api/v1/system/update/apply` would be accepted now.
    pub can_apply: bool,
    /// `idle`, `downloading`, `backing_up`, `installing`,
    /// `waiting_for_helper`, `restarting` or `failed`.
    pub phase: String,
    pub phase_error: Option<String>,
    pub last_update: Option<LastUpdateDto>,
}

impl From<UpdateStatus> for UpdateStatusDto {
    fn from(s: UpdateStatus) -> Self {
        let installable = matches!(
            s.mode,
            aegis_update::InstallMode::InPlace | aegis_update::InstallMode::Helper
        );
        let idle = matches!(
            s.phase,
            aegis_update::Phase::Idle | aegis_update::Phase::Failed
        );
        Self {
            can_apply: s.check_enabled
                && s.update_available
                && installable
                && idle
                && s.blocked_reason.is_none(),
            current_version: s.current_version,
            check_enabled: s.check_enabled,
            checked_at: s.checked_at,
            check_error: s.check_error,
            latest: s.latest.map(|r| LatestReleaseDto {
                version: r.tag,
                name: r.name,
                url: r.html_url,
                published_at: r.published_at,
            }),
            update_available: s.update_available,
            mode: s.mode.as_str().to_string(),
            mode_detail: s.mode_detail,
            manual_command: s.manual_command,
            blocked_reason: s.blocked_reason,
            phase: s.phase.as_str().to_string(),
            phase_error: s.phase_error,
            last_update: s.last_update.map(|u| LastUpdateDto {
                from_version: u.from_version,
                to_version: u.to_version,
                at: u.at,
                method: u.method,
            }),
        }
    }
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct ApplyUpdateRequest {
    /// The version the operator confirmed (e.g. `v0.2.0`); refused if the
    /// latest known release is a different one.
    #[serde(default)]
    pub version: Option<String>,
}

/// Records update events and takes the pre-update backup through the
/// control plane.
pub struct ControlUpdateHooks(pub ControlPlane);

#[async_trait::async_trait]
impl UpdateHooks for ControlUpdateHooks {
    async fn record(&self, event_type: &str, payload: serde_json::Value) {
        if let Err(e) = self.0.record(event_type, payload).await {
            tracing::error!(error = %e, event_type, "Failed to record update event");
        }
    }

    async fn backup(&self) -> anyhow::Result<String> {
        // The database, secrets key and certificates; releases stay on disk.
        self.0
            .create_backup(Some(false), "update")
            .await
            .map(|b| b.name)
            .map_err(|e| anyhow::anyhow!("The pre-update backup failed: {}", e))
    }
}

fn not_configured(version: &str) -> UpdateStatusDto {
    UpdateStatusDto {
        current_version: version.to_string(),
        check_enabled: false,
        checked_at: None,
        check_error: None,
        latest: None,
        update_available: false,
        mode: "unsupported".into(),
        mode_detail: "Self-update is not available in this daemon.".into(),
        manual_command: None,
        blocked_reason: None,
        can_apply: false,
        phase: "idle".into(),
        phase_error: None,
        last_update: None,
    }
}

fn update_error(e: UpdateError) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "failed_precondition", e.to_string())
}

#[utoipa::path(get, path = "/api/v1/system/update", tag = "system",
    responses((status = 200, description = "Whether a newer release is available (cached; checked every `[update] interval_hours`)", body = UpdateStatusDto)))]
pub(crate) async fn update_status(State(st): State<AppState>) -> Json<UpdateStatusDto> {
    Json(match &st.updates {
        Some(m) => m.status().into(),
        None => not_configured(&st.version),
    })
}

#[utoipa::path(post, path = "/api/v1/system/update/check", tag = "system",
    responses((status = 200, description = "Checked GitHub now; a failed check is reported in `check_error`", body = UpdateStatusDto),
              (status = 403, description = "Admins only (signed-in users, not API tokens)", body = ErrorBody),
              (status = 409, description = "Update checks are disabled", body = ErrorBody)))]
pub(crate) async fn check_update(State(st): State<AppState>) -> ApiResult<Json<UpdateStatusDto>> {
    let manager = st
        .updates
        .as_ref()
        .ok_or_else(|| update_error(UpdateError::Disabled))?;
    Ok(Json(
        manager.check_now().await.map_err(update_error)?.into(),
    ))
}

#[utoipa::path(post, path = "/api/v1/system/update/apply", tag = "system",
    request_body = ApplyUpdateRequest,
    responses((status = 202, description = "Started: backup, download and verification, binary swap, restart. Follow `phase` in GET /api/v1/system/update; the daemon restarts at the end", body = UpdateStatusDto),
              (status = 403, description = "Admins only (signed-in users, not API tokens)", body = ErrorBody),
              (status = 409, description = "Nothing to apply, already running, checks disabled, or this machine needs the update run by an administrator (see the message)", body = ErrorBody)))]
pub(crate) async fn apply_update(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(req): ApiJson<ApplyUpdateRequest>,
) -> ApiResult<(StatusCode, Json<UpdateStatusDto>)> {
    let manager = st
        .updates
        .as_ref()
        .ok_or_else(|| update_error(UpdateError::Disabled))?;
    let status = manager
        .start_apply(principal.actor(), req.version.as_deref())
        .map_err(update_error)?;
    Ok((StatusCode::ACCEPTED, Json(status.into())))
}
