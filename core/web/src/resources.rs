//! Resource limits and metrics endpoints.

use crate::api::{json_event, until_shutdown, SseStream};
use crate::auth::Credential;
use crate::dto::*;
use crate::{non_empty, ApiError, ApiJson, ApiResult, AppState};
use aegis_config::ResourceLimits;
use axum::extract::{Path, Query, State};
use axum::response::sse::Sse;
use axum::{Extension, Json};
use futures_util::stream::StreamExt as _;
use serde::Deserialize;
use std::time::Duration;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use utoipa::IntoParams;

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MetricsQuery {
    /// How far back: e.g. `5m`, `15m` (default), `1h`, `6h`, `24h`, `7d`.
    /// Up to an hour uses 2-second samples; longer uses per-minute rollups.
    pub range: Option<String>,
    /// Maximum number of points (default 300, max 2000).
    pub points: Option<usize>,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MetricsStreamQuery {
    /// Only this project (id or name); every app and the host when omitted.
    pub project: Option<String>,
    /// With `project`, also send host samples.
    #[serde(default)]
    pub host: bool,
}

/// "15m" -> 15 minutes. Units: s, m, h, d; at most 31 days.
pub(crate) fn parse_range(text: &str) -> Result<Duration, ApiError> {
    let text = text.trim();
    let bad = || {
        ApiError::bad_request(format!(
            "Invalid range '{}'; use e.g. 15m, 1h, 24h or 7d",
            text
        ))
    };
    let (number, unit) = text.split_at(text.find(|c: char| !c.is_ascii_digit()).ok_or_else(bad)?);
    let n: u64 = number.parse().map_err(|_| bad())?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        _ => return Err(bad()),
    };
    if secs == 0 || secs > 31 * 86400 {
        return Err(bad());
    }
    Ok(Duration::from_secs(secs))
}

fn overview(st: &AppState) -> ResourceCapabilitiesDto {
    st.control.resource_overview().into()
}

fn project_resources(st: &AppState, key: &str) -> ApiResult<ProjectResourcesDto> {
    let project = st.control.resolve_project(key)?;
    let running = project
        .current_process
        .and_then(|id| st.control.supervisor().get(&id))
        .is_some_and(|p| p.status == aegis_process::ProcessStatus::Running);
    Ok(ProjectResourcesDto {
        project_id: project.id.to_string(),
        limits: project.resources.into(),
        usage: running
            .then(|| st.control.latest_sample(&project.id.to_string()))
            .flatten()
            .map(Into::into),
        capabilities: overview(st),
    })
}

#[utoipa::path(get, path = "/api/v1/resources", tag = "resources",
    responses((status = 200, description = "Whether this host can enforce limits, and how much is committed", body = ResourceCapabilitiesDto)))]
pub(crate) async fn capabilities(State(st): State<AppState>) -> Json<ResourceCapabilitiesDto> {
    Json(overview(&st))
}

#[utoipa::path(get, path = "/api/v1/projects/{project}/resources", tag = "resources",
    params(("project" = String, Path, description = "Project id or name")),
    responses((status = 200, body = ProjectResourcesDto), (status = 404, body = ErrorBody)))]
pub(crate) async fn get_resources(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<ProjectResourcesDto>> {
    Ok(Json(project_resources(&st, &key)?))
}

#[utoipa::path(put, path = "/api/v1/projects/{project}/resources", tag = "resources",
    params(("project" = String, Path, description = "Project id or name")),
    request_body = SetResourcesRequest,
    responses(
        (status = 200, description = "Applied to the running app at once (no restart) and to every later start", body = ProjectResourcesDto),
        (status = 400, description = "Out of range", body = ErrorBody),
        (status = 409, description = "`confirmation_required`: repeat with `force: true`; `failed_precondition`: not supported on this host or refused by the kernel", body = ErrorBody)))]
pub(crate) async fn set_resources(
    State(st): State<AppState>,
    Path(key): Path<String>,
    ApiJson(req): ApiJson<SetResourcesRequest>,
) -> ApiResult<Json<ProjectResourcesDto>> {
    let project = st.control.resolve_project(&key)?;
    let limits = ResourceLimits {
        cpu_cores: req.cpu_cores,
        memory_bytes: req.memory_bytes,
        pids_max: req.pids_max,
    };
    st.control
        .set_resource_limits(project.id, limits, req.force)
        .await?;
    Ok(Json(project_resources(&st, &project.id.to_string())?))
}

async fn metrics_for(
    st: &AppState,
    series: String,
    q: MetricsQuery,
) -> ApiResult<Json<MetricsDto>> {
    let range = match non_empty(q.range) {
        Some(r) => parse_range(&r)?,
        None => Duration::from_secs(15 * 60),
    };
    let points = q.points.unwrap_or(300).clamp(1, 2000);
    let points = st.control.metrics(&series, range, points).await?;
    Ok(Json(MetricsDto {
        series,
        range_secs: range.as_secs(),
        points: points.into_iter().map(Into::into).collect(),
    }))
}

#[utoipa::path(get, path = "/api/v1/projects/{project}/metrics", tag = "resources",
    params(("project" = String, Path, description = "Project id or name"), MetricsQuery),
    responses((status = 200, description = "CPU and memory history with the limits in effect", body = MetricsDto),
              (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub(crate) async fn project_metrics(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<MetricsQuery>,
) -> ApiResult<Json<MetricsDto>> {
    let project = st.control.resolve_project(&key)?;
    metrics_for(&st, project.id.to_string(), q).await
}

#[utoipa::path(get, path = "/api/v1/host/metrics", tag = "resources",
    params(MetricsQuery),
    responses((status = 200, description = "Host CPU (in cores) and memory history", body = MetricsDto)))]
pub(crate) async fn host_metrics(
    State(st): State<AppState>,
    Query(q): Query<MetricsQuery>,
) -> ApiResult<Json<MetricsDto>> {
    metrics_for(&st, aegis_control::HOST_SERIES.to_string(), q).await
}

#[derive(Debug, serde::Serialize)]
struct MetricsUpdateDto {
    series: String,
    sample: SampleDto,
}

#[utoipa::path(get, path = "/api/v1/metrics/stream", tag = "resources",
    params(MetricsStreamQuery),
    responses((status = 200, content_type = "text/event-stream",
        description = "`sample` messages, `{\"series\": <project id or \"host\">, \"sample\": SampleDto}`, every sample interval (2 s by default)")))]
pub(crate) async fn stream_metrics(
    State(st): State<AppState>,
    Extension(credential): Extension<Credential>,
    Query(q): Query<MetricsStreamQuery>,
) -> ApiResult<Sse<SseStream>> {
    let project = match non_empty(q.project) {
        Some(key) => Some(st.control.resolve_project(&key)?.id.to_string()),
        None => None,
    };
    let include_host = q.host;
    let updates = BroadcastStream::new(st.control.subscribe_metrics())
        .filter_map(move |item| {
            let project = project.clone();
            async move {
                match item {
                    Ok(update) => {
                        let wanted = match &project {
                            None => true,
                            Some(p) => {
                                update.series == *p
                                    || (include_host && update.series == aegis_control::HOST_SERIES)
                            }
                        };
                        wanted.then(|| {
                            Ok(json_event(
                                "sample",
                                None,
                                &MetricsUpdateDto {
                                    series: update.series,
                                    sample: update.sample.into(),
                                },
                            ))
                        })
                    }
                    Err(BroadcastStreamRecvError::Lagged(_)) => None,
                }
            }
        })
        .boxed();
    Ok(until_shutdown(&st, credential, updates))
}

#[cfg(test)]
mod tests {
    use super::parse_range;
    use std::time::Duration;

    #[test]
    fn test_parse_range() {
        assert_eq!(parse_range("15m").unwrap(), Duration::from_secs(900));
        assert_eq!(parse_range("7d").unwrap(), Duration::from_secs(7 * 86400));
        assert_eq!(parse_range("90s").unwrap(), Duration::from_secs(90));
        for bad in ["", "m", "15", "0m", "1y", "32d", "-5m", "1.5h"] {
            assert!(parse_range(bad).is_err(), "{bad}");
        }
    }
}
