//! Authentication middleware and the auth, token and webhook endpoints.
//!
//! Two kinds of principal can call `/api/v1`:
//! - a **user** with a session cookie (`aegis_session`: HttpOnly,
//!   SameSite=Strict, Secure over HTTPS). Requests that change state must
//!   also carry the session's CSRF token in `X-CSRF-Token`.
//! - an **API token** (`Authorization: Bearer aegis_...`), scoped `read` or
//!   `deploy`. Tokens can't manage tokens, passwords or webhook secrets,
//!   and can't change domains.
//!
//! Everything a principal does is recorded with `actor` = `user:<name>` or
//! `token:<name>` (see `aegis_control::with_actor`).

use crate::dto::*;
use crate::{ApiError, ApiJson, ApiResult, AppState};
use aegis_auth::{constant_time_eq, Role, TokenScope};
use aegis_control::{with_actor, ControlError};
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde_json::json;
use std::net::{IpAddr, SocketAddr};

pub const SESSION_COOKIE: &str = "aegis_session";
pub(crate) const MAX_REMEMBERED_DELIVERIES: usize = 1000;

#[derive(Debug, Clone)]
pub enum Principal {
    User {
        username: String,
        role: Role,
        csrf_token: String,
        expires_at: i64,
    },
    Token {
        name: String,
        scope: TokenScope,
    },
}

impl Principal {
    pub fn actor(&self) -> String {
        match self {
            Self::User { username, .. } => format!("user:{}", username),
            Self::Token { name, .. } => format!("token:{}", name),
        }
    }
}

/// The secret that authenticated a request, kept so long-lived streams can
/// re-check it.
#[derive(Clone)]
pub enum Credential {
    Token(String),
    Session(String),
}

impl Credential {
    pub async fn still_valid(&self, auth: &aegis_auth::AuthStore) -> bool {
        match self {
            Self::Token(t) => matches!(auth.authenticate_token(t).await, Ok(Some(_))),
            Self::Session(s) => matches!(auth.session(s).await, Ok(Some(_))),
        }
    }
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

async fn authenticate(
    st: &AppState,
    headers: &HeaderMap,
) -> Result<Option<(Principal, Credential)>, ApiError> {
    if let Some(token) = bearer(headers) {
        return Ok(st
            .auth
            .authenticate_token(token)
            .await
            .map_err(ApiError::internal)?
            .map(|info| {
                (
                    Principal::Token {
                        name: info.name,
                        scope: info.scope,
                    },
                    Credential::Token(token.to_string()),
                )
            }));
    }
    if let Some(cookie) = cookie_value(headers, SESSION_COOKIE) {
        return Ok(st
            .auth
            .session(&cookie)
            .await
            .map_err(ApiError::internal)?
            .map(|s| {
                (
                    Principal::User {
                        username: s.username,
                        role: s.role,
                        csrf_token: s.csrf_token,
                        expires_at: s.expires_at,
                    },
                    Credential::Session(cookie),
                )
            }));
    }
    Ok(None)
}

/// Endpoints only a signed-in person may use, never an automation token.
fn user_only(path: &str) -> bool {
    // /api/v1/projects/<project>/<section>/...
    let section = path
        .strip_prefix("/api/v1/projects/")
        .and_then(|rest| rest.split('/').nth(1));
    path.starts_with("/api/v1/tokens")
        || path.starts_with("/api/v1/users")
        || path == "/api/v1/auth/password"
        // Backups hold the secrets key and password hashes.
        || path == "/api/v1/backups"
        || path.starts_with("/api/v1/backups/")
        // Installing a new Aegis binary (and asking GitHub on demand).
        || path.starts_with("/api/v1/system/update/")
        || matches!(section, Some("webhook" | "secrets" | "deploy-key"))
        // A group's webhook secret, like an app's.
        || (path.starts_with("/api/v1/groups/")
            && path.split('/').nth(5) == Some("webhook"))
        // Reading server folders and registering apps in bulk.
        || path == "/api/v1/apps"
        || path.starts_with("/api/v1/apps/")
        // Imports write secrets.
        || (section == Some("env") && path.contains("/env/import"))
}

/// `DELETE /api/v1/projects/<project>`: deleting an app (its releases,
/// secrets and domains) is for a signed-in admin, never an API token.
fn deletes_project(method: &Method, path: &str) -> bool {
    *method == Method::DELETE
        && path
            .strip_prefix("/api/v1/projects/")
            .is_some_and(|rest| !rest.is_empty() && !rest.trim_end_matches('/').contains('/'))
}

/// Mapping a domain decides where public traffic (and certificates) go, so
/// even deploy tokens may only read domains.
fn changes_domains(path: &str) -> bool {
    path.starts_with("/api/v1/projects/")
        && (path.ends_with("/domains") || path.contains("/domains/"))
}

/// The least role a signed-in user needs for a request. Viewers read,
/// deployers also deploy, roll back and start/stop/restart apps, and admins
/// change everything else (projects, settings, secrets, domains, limits,
/// webhooks, tokens, users, backups).
pub(crate) fn required_role(method: &Method, path: &str) -> Role {
    let read = matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    let parts: Vec<&str> = path
        .strip_prefix("/api/v1/")
        .unwrap_or("")
        .split('/')
        .collect();
    match parts.as_slice() {
        // Everyone manages their own session and password.
        ["auth", ..] => Role::Viewer,
        // Credentials and secret material, even to read.
        ["users" | "tokens" | "backups", ..] => Role::Admin,
        ["projects", _, "secrets" | "webhook" | "deploy-key", ..] => Role::Admin,
        ["groups", _, "webhook", ..] => Role::Admin,
        // Scanning server folders, access checks and bulk import.
        ["apps", ..] => Role::Admin,
        // Pasted .env contents, and writes of Variables and Secrets.
        ["projects", _, "env", "import", ..] => Role::Admin,
        // Variable names and their sources (never values).
        ["projects", _, "env"] if read => Role::Viewer,
        _ if read => Role::Viewer,
        // Checking now and installing a new Aegis version.
        ["system", "update", ..] => Role::Admin,
        // Shows the aegis.toml a settings change would produce; saves nothing.
        ["projects", _, "settings", "preview"] => Role::Viewer,
        ["projects", _, "deploy" | "rollback"] => Role::Deployer,
        ["groups", _, "deploy" | "rollback"] => Role::Deployer,
        // "Check now" for new commits: reads the git server, changes nothing.
        ["projects" | "groups", _, "updates", "check"] => Role::Deployer,
        ["processes", _, "start" | "stop" | "restart"] => Role::Deployer,
        _ => Role::Admin,
    }
}

pub async fn require_auth(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let protected = path == "/api/v1" || path.starts_with("/api/v1/");
    if !protected || path == "/api/v1/auth/login" {
        return next.run(req).await;
    }
    let (principal, credential) = match authenticate(&st, req.headers()).await {
        Ok(Some(found)) => found,
        Ok(None) => {
            let mut res = ApiError::unauthenticated(
                "Sign in (POST /api/v1/auth/login) or send Authorization: Bearer <api token>",
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

    let mutating = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    match &principal {
        Principal::Token { scope, .. } => {
            if user_only(&path) || deletes_project(req.method(), &path) {
                return ApiError::forbidden(
                    "This endpoint requires a signed-in user, not an API token",
                )
                .into_response();
            }
            if mutating && changes_domains(&path) {
                return ApiError::forbidden(
                    "Only a signed-in user can change domains, not an API token",
                )
                .into_response();
            }
            if mutating && *scope == TokenScope::Read {
                return ApiError::forbidden("This API token is read-only").into_response();
            }
        }
        Principal::User {
            csrf_token, role, ..
        } => {
            let sent = req
                .headers()
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if mutating && !constant_time_eq(sent, csrf_token) {
                return ApiError::new(
                    StatusCode::FORBIDDEN,
                    "csrf",
                    "Missing or wrong X-CSRF-Token header (get it from GET /api/v1/auth/session)",
                )
                .into_response();
            }
            let needed = required_role(req.method(), &path);
            if *role < needed {
                return ApiError::forbidden(format!(
                    "This needs the {} role; you are a {}",
                    needed.as_str(),
                    role.as_str()
                ))
                .into_response();
            }
        }
    }
    let actor = principal.actor();
    req.extensions_mut().insert(principal);
    req.extensions_mut().insert(credential);
    with_actor(actor, next.run(req)).await
}

/// The client address, taking a local reverse proxy into account.
fn client_ip(st: &AppState, peer: Option<SocketAddr>, headers: &HeaderMap) -> String {
    let peer_ip = peer.map(|p| p.ip());
    if st.settings.trust_proxy && peer_ip.is_some_and(|ip| ip.is_loopback()) {
        // The proxy appends the address it saw; earlier entries are client-supplied.
        if let Some(ip) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .and_then(|v| v.trim().parse::<IpAddr>().ok())
        {
            return ip.to_string();
        }
    }
    peer_ip.map_or_else(|| "unknown".to_string(), |ip| ip.to_string())
}

fn session_cookie(st: &AppState, value: &str, max_age: u64) -> HeaderValue {
    let secure = if st.settings.https { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
        SESSION_COOKIE, value, max_age, secure
    ))
    .expect("cookie is ASCII")
}

/// Records an audit event; failures are logged, not surfaced.
async fn audit(st: &AppState, actor: String, event_type: &str, payload: serde_json::Value) {
    let result = with_actor(actor, st.control.record(event_type, payload)).await;
    if let Err(e) = result {
        tracing::error!(error = %e, event_type, "Failed to record audit event");
    }
}

#[utoipa::path(post, path = "/api/v1/auth/login", tag = "auth", security(()),
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Signed in; sets the aegis_session cookie", body = SessionDto),
        (status = 401, description = "Wrong username or password", body = ErrorBody),
        (status = 429, description = "Too many failed attempts; see Retry-After", body = ErrorBody)))]
pub(crate) async fn login(
    State(st): State<AppState>,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    ApiJson(req): ApiJson<LoginRequest>,
) -> ApiResult<Response> {
    let username: String = req.username.trim().chars().take(64).collect();
    let ip = client_ip(&st, peer.map(|c| c.0), &headers);
    // The attempt is counted before the (expensive) password check, so
    // parallel requests can't exceed the limit.
    if let Err(wait) = st.limiter.begin(&ip, &username) {
        let mut res = ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Too many failed sign-in attempts; try again later",
        )
        .into_response();
        res.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from(wait.as_secs().max(1)),
        );
        return Ok(res);
    }

    let ok = st
        .auth
        .verify_login(&username, &req.password)
        .await
        .map_err(ApiError::internal)?;
    if !ok {
        // Record only real usernames: a password typed into the username
        // field must not end up in the audit log.
        let known = st
            .auth
            .user_exists(&username)
            .await
            .map_err(ApiError::internal)?;
        audit(
            &st,
            "anonymous".into(),
            "UserLoginFailed",
            json!({"username": if known { username.as_str() } else { "(unknown user)" }, "ip": ip}),
        )
        .await;
        return Err(ApiError::unauthenticated("Wrong username or password"));
    }
    st.limiter.record_success(&ip, &username);
    let role = st
        .auth
        .user(&username)
        .await
        .map_err(ApiError::internal)?
        .map(|u| u.role)
        .unwrap_or(Role::Viewer);
    let session = st
        .auth
        .create_session(&username)
        .await
        .map_err(ApiError::internal)?;
    audit(
        &st,
        format!("user:{}", username),
        "UserLoggedIn",
        json!({"username": username, "ip": ip}),
    )
    .await;

    let max_age = st.auth.config().session_absolute.as_secs();
    let mut res = Json(SessionDto {
        kind: "user".into(),
        name: username,
        csrf_token: Some(session.csrf_token),
        expires_at: Some(session.expires_at),
        scope: None,
        role: Some(role.as_str().to_string()),
    })
    .into_response();
    res.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie(&st, &session.token, max_age),
    );
    Ok(res)
}

#[utoipa::path(post, path = "/api/v1/auth/logout", tag = "auth",
    request_body(content = Object, description = "Empty object `{}`"),
    responses((status = 204, description = "Session ended; cookie cleared")))]
pub(crate) async fn logout(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if let (Principal::User { username, .. }, Some(cookie)) =
        (&principal, cookie_value(&headers, SESSION_COOKIE))
    {
        st.auth
            .delete_session(&cookie)
            .await
            .map_err(ApiError::internal)?;
        audit(
            &st,
            principal.actor(),
            "UserLoggedOut",
            json!({"username": username}),
        )
        .await;
    }
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut()
        .insert(header::SET_COOKIE, session_cookie(&st, "", 0));
    Ok(res)
}

#[utoipa::path(get, path = "/api/v1/auth/session", tag = "auth",
    responses((status = 200, description = "Who is calling; includes the CSRF token for sessions", body = SessionDto),
              (status = 401, body = ErrorBody)))]
pub(crate) async fn session_info(Extension(principal): Extension<Principal>) -> Json<SessionDto> {
    Json(match principal {
        Principal::User {
            username,
            role,
            csrf_token,
            expires_at,
        } => SessionDto {
            kind: "user".into(),
            name: username,
            csrf_token: Some(csrf_token),
            expires_at: Some(expires_at),
            scope: None,
            role: Some(role.as_str().to_string()),
        },
        Principal::Token { name, scope } => SessionDto {
            kind: "token".into(),
            name,
            csrf_token: None,
            expires_at: None,
            scope: Some(scope.as_str().to_string()),
            role: None,
        },
    })
}

fn signed_in_user(principal: &Principal) -> ApiResult<&str> {
    match principal {
        Principal::User { username, .. } => Ok(username),
        Principal::Token { .. } => Err(ApiError::forbidden(
            "This endpoint requires a signed-in user, not an API token",
        )),
    }
}

#[utoipa::path(post, path = "/api/v1/auth/password", tag = "auth",
    request_body = ChangePasswordRequest,
    responses(
        (status = 204, description = "Changed; all sessions (including this one) are ended"),
        (status = 400, description = "New password too short", body = ErrorBody),
        (status = 403, description = "Current password is wrong", body = ErrorBody)))]
pub(crate) async fn change_password(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(req): ApiJson<ChangePasswordRequest>,
) -> ApiResult<Response> {
    let username = signed_in_user(&principal)?.to_string();
    if st
        .limiter
        .by_user
        .try_begin(&username.to_lowercase())
        .is_err()
    {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Too many failed attempts; try again later",
        ));
    }
    let ok = st
        .auth
        .verify_login(&username, &req.current_password)
        .await
        .map_err(ApiError::internal)?;
    if !ok {
        return Err(ApiError::forbidden("Current password is incorrect"));
    }
    st.limiter
        .by_user
        .record_success(&[username.to_lowercase()]);
    st.auth
        .set_password(&username, &req.new_password)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    audit(
        &st,
        principal.actor(),
        "PasswordChanged",
        json!({"username": username}),
    )
    .await;
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut()
        .insert(header::SET_COOKIE, session_cookie(&st, "", 0));
    Ok(res)
}

#[utoipa::path(get, path = "/api/v1/tokens", tag = "auth",
    responses((status = 200, body = [TokenDto])))]
pub(crate) async fn list_tokens(State(st): State<AppState>) -> ApiResult<Json<Vec<TokenDto>>> {
    Ok(Json(
        st.auth
            .list_tokens()
            .await
            .map_err(ApiError::internal)?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

#[utoipa::path(post, path = "/api/v1/tokens", tag = "auth",
    request_body = CreateTokenRequest,
    responses((status = 201, description = "The token is shown only in this response", body = NewTokenDto),
              (status = 400, body = ErrorBody)))]
pub(crate) async fn create_token(
    State(st): State<AppState>,
    ApiJson(req): ApiJson<CreateTokenRequest>,
) -> ApiResult<(StatusCode, Json<NewTokenDto>)> {
    let scope = TokenScope::parse(&req.scope)
        .ok_or_else(|| ApiError::bad_request("scope must be \"read\" or \"deploy\""))?;
    let created = st
        .auth
        .create_token(&req.name, scope)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    st.control
        .record(
            "ApiTokenCreated",
            json!({"token_id": created.info.id, "name": created.info.name, "scope": scope.as_str()}),
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(NewTokenDto {
            token: created.token,
            info: created.info.into(),
        }),
    ))
}

#[utoipa::path(delete, path = "/api/v1/tokens/{id}", tag = "auth",
    params(("id" = String, Path, description = "Token id")),
    responses((status = 204, description = "Revoked"), (status = 404, body = ErrorBody)))]
pub(crate) async fn revoke_token(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    if !st
        .auth
        .revoke_token(&id)
        .await
        .map_err(ApiError::internal)?
    {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "No active token with that id",
        ));
    }
    st.control
        .record("ApiTokenRevoked", json!({"token_id": id}))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/v1/projects/{project}/webhook", tag = "webhooks",
    params(("project" = String, Path, description = "Project id or name")),
    request_body(content = Object, description = "Empty object `{}`"),
    responses((status = 200, description = "New secret, shown only in this response; the previous secret stops working", body = WebhookSecretDto)))]
pub(crate) async fn rotate_webhook_secret(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<WebhookSecretDto>> {
    let project = st.control.resolve_project(&key)?;
    let secret = st
        .auth
        .rotate_webhook_secret(&project.id.to_string())
        .await
        .map_err(ApiError::internal)?;
    st.control
        .record(
            "WebhookSecretRotated",
            json!({"project_id": project.id.to_string()}),
        )
        .await?;
    Ok(Json(WebhookSecretDto {
        path: format!("/hooks/github/{}", project.id),
        secret,
        content_type: "application/json".into(),
        events: vec!["push".into()],
    }))
}

pub(crate) fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

pub(crate) fn webhook_result(
    status: StatusCode,
    outcome: &str,
    reason: Option<String>,
) -> (StatusCode, Json<WebhookResult>) {
    (
        status,
        Json(WebhookResult {
            status: outcome.to_string(),
            deployment_id: None,
            reason,
        }),
    )
}

#[utoipa::path(post, path = "/hooks/github/{project}", tag = "webhooks", security(()),
    params(("project" = String, Path, description = "Project id")),
    request_body(content = Object, description = "GitHub webhook payload, signed with X-Hub-Signature-256"),
    responses(
        (status = 202, description = "Push to the project's branch: deployment queued (or event ignored)", body = WebhookResult),
        (status = 200, description = "ping, or a redelivery that was already handled", body = WebhookResult),
        (status = 401, description = "Missing or invalid signature", body = ErrorBody),
        (status = 404, description = "Unknown project or no webhook configured", body = ErrorBody)))]
pub(crate) async fn github_webhook(
    State(st): State<AppState>,
    Path(key): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<WebhookResult>)> {
    let unknown = || ApiError::new(StatusCode::NOT_FOUND, "not_found", "Unknown webhook");
    let project = st.control.resolve_project(&key).map_err(|_| unknown())?;
    let secret = st
        .auth
        .webhook_secret(&project.id.to_string())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(unknown)?;
    let signature = header_str(&headers, "x-hub-signature-256");
    if !aegis_webhook::verify_github_signature(&secret, &body, signature) {
        return Err(ApiError::unauthenticated(
            "Missing or invalid X-Hub-Signature-256",
        ));
    }

    let event = header_str(&headers, "x-github-event").to_string();
    let delivery = header_str(&headers, "x-github-delivery").to_string();
    let result = github_event(&st, &project, &headers, &body).await;
    // A push means new commits: refresh the "new commits" badge.
    if event == "push" {
        st.control.check_updates_soon(project.id);
    }
    // Signed deliveries are recorded so the dashboard can show them.
    let (status, reason, deployment_id) = match &result {
        Ok((_, Json(r))) => (r.status.clone(), r.reason.clone(), r.deployment_id.clone()),
        Err(e) => ("error".to_string(), Some(e.message().to_string()), None),
    };
    let _ = with_actor(
        "webhook:github".into(),
        st.control.record(
            "WebhookReceived",
            json!({
                "project_id": project.id.to_string(),
                "delivery": delivery,
                "event": event,
                "status": status,
                "reason": reason,
                "deployment_id": deployment_id,
            }),
        ),
    )
    .await;
    result
}

/// Handles a delivery whose signature has been verified.
async fn github_event(
    st: &AppState,
    project: &aegis_projection::ProjectState,
    headers: &HeaderMap,
    body: &[u8],
) -> ApiResult<(StatusCode, Json<WebhookResult>)> {
    let delivery = header_str(headers, "x-github-delivery");
    if !delivery.is_empty() {
        let mut seen = st.deliveries.lock().unwrap();
        if seen.iter().any(|d| d == delivery) {
            return Ok(webhook_result(StatusCode::OK, "duplicate", None));
        }
        if seen.len() >= MAX_REMEMBERED_DELIVERIES {
            seen.pop_front();
        }
        seen.push_back(delivery.to_string());
    }

    match header_str(headers, "x-github-event") {
        "ping" => Ok(webhook_result(StatusCode::OK, "pong", None)),
        "push" => {
            let push = aegis_webhook::parse_push(body)
                .map_err(|e| ApiError::bad_request(e.to_string()))?;
            // A group deploys its apps together, from its own webhook: the
            // same push must not also deploy this app on its own.
            if let Some(group) = st.control.group_of(&project.id) {
                return Ok(webhook_result(
                    StatusCode::ACCEPTED,
                    "ignored",
                    Some(format!(
                        "'{}' is in group '{}': pushes deploy it through the group's webhook",
                        project.name, group.name
                    )),
                ));
            }
            if push.deleted || push.branch.as_deref() != Some(project.branch.as_str()) {
                return Ok(webhook_result(
                    StatusCode::ACCEPTED,
                    "ignored",
                    Some(format!("Only pushes to '{}' deploy", project.branch)),
                ));
            }
            // A replayed old push (same signed body, new delivery id) must not
            // roll the app back to a commit that was already deployed.
            if st
                .control
                .list_releases(&project.id)
                .iter()
                .any(|r| r.commit_sha == push.after)
            {
                return Ok(webhook_result(
                    StatusCode::OK,
                    "ignored",
                    Some("This commit has already been deployed".into()),
                ));
            }
            if project.repository_url.is_empty() {
                return Err(ControlError::FailedPrecondition(
                    "Project has no repository_url to clone; set one to deploy from webhooks"
                        .into(),
                )
                .into());
            }
            // Always clone the project's configured repository, never a URL
            // from the payload.
            let deployment_id = with_actor(
                "webhook:github".into(),
                st.control.queue_deployment(aegis_control::DeployRequest {
                    project_id: project.id,
                    branch: Some(project.branch.clone()),
                    repository_url: Some(project.repository_url.clone()),
                    commit: Some(push.after.clone()),
                    trigger: "github-push".into(),
                    ..Default::default()
                }),
            )
            .await?;
            Ok((
                StatusCode::ACCEPTED,
                Json(WebhookResult {
                    status: "queued".into(),
                    deployment_id: Some(deployment_id.to_string()),
                    reason: None,
                }),
            ))
        }
        other => Ok(webhook_result(
            StatusCode::ACCEPTED,
            "ignored",
            Some(format!("Event '{}' is not handled", other)),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_needed_per_endpoint() {
        let need = |m: Method, p: &str| required_role(&m, p);
        // Reading is for everyone, except credentials and secret material.
        for p in [
            "/api/v1/projects",
            "/api/v1/projects/web/releases",
            "/api/v1/events/stream",
            "/api/v1/processes/web/logs",
            "/api/v1/projects/web/domains",
            "/api/v1/projects/web/env",
            "/api/v1/system/update",
        ] {
            assert_eq!(need(Method::GET, p), Role::Viewer, "{p}");
        }
        for p in [
            "/api/v1/users",
            "/api/v1/tokens",
            "/api/v1/backups",
            "/api/v1/projects/web/secrets",
            "/api/v1/projects/web/webhook",
            "/api/v1/projects/web/deploy-key",
            "/api/v1/apps/scan",
        ] {
            assert_eq!(need(Method::GET, p), Role::Admin, "{p}");
        }
        assert_eq!(need(Method::POST, "/api/v1/auth/logout"), Role::Viewer);
        assert_eq!(need(Method::POST, "/api/v1/auth/password"), Role::Viewer);
        assert_eq!(
            need(Method::POST, "/api/v1/projects/web/settings/preview"),
            Role::Viewer
        );
        // Day-to-day operations.
        assert_eq!(
            need(Method::POST, "/api/v1/projects/web/deploy"),
            Role::Deployer
        );
        assert_eq!(
            need(Method::POST, "/api/v1/projects/web/rollback"),
            Role::Deployer
        );
        for a in ["start", "stop", "restart"] {
            assert_eq!(
                need(Method::POST, &format!("/api/v1/processes/web/{a}")),
                Role::Deployer
            );
        }
        // Everything else that changes state.
        for (m, p) in [
            (Method::POST, "/api/v1/projects"),
            (Method::PUT, "/api/v1/projects/web/settings"),
            (Method::PUT, "/api/v1/projects/web/resources"),
            (Method::PUT, "/api/v1/projects/web/schedule"),
            (Method::POST, "/api/v1/projects/web/domains"),
            (Method::PUT, "/api/v1/projects/web/secrets/A"),
            (Method::POST, "/api/v1/projects/web/env/import/preview"),
            (Method::POST, "/api/v1/projects/web/env/import"),
            (Method::POST, "/api/v1/users"),
            (Method::POST, "/api/v1/detect"),
            (Method::POST, "/api/v1/processes/web/unknown"),
            (Method::POST, "/api/v1/system/update/check"),
            (Method::POST, "/api/v1/system/update/apply"),
        ] {
            assert_eq!(need(m.clone(), p), Role::Admin, "{m} {p}");
        }
        // Imports write secrets: signed-in users only.
        assert!(user_only("/api/v1/projects/web/env/import/preview"));
        assert!(user_only("/api/v1/projects/web/env/import"));
        assert!(!user_only("/api/v1/projects/web/env"));
        // Deploy keys, scans and imports: admins, signed in.
        for (m, p) in [
            (Method::POST, "/api/v1/projects/web/deploy-key"),
            (Method::DELETE, "/api/v1/projects/web/deploy-key"),
            (Method::POST, "/api/v1/projects/web/deploy-key/test"),
            (Method::POST, "/api/v1/apps/scan"),
            (Method::POST, "/api/v1/apps/import"),
            (Method::POST, "/api/v1/apps/check-access"),
        ] {
            assert_eq!(need(m.clone(), p), Role::Admin, "{m} {p}");
            assert!(user_only(p), "{p}");
        }
        assert!(user_only("/api/v1/projects/web/deploy-key"));
        // Deleting an app: admins, signed in. Changing its source: admins.
        assert_eq!(need(Method::DELETE, "/api/v1/projects/web"), Role::Admin);
        assert_eq!(
            need(Method::PUT, "/api/v1/projects/web/source"),
            Role::Admin
        );
        assert!(deletes_project(&Method::DELETE, "/api/v1/projects/web"));
        assert!(deletes_project(&Method::DELETE, "/api/v1/projects/web/"));
        assert!(!deletes_project(&Method::GET, "/api/v1/projects/web"));
        assert!(!deletes_project(
            &Method::DELETE,
            "/api/v1/projects/web/domains/a.example"
        ));
        assert!(!deletes_project(&Method::DELETE, "/api/v1/projects/"));
        // Groups: everyone reads, deployers deploy and roll back, admins
        // change them; the webhook secret is for signed-in admins.
        assert_eq!(need(Method::GET, "/api/v1/groups"), Role::Viewer);
        assert_eq!(need(Method::GET, "/api/v1/groups/ty"), Role::Viewer);
        assert_eq!(need(Method::GET, "/api/v1/groups/ty/webhook"), Role::Admin);
        assert_eq!(
            need(Method::POST, "/api/v1/groups/ty/deploy"),
            Role::Deployer
        );
        assert_eq!(
            need(Method::POST, "/api/v1/groups/ty/rollback"),
            Role::Deployer
        );
        for (m, p) in [
            (Method::POST, "/api/v1/groups"),
            (Method::PUT, "/api/v1/groups/ty"),
            (Method::DELETE, "/api/v1/groups/ty"),
            (Method::PUT, "/api/v1/groups/ty/members"),
            (Method::POST, "/api/v1/groups/ty/members"),
            (Method::DELETE, "/api/v1/groups/ty/members/web"),
            (Method::POST, "/api/v1/groups/ty/webhook"),
        ] {
            assert_eq!(need(m.clone(), p), Role::Admin, "{m} {p}");
        }
        assert!(user_only("/api/v1/groups/ty/webhook"));
        assert!(user_only("/api/v1/groups/ty/webhook/test"));
        assert!(!user_only("/api/v1/groups/ty/deploy"));
        assert_eq!(
            need(Method::GET, "/api/v1/projects/web/updates"),
            Role::Viewer
        );
        assert_eq!(
            need(Method::POST, "/api/v1/projects/web/updates/check"),
            Role::Deployer
        );
        assert_eq!(
            need(Method::POST, "/api/v1/groups/ty/updates/check"),
            Role::Deployer
        );
    }
}
