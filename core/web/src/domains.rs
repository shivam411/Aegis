//! Domains and the built-in reverse proxy.
//!
//! Anyone signed in (and read/deploy tokens) can list domains and see the
//! proxy status; only signed-in users can map or unmap domains (see
//! `auth::require_auth`), since a domain decides where public traffic and
//! certificates go.

use crate::dto::ErrorBody;
use crate::{non_empty, ApiJson, ApiResult, AppState};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The certificate state of a domain.
#[derive(Debug, Serialize, ToSchema)]
pub struct CertStatusDto {
    /// "valid", "expiring" (renewal due), "expired", "pending" (being
    /// requested), "error", "disabled" (ACME is off) or "off" (HTTP only).
    pub state: String,
    /// When the served certificate expires (RFC 3339).
    pub not_after: Option<String>,
    pub message: Option<String>,
}

impl From<aegis_ingress::CertStatus> for CertStatusDto {
    fn from(c: aegis_ingress::CertStatus) -> Self {
        Self {
            state: c.state,
            not_after: c.not_after,
            message: c.message,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DomainDto {
    /// Lowercase hostname.
    pub domain: String,
    /// "acme", "files" or "off".
    pub tls: String,
    pub cert_path: Option<String>,
    pub key_path: Option<String>,
    pub created_at: String,
    /// Absent while the proxy is disabled.
    pub cert: Option<CertStatusDto>,
}

impl From<aegis_control::DomainView> for DomainDto {
    fn from(d: aegis_control::DomainView) -> Self {
        Self {
            domain: d.domain,
            tls: d.tls,
            cert_path: d.cert_path,
            key_path: d.key_path,
            created_at: d.created_at,
            cert: d.cert.map(Into::into),
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct MapDomainRequest {
    /// e.g. "app.example.com"; no wildcards.
    pub domain: String,
    /// "acme" (automatic certificate, default), "files" or "off".
    pub tls: Option<String>,
    /// PEM certificate chain on the server, for tls = "files".
    pub cert_path: Option<String>,
    /// PEM private key on the server, for tls = "files".
    pub key_path: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProxyRouteDto {
    pub domain: String,
    /// "project" or "dashboard".
    pub target: String,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    /// Local port requests go to right now (the first instance's).
    pub upstream_port: Option<u16>,
    /// Every instance's port; requests are spread over them.
    pub upstream_ports: Vec<u16>,
    pub cert: CertStatusDto,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProxyStatusDto {
    /// Whether the built-in proxy runs (`[proxy] enabled`).
    pub enabled: bool,
    pub host: Option<String>,
    pub http_port: Option<u16>,
    pub https_port: Option<u16>,
    /// Automatic certificates (`[proxy.acme]`).
    pub acme_enabled: bool,
    pub acme_email: Option<String>,
    pub acme_directory: Option<String>,
    pub routes: Vec<ProxyRouteDto>,
}

#[utoipa::path(get, path = "/api/v1/projects/{project}/domains", tag = "domains",
    params(("project" = String, Path, description = "Project id or name")),
    responses((status = 200, body = [DomainDto]), (status = 404, body = ErrorBody)))]
pub(crate) async fn list_domains(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Vec<DomainDto>>> {
    let project = st.control.resolve_project(&key)?;
    Ok(Json(
        st.control
            .list_domains(&project.id)?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

#[utoipa::path(post, path = "/api/v1/projects/{project}/domains", tag = "domains",
    params(("project" = String, Path, description = "Project id or name")),
    request_body = MapDomainRequest,
    responses(
        (status = 201, description = "Mapped. With tls = \"acme\", the certificate is requested in the background; the domain's DNS must point to this server and port 80 must be reachable.", body = DomainDto),
        (status = 400, description = "Invalid domain, TLS mode or certificate files", body = ErrorBody),
        (status = 403, description = "API tokens can't change domains", body = ErrorBody),
        (status = 409, description = "The domain belongs to another app", body = ErrorBody)))]
pub(crate) async fn map_domain(
    State(st): State<AppState>,
    Path(key): Path<String>,
    ApiJson(req): ApiJson<MapDomainRequest>,
) -> ApiResult<(StatusCode, Json<DomainDto>)> {
    let project = st.control.resolve_project(&key)?;
    let tls = non_empty(req.tls).unwrap_or_else(|| "acme".into());
    let view = st
        .control
        .map_domain(project.id, &req.domain, &tls, req.cert_path, req.key_path)
        .await?;
    Ok((StatusCode::CREATED, Json(view.into())))
}

#[utoipa::path(delete, path = "/api/v1/projects/{project}/domains/{domain}", tag = "domains",
    params(
        ("project" = String, Path, description = "Project id or name"),
        ("domain" = String, Path, description = "The mapped domain")),
    responses(
        (status = 204, description = "No longer served"),
        (status = 403, description = "API tokens can't change domains", body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub(crate) async fn unmap_domain(
    State(st): State<AppState>,
    Path((key, domain)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let project = st.control.resolve_project(&key)?;
    st.control.unmap_domain(project.id, &domain).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/api/v1/proxy", tag = "domains",
    responses((status = 200, description = "Listeners, routes and certificates of the built-in proxy", body = ProxyStatusDto)))]
pub(crate) async fn proxy_status(State(st): State<AppState>) -> Json<ProxyStatusDto> {
    let Some(status) = st.control.proxy_status() else {
        return Json(ProxyStatusDto {
            enabled: false,
            host: None,
            http_port: None,
            https_port: None,
            acme_enabled: false,
            acme_email: None,
            acme_directory: None,
            routes: Vec::new(),
        });
    };
    let name_of = |id: &str| {
        id.parse()
            .ok()
            .and_then(|id| st.control.projection().get_project(&id))
            .map(|p| p.name)
    };
    Json(ProxyStatusDto {
        enabled: true,
        host: Some(status.host),
        http_port: Some(status.http_port),
        https_port: Some(status.https_port),
        acme_enabled: status.acme_enabled,
        acme_email: status.acme_email,
        acme_directory: status.acme_directory,
        routes: status
            .routes
            .into_iter()
            .map(|r| ProxyRouteDto {
                project_name: r.project_id.as_deref().and_then(name_of),
                domain: r.domain,
                target: r.target,
                project_id: r.project_id,
                upstream_port: r.upstream_port,
                upstream_ports: r.upstream_ports,
                cert: r.cert.into(),
            })
            .collect(),
    })
}
