//! Backups: list, create, download and delete archives in
//! `<data_dir>/backups`. Restores are offline only (`aegis-daemon admin
//! restore`, with the daemon stopped), so there is no restore endpoint.
//!
//! Backups contain the secrets key and password hashes, so these endpoints
//! are for signed-in users only, never API tokens (see `auth::user_only`).

use crate::dto::ErrorBody;
use crate::{ApiError, ApiJson, ApiResult, AppState};
use aegis_control::backup::BackupInfo;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A backup archive.
#[derive(Debug, Serialize, ToSchema)]
pub struct BackupDto {
    /// File name, e.g. `aegis-backup-20261016T030000000Z.tar.gz`.
    pub name: String,
    /// Where the archive is on the server (for `aegis-daemon admin restore`).
    pub path: String,
    pub size_bytes: u64,
    /// RFC 3339.
    pub created_at: String,
    /// False for database-and-keys-only backups.
    pub includes_releases: bool,
    /// The Aegis version that made it.
    pub aegis_version: String,
    pub file_count: usize,
    /// Set when the archive can't be read (it can still be deleted).
    pub problem: Option<String>,
}

impl From<BackupInfo> for BackupDto {
    fn from(b: BackupInfo) -> Self {
        Self {
            name: b.name,
            path: b.path.to_string_lossy().to_string(),
            size_bytes: b.size_bytes,
            created_at: b.created_at,
            includes_releases: b.includes_releases,
            aegis_version: b.aegis_version,
            file_count: b.file_count,
            problem: b.problem,
        }
    }
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct CreateBackupRequest {
    /// Include release directories. Defaults to `[backup] include_releases`.
    #[serde(default)]
    pub include_releases: Option<bool>,
}

#[utoipa::path(get, path = "/api/v1/backups", tag = "backups",
    responses((status = 200, description = "Backups, newest first", body = [BackupDto]),
              (status = 403, description = "API tokens can't manage backups", body = ErrorBody)))]
pub(crate) async fn list_backups(State(st): State<AppState>) -> ApiResult<Json<Vec<BackupDto>>> {
    Ok(Json(
        st.control
            .list_backups()?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

#[utoipa::path(post, path = "/api/v1/backups", tag = "backups",
    request_body = CreateBackupRequest,
    responses((status = 201, description = "Created (older backups beyond `[backup] keep` are deleted)", body = BackupDto),
              (status = 403, description = "API tokens can't manage backups", body = ErrorBody),
              (status = 500, description = "The backup failed (recorded as a BackupFailed event)", body = ErrorBody)))]
pub(crate) async fn create_backup(
    State(st): State<AppState>,
    ApiJson(req): ApiJson<CreateBackupRequest>,
) -> ApiResult<(StatusCode, Json<BackupDto>)> {
    let info = st
        .control
        .create_backup(req.include_releases, "manual")
        .await?;
    Ok((StatusCode::CREATED, Json(info.into())))
}

#[utoipa::path(get, path = "/api/v1/backups/{name}/download", tag = "backups",
    params(("name" = String, Path, description = "Backup file name")),
    responses((status = 200, description = "The archive (application/gzip)", content_type = "application/gzip", body = Vec<u8>),
              (status = 400, description = "Not a backup name", body = ErrorBody),
              (status = 404, body = ErrorBody)))]
pub(crate) async fn download_backup(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Response> {
    let path = st.control.backup_file(&name)?;
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(ApiError::internal)?;
    let len = file.metadata().await.map_err(ApiError::internal)?.len();
    let body = Body::from_stream(tokio_util::io::ReaderStream::new(file));
    // The name is validated (letters, digits, '-', '.'), so it needs no quoting.
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{}\"", name))
        .map_err(ApiError::internal)?;
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/gzip"),
            ),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CONTENT_LENGTH, HeaderValue::from(len)),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        body,
    )
        .into_response())
}

#[utoipa::path(delete, path = "/api/v1/backups/{name}", tag = "backups",
    params(("name" = String, Path, description = "Backup file name")),
    responses((status = 204, description = "Deleted"),
              (status = 400, description = "Not a backup name", body = ErrorBody),
              (status = 404, body = ErrorBody)))]
pub(crate) async fn delete_backup(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    st.control.delete_backup(&name).await?;
    Ok(StatusCode::NO_CONTENT)
}
