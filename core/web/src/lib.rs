//! HTTP/JSON API for the Aegis control plane, with Server-Sent Events for
//! live events and logs. Every handler is a thin adapter over
//! [`aegis_control::ControlPlane`], the same layer the gRPC API uses.
//!
//! Every `/api/v1` route except `POST /api/v1/auth/login` requires a signed-in
//! user (session cookie + CSRF header for changes) or an API token. GitHub
//! webhooks under `/hooks` are authenticated by their HMAC signature.

mod api;
mod auth;
pub mod dto;
mod guard;
pub mod tls;

use aegis_auth::{AuthStore, LoginThrottle};
use aegis_control::{ControlError, ControlPlane};
use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, FromRequest, Request};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use dto::*;
use std::collections::VecDeque;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

pub use auth::Principal;

/// Deployment-specific HTTP settings (from `[web]` in the daemon config).
#[derive(Debug, Clone)]
pub struct WebSettings {
    /// Public hostnames accepted in the Host header, besides loopback names.
    pub allowed_hosts: Vec<String>,
    /// Clients connect over HTTPS (TLS here or at a reverse proxy):
    /// cookies get `Secure` and responses get HSTS.
    pub https: bool,
    /// Trust `X-Forwarded-For` from a loopback peer (the local proxy).
    pub trust_proxy: bool,
    /// How often open event/log streams re-check their credential.
    pub stream_auth_recheck: std::time::Duration,
}

impl Default for WebSettings {
    fn default() -> Self {
        Self {
            allowed_hosts: Vec::new(),
            https: false,
            trust_proxy: false,
            stream_auth_recheck: std::time::Duration::from_secs(15),
        }
    }
}

/// Shared state for all handlers.
#[derive(Clone)]
pub struct AppState {
    pub control: ControlPlane,
    pub auth: AuthStore,
    pub limiter: Arc<LoginThrottle>,
    pub settings: Arc<WebSettings>,
    pub version: String,
    /// Flips to `true` on daemon shutdown so long-lived streams end.
    pub shutdown: watch::Receiver<bool>,
    /// Recently seen webhook delivery ids, to ignore redeliveries.
    pub deliveries: Arc<Mutex<VecDeque<String>>>,
}

impl AppState {
    pub fn new(
        control: ControlPlane,
        auth: AuthStore,
        settings: WebSettings,
        version: String,
        shutdown: watch::Receiver<bool>,
    ) -> Self {
        Self {
            control,
            auth,
            limiter: Arc::new(LoginThrottle::default()),
            settings: Arc::new(settings),
            version,
            shutdown,
            deliveries: Arc::new(Mutex::new(VecDeque::new())),
        }
    }
}

// ----------------------------------------------------------------------
// Errors
// ----------------------------------------------------------------------

/// An error rendered as `{"error": {"code": ..., "message": ...}}`.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_argument", message)
    }

    pub(crate) fn unauthenticated(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthenticated", message)
    }

    pub(crate) fn internal(error: impl std::fmt::Display) -> Self {
        tracing::error!(error = %error, "Internal error in HTTP handler");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "Internal error; see the daemon log",
        )
    }
}

impl From<ControlError> for ApiError {
    fn from(e: ControlError) -> Self {
        match e {
            ControlError::NotFound(m) => Self::new(StatusCode::NOT_FOUND, "not_found", m),
            ControlError::InvalidArgument(m) => Self::bad_request(m),
            ControlError::FailedPrecondition(m) => {
                Self::new(StatusCode::CONFLICT, "failed_precondition", m)
            }
            ControlError::Internal(e) => Self::internal(e),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: ErrorDetail {
                    code: self.code.to_string(),
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}

pub(crate) type ApiResult<T> = Result<T, ApiError>;

/// `Json<T>` whose rejections use the API error format.
pub struct ApiJson<T>(pub T);

#[axum::async_trait]
impl<S, T> FromRequest<S> for ApiJson<T>
where
    Json<T>: FromRequest<S, Rejection = JsonRejection>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(ApiError::bad_request(rejection.body_text())),
        }
    }
}

pub(crate) fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

// ----------------------------------------------------------------------
// OpenAPI, router, server
// ----------------------------------------------------------------------

struct SecuritySchemes;

impl Modify for SecuritySchemes {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "token",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .description(Some("API token created with POST /api/v1/tokens"))
                    .build(),
            ),
        );
        components.add_security_scheme(
            "session",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                "aegis_session",
                "Session cookie from POST /api/v1/auth/login. Requests that change state also need the X-CSRF-Token header.",
            ))),
        );
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Aegis HTTP API",
        version = "1",
        description = "Control Aegis deployments over HTTP. Authenticate with a session (POST /api/v1/auth/login; send X-CSRF-Token on changes) or an API token (Authorization: Bearer). Requests that change state must send `Content-Type: application/json`."
    ),
    modifiers(&SecuritySchemes),
    security(("session" = []), ("token" = [])),
    paths(
        api::status, api::list_projects, api::create_project, api::get_project, api::deploy,
        api::rollback, api::set_schedule, api::list_releases, api::list_deployments,
        api::get_deployment, api::list_processes, api::process_action, api::get_logs,
        api::stream_logs, api::list_events, api::stream_events,
        auth::login, auth::logout, auth::session_info, auth::change_password,
        auth::list_tokens, auth::create_token, auth::revoke_token,
        auth::rotate_webhook_secret, auth::github_webhook
    ),
    components(schemas(
        StatusDto, ProjectDto, ScheduleDto, ProcessDto, ReleaseDto, DeploymentDto, EventDto,
        LogLineDto, LogsDto, CreateProjectRequest, DeployRequest, DeployAccepted,
        RollbackRequest, RollbackResult, ScheduleRequest, ErrorBody, ErrorDetail,
        LoginRequest, SessionDto, ChangePasswordRequest, CreateTokenRequest, TokenDto,
        NewTokenDto, WebhookSecretDto, WebhookResult
    )),
    tags(
        (name = "auth"), (name = "system"), (name = "projects"), (name = "deployments"),
        (name = "processes"), (name = "events"), (name = "webhooks")
    )
)]
pub struct ApiDoc;

async fn openapi() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

async fn not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "not_found", "No such endpoint")
}

async fn healthz() -> &'static str {
    "ok"
}

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/session", get(auth::session_info))
        .route("/auth/password", post(auth::change_password))
        .route("/tokens", get(auth::list_tokens).post(auth::create_token))
        .route("/tokens/:id", delete(auth::revoke_token))
        .route("/status", get(api::status))
        .route(
            "/projects",
            get(api::list_projects).post(api::create_project),
        )
        .route("/projects/:project", get(api::get_project))
        .route("/projects/:project/deploy", post(api::deploy))
        .route("/projects/:project/rollback", post(api::rollback))
        .route("/projects/:project/schedule", put(api::set_schedule))
        .route(
            "/projects/:project/webhook",
            post(auth::rotate_webhook_secret),
        )
        .route("/projects/:project/releases", get(api::list_releases))
        .route("/deployments", get(api::list_deployments))
        .route("/deployments/:id", get(api::get_deployment))
        .route("/processes", get(api::list_processes))
        .route("/processes/:target/logs", get(api::get_logs))
        .route("/processes/:target/logs/stream", get(api::stream_logs))
        .route("/processes/:target/:action", post(api::process_action))
        .route("/events", get(api::list_events))
        .route("/events/stream", get(api::stream_events))
        .route("/openapi.json", get(openapi))
        .fallback(not_found);
    let hooks = Router::new()
        .route("/github/:project", post(auth::github_webhook))
        .layer(DefaultBodyLimit::max(5 * 1024 * 1024));
    Router::new()
        .route("/healthz", get(healthz))
        .nest("/api/v1", api)
        .nest("/hooks", hooks)
        .fallback(not_found)
        // Layers run outermost-last-added: headers on every response
        // (including rejections), then Host/Origin/content-type checks,
        // then authentication.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            guard::guard,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            guard::security_headers,
        ))
        .with_state(state)
}

/// Serves the API over plain HTTP until `shutdown` resolves.
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: AppState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}
