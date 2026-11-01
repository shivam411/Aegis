//! The server's Docker containers and their logs. Separate from app access:
//! only the owner (the first admin) and users the owner granted it to may
//! look. API tokens can't.

use crate::auth::Principal;
use crate::dto::ErrorBody;
use crate::{ApiError, ApiResult, AppState};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ContainerDto {
    pub id: String,
    pub name: String,
    pub image: String,
    /// running, exited, paused, created, restarting, ...
    pub state: String,
    /// For example "Up 3 hours".
    pub status: String,
    pub ports: String,
    pub created: String,
    /// Started by this Aegis for one of its apps.
    pub managed: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ContainersDto {
    /// False when Docker isn't installed or can't be reached; see `reason`.
    pub available: bool,
    pub reason: Option<String>,
    pub containers: Vec<ContainerDto>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ContainerLogLineDto {
    pub timestamp: String,
    /// "out" or "err".
    pub stream: String,
    pub line: String,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ContainerLogsQuery {
    /// Lines to return, newest last (default 300, at most 5000).
    pub lines: Option<usize>,
    /// Only output since this time (RFC 3339) or age (for example 10m).
    pub since: Option<String>,
}

async fn require_docker(st: &AppState, principal: &Principal) -> ApiResult<()> {
    let allowed = match principal {
        Principal::User { username, .. } => st
            .auth
            .has_docker_access(username)
            .await
            .map_err(ApiError::internal)?,
        _ => false,
    };
    if allowed {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "Docker is not shared with your account; ask the owner to allow it",
        ))
    }
}

#[utoipa::path(get, path = "/api/v1/docker/containers", tag = "docker",
    responses((status = 200, body = ContainersDto),
              (status = 403, description = "No Docker access", body = ErrorBody)))]
pub(crate) async fn list_containers(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Json<ContainersDto>> {
    require_docker(&st, &principal).await?;
    Ok(Json(match st.control.host_containers().await {
        Ok(list) => ContainersDto {
            available: true,
            reason: None,
            containers: list
                .into_iter()
                .map(|c| ContainerDto {
                    id: c.id,
                    name: c.name,
                    image: c.image,
                    state: c.state,
                    status: c.status,
                    ports: c.ports,
                    created: c.created,
                    managed: c.managed,
                })
                .collect(),
        },
        Err(e) => ContainersDto {
            available: false,
            reason: Some(e.to_string()),
            containers: Vec::new(),
        },
    }))
}

#[utoipa::path(get, path = "/api/v1/docker/containers/{id}/logs", tag = "docker",
    params(("id" = String, Path, description = "Container id or name"), ContainerLogsQuery),
    responses((status = 200, body = [ContainerLogLineDto]),
              (status = 403, description = "No Docker access", body = ErrorBody),
              (status = 404, body = ErrorBody)))]
pub(crate) async fn container_logs(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    Query(q): Query<ContainerLogsQuery>,
) -> ApiResult<Json<Vec<ContainerLogLineDto>>> {
    require_docker(&st, &principal).await?;
    let lines = q.lines.unwrap_or(300).clamp(1, 5000);
    let (out, err) = st
        .control
        .container_logs(&id, lines, q.since.as_deref())
        .await
        .map_err(|e| ApiError::new(StatusCode::NOT_FOUND, "not_found", e.to_string()))?;
    let parse = |text: &str, stream: &str| -> Vec<ContainerLogLineDto> {
        text.lines()
            .map(|l| {
                let (ts, rest) = l.split_once(' ').unwrap_or(("", l));
                ContainerLogLineDto {
                    timestamp: ts.to_string(),
                    stream: stream.to_string(),
                    line: rest.to_string(),
                }
            })
            .collect()
    };
    let mut all = parse(&out, "out");
    all.extend(parse(&err, "err"));
    // RFC 3339 timestamps sort as text.
    all.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    let skip = all.len().saturating_sub(lines);
    Ok(Json(all.into_iter().skip(skip).collect()))
}
