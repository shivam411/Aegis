//! Where apps' code comes from: per-app SSH deploy keys, repository access
//! checks, and "Scan a folder on the server" with bulk import. Admins only,
//! and signed-in users only (not API tokens): see `auth::required_role` and
//! `auth::user_only`.

use crate::{ApiError, ApiJson, ApiResult, AppState};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use utoipa::ToSchema;

/// A deploy key's public half. The private key is never returned.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DeployKeyDto {
    /// One line, `ssh-ed25519 AAAA... aegis:<app>`: add it to the
    /// repository's deploy keys (read-only).
    pub public_key: String,
    /// `SHA256:...`, as GitHub shows it.
    pub fingerprint: String,
    pub created_at: String,
    /// The URL deployments clone with this key (GitHub HTTPS URLs are
    /// converted to SSH); `null` when the app's source can't use a key.
    pub clone_url: Option<String>,
    /// Where to add the key, for GitHub repositories.
    pub add_key_url: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct CreateDeployKeyRequest {
    /// Replace an existing key (it stops working once removed from the
    /// repository). Without it, an existing key makes this a 409.
    pub replace: bool,
}

/// Whether a repository could be read (`git ls-remote`).
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AccessCheckDto {
    pub ok: bool,
    /// The URL that was checked.
    pub url: String,
    pub with_deploy_key: bool,
    /// Branches seen (when `ok`).
    pub branches: Vec<String>,
    /// Whether the app's branch exists (when known).
    pub branch_found: Option<bool>,
    /// git's error message (never key material).
    pub error: Option<String>,
    /// What to do next.
    pub hint: Option<String>,
}

impl From<aegis_control::AccessCheck> for AccessCheckDto {
    fn from(c: aegis_control::AccessCheck) -> Self {
        Self {
            ok: c.ok,
            url: c.url,
            with_deploy_key: c.with_deploy_key,
            branches: c.branches,
            branch_found: c.branch_found,
            error: c.error,
            hint: c.hint,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct CheckAccessRequest {
    /// A GitHub repository URL (any form).
    pub repository_url: String,
    pub branch: Option<String>,
}

/// Whether scanning is on, and where.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ScanInfoDto {
    /// `[apps] import_dirs` plus `[apps] env_file_dirs`.
    pub allowed_dirs: Vec<String>,
    /// Env files are on (`[apps] env_file_dirs` is set).
    pub env_files_enabled: bool,
}

#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ScanRequest {
    /// An absolute path inside one of the allowed folders.
    pub path: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ScanWarningDto {
    /// `private_repo`, `ssh_remote`, `credentials_in_url`, `port_in_use`,
    /// `hardcoded_port`, `env_unreadable`, `env_outside_allowed`,
    /// `no_remote` or `detached_head`.
    pub code: String,
    pub message: String,
}

/// A start script that fixes the app's port, and its `$PORT` form.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct StartHintDto {
    pub port: u16,
    /// The package.json `start` script.
    pub script: String,
    /// The proposed start command, e.g. `npx next start -p $PORT`.
    pub suggested_start: String,
}

impl From<aegis_engine::StartHint> for StartHintDto {
    fn from(h: aegis_engine::StartHint) -> Self {
        Self {
            port: h.port,
            script: h.script,
            suggested_start: h.suggested_start,
        }
    }
}

/// An app found by a scan, with the settings an import would use.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ScanCandidateDto {
    pub path: String,
    /// The git checkout the app is in.
    pub repo_root: Option<String>,
    /// The app's folder inside the checkout (monorepo apps).
    pub source_subdir: Option<String>,
    pub name: String,
    pub runtime: String,
    pub install_command: Option<String>,
    pub build_command: Option<String>,
    pub start_command: String,
    pub port: Option<u16>,
    pub health_check_url: Option<String>,
    /// The checkout's `origin` remote (credentials removed).
    pub repository_url: Option<String>,
    pub branch: Option<String>,
    /// `github`, `ssh`, `https` or `none`.
    pub remote_kind: String,
    /// `required` (SSH remote), `check` (GitHub over HTTPS: may be
    /// private), `unknown` or `no`.
    pub deploy_key: String,
    /// The app's `.env`, if there is one.
    pub env_file: Option<String>,
    pub env_file_readable: bool,
    /// Variable names in the `.env` (values are never read out).
    pub env_names: Vec<String>,
    /// The `.env` may be used as the Env file (it is in `env_file_dirs`).
    pub env_file_allowed: bool,
    pub start_hint: Option<StartHintDto>,
    /// What listens on the app's port right now.
    pub port_in_use: Option<String>,
    /// The registered app with the same source (skip it).
    pub registered_as: Option<String>,
    pub warnings: Vec<ScanWarningDto>,
}

impl From<aegis_control::ScanCandidate> for ScanCandidateDto {
    fn from(c: aegis_control::ScanCandidate) -> Self {
        Self {
            path: c.path,
            repo_root: c.repo_root,
            source_subdir: c.source_subdir,
            name: c.name,
            runtime: c.runtime,
            install_command: c.install_command,
            build_command: c.build_command,
            start_command: c.start_command,
            port: c.port,
            health_check_url: c.health_check_url,
            repository_url: c.repository_url,
            branch: c.branch,
            remote_kind: c.remote_kind.to_string(),
            deploy_key: c.deploy_key.to_string(),
            env_file: c.env_file,
            env_file_readable: c.env_file_readable,
            env_names: c.env_names,
            env_file_allowed: c.env_file_allowed,
            start_hint: c.start_hint.map(Into::into),
            port_in_use: c.port_in_use,
            registered_as: c.registered_as,
            warnings: c
                .warnings
                .into_iter()
                .map(|w| ScanWarningDto {
                    code: w.code.to_string(),
                    message: w.message,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ScanResultDto {
    /// The scanned folder, resolved.
    pub root: String,
    pub candidates: Vec<ScanCandidateDto>,
    /// The scan stopped at its depth or entry limit.
    pub truncated: bool,
    /// Folders not looked into, and why.
    pub skipped: Vec<String>,
}

/// One app to import. Give a `repository_url` (cloned at each deployment)
/// or a `source_dir` (copied at each deployment).
#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ImportAppRequest {
    pub name: String,
    pub repository_url: Option<String>,
    pub branch: Option<String>,
    pub source_dir: Option<String>,
    /// Build and run only this folder of the source (monorepo apps).
    pub source_subdir: Option<String>,
    pub runtime: Option<String>,
    /// Dashboard settings to save with the app (start command, port, env
    /// file, ...); omitted fields follow aegis.toml and detection.
    pub settings: Option<crate::dto::ProjectSettingsDto>,
}

#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ImportRequest {
    pub apps: Vec<ImportAppRequest>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ImportedAppDto {
    pub name: String,
    /// Set when the app was registered.
    pub project_id: Option<String>,
    pub error: Option<String>,
    /// What to do before the first deployment.
    pub next_steps: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ImportResultDto {
    /// One entry per requested app, in order. Imported apps are not deployed.
    pub apps: Vec<ImportedAppDto>,
}

fn key_dto(
    project: &aegis_projection::ProjectState,
    info: aegis_control::DeployKeyInfo,
) -> DeployKeyDto {
    let url = &project.repository_url;
    DeployKeyDto {
        public_key: info.public_key,
        fingerprint: info.fingerprint,
        created_at: info.created_at,
        clone_url: (!url.is_empty())
            .then(|| aegis_ssh::url_for_deploy_key(url))
            .flatten(),
        add_key_url: aegis_ssh::github_deploy_keys_page(url),
    }
}

#[utoipa::path(get, path = "/api/v1/projects/{project}/deploy-key", tag = "sources",
    params(("project" = String, Path, description = "Project id or name")),
    responses((status = 200, description = "The public key (the private key is never returned)", body = DeployKeyDto),
              (status = 404, description = "No deploy key", body = crate::dto::ErrorBody)))]
pub(crate) async fn get_deploy_key(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<DeployKeyDto>> {
    let project = st.control.resolve_project(&key)?;
    let info = st.control.deploy_key(&project.id).ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "This app has no deploy key",
        )
    })?;
    Ok(Json(key_dto(&project, info)))
}

#[utoipa::path(post, path = "/api/v1/projects/{project}/deploy-key", tag = "sources",
    params(("project" = String, Path, description = "Project id or name")),
    request_body = CreateDeployKeyRequest,
    responses((status = 201, description = "Generated (Ed25519); add the public key to the repository, read-only", body = DeployKeyDto),
              (status = 409, description = "The app already has a key; send replace: true", body = crate::dto::ErrorBody)))]
pub(crate) async fn create_deploy_key(
    State(st): State<AppState>,
    Path(key): Path<String>,
    ApiJson(req): ApiJson<CreateDeployKeyRequest>,
) -> ApiResult<(StatusCode, Json<DeployKeyDto>)> {
    let project = st.control.resolve_project(&key)?;
    let info = st
        .control
        .create_deploy_key(project.id, req.replace)
        .await?;
    Ok((StatusCode::CREATED, Json(key_dto(&project, info))))
}

#[utoipa::path(delete, path = "/api/v1/projects/{project}/deploy-key", tag = "sources",
    params(("project" = String, Path, description = "Project id or name")),
    responses((status = 204, description = "Deleted; also remove it from the repository's deploy keys"),
              (status = 404, body = crate::dto::ErrorBody)))]
pub(crate) async fn delete_deploy_key(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<StatusCode> {
    let project = st.control.resolve_project(&key)?;
    st.control.delete_deploy_key(project.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/v1/projects/{project}/deploy-key/test", tag = "sources",
    params(("project" = String, Path, description = "Project id or name")),
    request_body(content = Object, description = "Empty object `{}`"),
    responses((status = 200, description = "`git ls-remote` with the key; `ok` false carries git's error and a hint", body = AccessCheckDto),
              (status = 409, description = "No deploy key, or the source can't use one", body = crate::dto::ErrorBody)))]
pub(crate) async fn test_deploy_key(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<AccessCheckDto>> {
    let project = st.control.resolve_project(&key)?;
    Ok(Json(st.control.test_deploy_key(project.id).await?.into()))
}

#[utoipa::path(post, path = "/api/v1/apps/check-access", tag = "sources",
    request_body = CheckAccessRequest,
    responses((status = 200, description = "Whether the GitHub repository can be read without credentials (`ok` false: private, or doesn't exist)", body = AccessCheckDto),
              (status = 400, description = "Not a GitHub repository URL", body = crate::dto::ErrorBody)))]
pub(crate) async fn check_access(
    State(st): State<AppState>,
    ApiJson(req): ApiJson<CheckAccessRequest>,
) -> ApiResult<Json<AccessCheckDto>> {
    let branch = crate::non_empty(req.branch);
    Ok(Json(
        st.control
            .check_public_access(&req.repository_url, branch.as_deref())
            .await?
            .into(),
    ))
}

#[utoipa::path(get, path = "/api/v1/apps/scan", tag = "sources",
    responses((status = 200, description = "The folders scans may look in (empty: scanning is off)", body = ScanInfoDto)))]
pub(crate) async fn scan_info(State(st): State<AppState>) -> Json<ScanInfoDto> {
    Json(ScanInfoDto {
        allowed_dirs: st
            .control
            .import_dirs()
            .iter()
            .map(|d| d.display().to_string())
            .collect(),
        env_files_enabled: st.control.env_files_enabled(),
    })
}

#[utoipa::path(post, path = "/api/v1/apps/scan", tag = "sources",
    request_body = ScanRequest,
    responses((status = 200, description = "Apps found (at most 2 levels deep); nothing is changed", body = ScanResultDto),
              (status = 400, description = "Not inside an allowed folder", body = crate::dto::ErrorBody),
              (status = 409, description = "Scanning is off", body = crate::dto::ErrorBody)))]
pub(crate) async fn scan(
    State(st): State<AppState>,
    ApiJson(req): ApiJson<ScanRequest>,
) -> ApiResult<Json<ScanResultDto>> {
    let result = st.control.scan_folder(&req.path).await?;
    Ok(Json(ScanResultDto {
        root: result.root,
        candidates: result.candidates.into_iter().map(Into::into).collect(),
        truncated: result.truncated,
        skipped: result.skipped,
    }))
}

#[utoipa::path(post, path = "/api/v1/apps/import", tag = "sources",
    request_body = ImportRequest,
    responses((status = 200, description = "Registered (not deployed); per-app errors and next steps", body = ImportResultDto),
              (status = 400, body = crate::dto::ErrorBody)))]
pub(crate) async fn import(
    State(st): State<AppState>,
    ApiJson(req): ApiJson<ImportRequest>,
) -> ApiResult<Json<ImportResultDto>> {
    let apps = req
        .apps
        .into_iter()
        .map(|a| aegis_control::ImportApp {
            name: a.name,
            repository_url: crate::non_empty(a.repository_url),
            branch: crate::non_empty(a.branch),
            source_dir: crate::non_empty(a.source_dir).map(PathBuf::from),
            source_subdir: crate::non_empty(a.source_subdir),
            runtime: crate::non_empty(a.runtime),
            settings: a.settings.map(Into::into).unwrap_or_default(),
        })
        .collect();
    let results = st.control.import_apps(apps).await?;
    Ok(Json(ImportResultDto {
        apps: results
            .into_iter()
            .map(|r| ImportedAppDto {
                name: r.name,
                project_id: r.project_id,
                error: r.error,
                next_steps: r.next_steps,
            })
            .collect(),
    }))
}
