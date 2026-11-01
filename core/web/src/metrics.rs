//! Prometheus metrics and the matching Grafana dashboard, both on demand:
//! `GET /metrics` answers 404 until an admin turns it on (Account →
//! Monitoring, or `PUT /api/v1/system/prometheus`). It needs an API token
//! (any scope) or a session, like the rest of the API.

use crate::auth::authenticate;
use crate::dto::ErrorBody;
use crate::{ApiError, ApiJson, ApiResult, AppState};
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fmt::Write as _;
use utoipa::ToSchema;

const SETTING: &str = "prometheus_enabled";
const GRAFANA_DASHBOARD: &str = include_str!("../assets/grafana-dashboard.json");

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PrometheusDto {
    /// Whether `GET /metrics` answers.
    pub enabled: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetPrometheusRequest {
    pub enabled: bool,
}

async fn enabled(st: &AppState) -> Result<bool, ApiError> {
    Ok(st
        .auth
        .setting(SETTING)
        .await
        .map_err(ApiError::internal)?
        .as_deref()
        == Some("1"))
}

#[utoipa::path(get, path = "/api/v1/system/prometheus", tag = "system",
    responses((status = 200, body = PrometheusDto)))]
pub(crate) async fn prometheus_status(
    State(st): State<AppState>,
) -> ApiResult<Json<PrometheusDto>> {
    Ok(Json(PrometheusDto {
        enabled: enabled(&st).await?,
    }))
}

#[utoipa::path(put, path = "/api/v1/system/prometheus", tag = "system",
    request_body = SetPrometheusRequest,
    responses((status = 200, body = PrometheusDto), (status = 403, body = ErrorBody)))]
pub(crate) async fn set_prometheus(
    State(st): State<AppState>,
    ApiJson(req): ApiJson<SetPrometheusRequest>,
) -> ApiResult<Json<PrometheusDto>> {
    st.auth
        .set_setting(SETTING, if req.enabled { "1" } else { "0" })
        .await
        .map_err(ApiError::internal)?;
    st.control
        .record("PrometheusToggled", json!({"enabled": req.enabled}))
        .await?;
    Ok(Json(PrometheusDto {
        enabled: req.enabled,
    }))
}

#[utoipa::path(get, path = "/api/v1/system/grafana-dashboard", tag = "system",
    responses((status = 200, content_type = "application/json",
        description = "A Grafana dashboard (import it in Grafana: Dashboards → New → Import) for the metrics at /metrics")))]
pub(crate) async fn grafana_dashboard() -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"aegis-grafana-dashboard.json\"",
            ),
        ],
        GRAFANA_DASHBOARD,
    )
        .into_response()
}

/// Escapes a label value for the Prometheus text format.
fn label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

struct Out(String);

impl Out {
    fn family(&mut self, name: &str, kind: &str, help: &str) {
        let _ = writeln!(self.0, "# HELP {name} {help}\n# TYPE {name} {kind}");
    }

    fn sample(&mut self, name: &str, labels: &[(&str, &str)], value: impl std::fmt::Display) {
        if labels.is_empty() {
            let _ = writeln!(self.0, "{name} {value}");
        } else {
            let labels: Vec<String> = labels
                .iter()
                .map(|(k, v)| format!("{k}=\"{}\"", label(v)))
                .collect();
            let _ = writeln!(self.0, "{name}{{{}}} {value}", labels.join(","));
        }
    }
}

/// `GET /metrics` in the Prometheus text format.
pub(crate) async fn prometheus_metrics(State(st): State<AppState>, headers: HeaderMap) -> Response {
    match enabled(&st).await {
        Ok(true) => {}
        Ok(false) => {
            return ApiError::new(StatusCode::NOT_FOUND, "not_found", "No such endpoint")
                .into_response()
        }
        Err(e) => return e.into_response(),
    }
    let vis = match authenticate(&st, &headers).await {
        Ok(Some((principal, credential))) => {
            // A signed-in user sees the metrics of their own apps only.
            match crate::access::visibility_for(&st, &principal, &credential).await {
                Ok(vis) => vis,
                Err(e) => return e.into_response(),
            }
        }
        Ok(None) => {
            let mut res = ApiError::unauthenticated(
                "Send Authorization: Bearer <api token> (create one under Account → API tokens)",
            )
            .into_response();
            res.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"aegis\""),
            );
            return res;
        }
        Err(e) => return e.into_response(),
    };
    let mut out = Out(String::new());
    let views: Vec<_> = st
        .control
        .list_projects()
        .into_iter()
        .filter(|v| !vis.hides(&v.project.id.to_string()))
        .collect();

    out.family("aegis_build_info", "gauge", "Aegis version.");
    out.sample("aegis_build_info", &[("version", &st.version)], 1);
    out.family("aegis_apps", "gauge", "Registered apps.");
    out.sample("aegis_apps", &[], views.len());
    if let Ok(count) = st.control.store().get_event_count().await {
        out.family(
            "aegis_events_total",
            "counter",
            "Events recorded in the audit log.",
        );
        out.sample("aegis_events_total", &[], count);
    }

    out.family(
        "aegis_app_up",
        "gauge",
        "1 while the app's process is running.",
    );
    out.family(
        "aegis_app_restarts",
        "gauge",
        "Restarts of the app's current process.",
    );
    out.family(
        "aegis_app_instances",
        "gauge",
        "Instances serving the live release.",
    );
    out.family(
        "aegis_app_cpu_cores",
        "gauge",
        "CPU in use, in cores (all instances).",
    );
    out.family(
        "aegis_app_memory_bytes",
        "gauge",
        "Memory in use (all instances).",
    );
    out.family(
        "aegis_app_cpu_limit_cores",
        "gauge",
        "CPU limit in cores, when set.",
    );
    out.family(
        "aegis_app_memory_limit_bytes",
        "gauge",
        "Memory limit, when set.",
    );
    out.family(
        "aegis_app_cpu_throttled_ratio",
        "gauge",
        "Share of CPU periods held back by the limit, 0-1.",
    );
    out.family("aegis_app_threads", "gauge", "Threads of the app.");
    out.family(
        "aegis_app_open_fds",
        "gauge",
        "Open file descriptors of the app.",
    );
    for v in &views {
        let app = v.project.name.as_str();
        let l = [("app", app)];
        let running = v.process.as_ref().is_some_and(|p| p.status == "Running");
        out.sample("aegis_app_up", &l, u8::from(running));
        if let Some(p) = &v.process {
            out.sample("aegis_app_restarts", &l, p.restart_count);
        }
        out.sample(
            "aegis_app_instances",
            &l,
            v.instances.len().max(usize::from(v.process.is_some())),
        );
        if let Some(u) = &v.usage {
            out.sample("aegis_app_cpu_cores", &l, u.cpu_cores);
            out.sample("aegis_app_memory_bytes", &l, u.memory_bytes);
            if let Some(limit) = u.cpu_limit_cores {
                out.sample("aegis_app_cpu_limit_cores", &l, limit);
            }
            if let Some(limit) = u.memory_limit_bytes {
                out.sample("aegis_app_memory_limit_bytes", &l, limit);
            }
            out.sample("aegis_app_cpu_throttled_ratio", &l, u.throttled_ratio);
            out.sample("aegis_app_threads", &l, u.threads);
            out.sample("aegis_app_open_fds", &l, u.fds);
        }
    }

    // Deployments by outcome, per app.
    out.family(
        "aegis_deployments",
        "gauge",
        "Deployments recorded, by app and status.",
    );
    let mut counts: std::collections::BTreeMap<(String, String), u64> = Default::default();
    for d in st
        .control
        .list_deployments(None)
        .into_iter()
        .filter(|d| !vis.hides(&d.project_id.to_string()))
    {
        let name = views
            .iter()
            .find(|v| v.project.id == d.project_id)
            .map_or_else(|| d.project_id.to_string(), |v| v.project.name.clone());
        *counts.entry((name, d.status.clone())).or_default() += 1;
    }
    for ((app, status), n) in &counts {
        out.sample("aegis_deployments", &[("app", app), ("status", status)], n);
    }

    let monitor = st.control.host().clone();
    if let Ok(host) = tokio::task::spawn_blocking(move || monitor.latest()).await {
        out.family("aegis_host_cpu_percent", "gauge", "Host CPU use, 0-100.");
        out.sample("aegis_host_cpu_percent", &[], host.cpu_percent);
        out.family("aegis_host_load", "gauge", "Host load average.");
        for (window, v) in ["1m", "5m", "15m"].iter().zip(host.load_average) {
            out.sample("aegis_host_load", &[("window", window)], v);
        }
        out.family(
            "aegis_host_memory_used_bytes",
            "gauge",
            "Host memory in use.",
        );
        out.sample("aegis_host_memory_used_bytes", &[], host.memory_used_bytes);
        out.family("aegis_host_memory_total_bytes", "gauge", "Host memory.");
        out.sample(
            "aegis_host_memory_total_bytes",
            &[],
            host.memory_total_bytes,
        );
        out.family(
            "aegis_host_disk_total_bytes",
            "gauge",
            "Size of the data directory's filesystem.",
        );
        out.sample("aegis_host_disk_total_bytes", &[], host.disk_total_bytes);
        out.family(
            "aegis_host_disk_available_bytes",
            "gauge",
            "Free space on the data directory's filesystem.",
        );
        out.sample(
            "aegis_host_disk_available_bytes",
            &[],
            host.disk_available_bytes,
        );
        out.family("aegis_host_uptime_seconds", "gauge", "Host uptime.");
        out.sample("aegis_host_uptime_seconds", &[], host.uptime_secs);
    }

    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        out.0,
    )
        .into_response()
}
