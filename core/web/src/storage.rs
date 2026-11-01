//! Disk use under the data directory, cleanup now, and optional automatic
//! cleanup (hourly: old builds, logs older than N days, old backups and
//! temporary files). Admins only; API tokens can't use these.

use crate::dto::ErrorBody;
use crate::{ApiError, ApiJson, ApiResult, AppState};
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use utoipa::ToSchema;

const AUTO: &str = "storage_auto";
const LOG_DAYS: &str = "storage_log_days";

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct StorageCategoryDto {
    /// releases, old_builds, logs, backups, database or temp.
    pub key: String,
    pub label: String,
    pub bytes: u64,
    /// What "Clean up now" would free in this category.
    pub reclaimable_bytes: u64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AppStorageDto {
    pub project_id: String,
    pub name: String,
    pub releases_bytes: u64,
    pub old_builds_bytes: u64,
    pub logs_bytes: u64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct StorageDto {
    pub total_bytes: u64,
    pub reclaimable_bytes: u64,
    pub categories: Vec<StorageCategoryDto>,
    pub apps: Vec<AppStorageDto>,
    /// Hourly automatic cleanup is on.
    pub auto_cleanup: bool,
    /// Log files untouched for this many days are removed by a cleanup.
    pub log_days: u64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CleanupDto {
    pub freed_bytes: u64,
    /// Bytes freed per target.
    pub freed: std::collections::BTreeMap<String, u64>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CleanupRequest {
    /// Any of old_builds, logs, backups, temp. Empty: all of them.
    #[serde(default)]
    pub targets: Vec<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AutoCleanupRequest {
    pub enabled: Option<bool>,
    /// 1 to 3650.
    pub log_days: Option<u64>,
}

async fn log_days(st: &AppState) -> u64 {
    st.auth
        .setting(LOG_DAYS)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(aegis_control::DEFAULT_LOG_DAYS)
}

async fn auto_on(st: &AppState) -> bool {
    st.auth.setting(AUTO).await.ok().flatten().as_deref() == Some("1")
}

async fn report(st: &AppState) -> ApiResult<StorageDto> {
    let days = log_days(st).await;
    let r = st.control.storage_report(days).await?;
    Ok(StorageDto {
        total_bytes: r.total_bytes,
        reclaimable_bytes: r.reclaimable_bytes,
        categories: r
            .categories
            .into_iter()
            .map(|c| StorageCategoryDto {
                key: c.key,
                label: c.label,
                bytes: c.bytes,
                reclaimable_bytes: c.reclaimable_bytes,
            })
            .collect(),
        apps: r
            .apps
            .into_iter()
            .map(|a| AppStorageDto {
                project_id: a.project_id,
                name: a.name,
                releases_bytes: a.releases_bytes,
                old_builds_bytes: a.old_builds_bytes,
                logs_bytes: a.logs_bytes,
            })
            .collect(),
        auto_cleanup: auto_on(st).await,
        log_days: days,
    })
}

#[utoipa::path(get, path = "/api/v1/system/storage", tag = "system",
    responses((status = 200, body = StorageDto), (status = 403, body = ErrorBody)))]
pub(crate) async fn storage(State(st): State<AppState>) -> ApiResult<Json<StorageDto>> {
    Ok(Json(report(&st).await?))
}

#[utoipa::path(post, path = "/api/v1/system/storage/cleanup", tag = "system",
    request_body = CleanupRequest,
    responses((status = 200, body = CleanupDto), (status = 400, body = ErrorBody),
              (status = 403, body = ErrorBody)))]
pub(crate) async fn cleanup(
    State(st): State<AppState>,
    ApiJson(req): ApiJson<CleanupRequest>,
) -> ApiResult<Json<CleanupDto>> {
    let all: Vec<String> = aegis_control::CLEANUP_TARGETS
        .iter()
        .map(|s| s.to_string())
        .collect();
    let targets = if req.targets.is_empty() {
        all.clone()
    } else {
        req.targets
    };
    if let Some(bad) = targets.iter().find(|t| !all.contains(t)) {
        return Err(ApiError::bad_request(format!(
            "Unknown target '{}'; use old_builds, logs, backups or temp",
            bad
        )));
    }
    let days = log_days(&st).await;
    let result = st.control.storage_cleanup(&targets, days, "manual").await?;
    Ok(Json(CleanupDto {
        freed_bytes: result.freed_bytes,
        freed: result.removed.into_iter().collect(),
    }))
}

#[utoipa::path(put, path = "/api/v1/system/storage/auto", tag = "system",
    request_body = AutoCleanupRequest,
    responses((status = 200, body = StorageDto), (status = 400, body = ErrorBody),
              (status = 403, body = ErrorBody)))]
pub(crate) async fn set_auto(
    State(st): State<AppState>,
    ApiJson(req): ApiJson<AutoCleanupRequest>,
) -> ApiResult<Json<StorageDto>> {
    if let Some(days) = req.log_days {
        if !(1..=3650).contains(&days) {
            return Err(ApiError::bad_request("log_days must be between 1 and 3650"));
        }
        st.auth
            .set_setting(LOG_DAYS, &days.to_string())
            .await
            .map_err(ApiError::internal)?;
    }
    if let Some(on) = req.enabled {
        st.auth
            .set_setting(AUTO, if on { "1" } else { "0" })
            .await
            .map_err(ApiError::internal)?;
    }
    st.control
        .record(
            "StorageAutoCleanupChanged",
            json!({"enabled": auto_on(&st).await, "log_days": log_days(&st).await}),
        )
        .await?;
    Ok(Json(report(&st).await?))
}

/// Starts the hourly automatic cleanup. It does nothing until an admin turns
/// it on, and stops when the daemon shuts down.
pub fn spawn_storage_janitor(st: AppState) {
    let mut stop = st.shutdown.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        tick.tick().await;
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = stop.wait_for(|s| *s) => return,
            }
            if auto_on(&st).await {
                let targets: Vec<String> = aegis_control::CLEANUP_TARGETS
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                let days = log_days(&st).await;
                if let Err(e) = st.control.storage_cleanup(&targets, days, "auto").await {
                    tracing::warn!(error = %e, "Automatic cleanup failed");
                }
            }
        }
    });
}
