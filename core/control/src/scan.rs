//! "Scan a folder on the server": finds apps in existing checkouts (for
//! example a server that ran them with PM2) and imports them without
//! deploying.
//!
//! Only directories allowed by the daemon configuration are scanned
//! (`[apps] import_dirs` and `[apps] env_file_dirs`, after resolving symbolic
//! links). The walk is bounded ([`MAX_DEPTH`] levels, [`MAX_ENTRIES`]
//! directory entries), skips dependency and build folders, and never follows
//! a symbolic link out of the allowed directories.
//!
//! What is read, read-only: the files the runtime detector looks at
//! (`package.json`, `aegis.toml`, `Cargo.toml`, ...), `.git/config` and
//! `.git/HEAD` (parsed directly, so git's "dubious ownership" check for
//! root-owned checkouts doesn't matter), and the *names* in a `.env` file.
//! `.env` values are never returned.

use crate::{ControlError, ControlPlane, ControlResult, RegisterProject};
use aegis_engine::{ResolvedProjectConfig, StartHint};
use serde_json::json;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

/// How deep below the scanned folder apps are looked for.
pub const MAX_DEPTH: usize = 2;
/// Upper bound for directory entries looked at in one scan.
pub const MAX_ENTRIES: usize = 5000;
/// Upper bound for apps listed by one scan.
pub const MAX_CANDIDATES: usize = 200;
/// Upper bound for one import.
pub const MAX_IMPORT: usize = 100;

/// Folders that never hold an app of their own.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".output",
    ".svelte-kit",
    ".turbo",
    ".cache",
    "venv",
    ".venv",
    "env",
    "__pycache__",
    "vendor",
    "coverage",
];

/// Files that make a directory an app candidate.
const MARKERS: &[&str] = &[
    "aegis.toml",
    "package.json",
    "pyproject.toml",
    "requirements.txt",
    "Cargo.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "Dockerfile",
];

/// Where monorepos keep their apps.
const WORKSPACE_DIRS: &[&str] = &["apps", "packages", "services"];

/// A problem or suggestion for one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanWarning {
    /// `private_repo`, `ssh_remote`, `credentials_in_url`, `port_in_use`,
    /// `hardcoded_port`, `env_unreadable`, `env_outside_allowed`,
    /// `no_remote`, `detached_head` or `unreadable`.
    pub code: &'static str,
    pub message: String,
}

/// An app found by a scan, with the settings an import would use.
#[derive(Debug, Clone)]
pub struct ScanCandidate {
    /// The app's directory.
    pub path: String,
    /// The checkout it belongs to (where `.git` is).
    pub repo_root: Option<String>,
    /// The app's directory relative to the checkout, for monorepo apps.
    pub source_subdir: Option<String>,
    pub name: String,
    pub runtime: String,
    pub install_command: Option<String>,
    pub build_command: Option<String>,
    pub start_command: String,
    pub port: Option<u16>,
    pub health_check_url: Option<String>,
    /// `origin` (or the only remote), without credentials.
    pub repository_url: Option<String>,
    /// The checked-out branch.
    pub branch: Option<String>,
    /// `github`, `ssh`, `https` or `none`.
    pub remote_kind: &'static str,
    /// Cloning will need a deploy key (an SSH remote), or might (GitHub
    /// HTTPS: check with the access check).
    pub deploy_key: &'static str,
    /// The app's `.env`, if there is one.
    pub env_file: Option<String>,
    pub env_file_readable: bool,
    /// Variable names in the `.env` (never values).
    pub env_names: Vec<String>,
    /// Whether the `.env` may be used as the app's Env file (inside
    /// `[apps] env_file_dirs`).
    pub env_file_allowed: bool,
    pub start_hint: Option<StartHint>,
    /// Something already listens on `port` (e.g. the current deployment).
    pub port_in_use: Option<String>,
    /// The registered app this checkout already is.
    pub registered_as: Option<String>,
    pub warnings: Vec<ScanWarning>,
}

#[derive(Debug, Clone)]
pub struct ScanResult {
    /// The scanned folder, resolved.
    pub root: String,
    pub candidates: Vec<ScanCandidate>,
    /// The walk stopped at a limit.
    pub truncated: bool,
    /// Folders that were skipped, with why (unreadable, symbolic link
    /// leaving the allowed folders).
    pub skipped: Vec<String>,
}

/// One app to import.
#[derive(Debug, Clone, Default)]
pub struct ImportApp {
    pub name: String,
    pub repository_url: Option<String>,
    pub branch: Option<String>,
    /// Copy from this directory instead of cloning.
    pub source_dir: Option<PathBuf>,
    pub source_subdir: Option<String>,
    pub runtime: Option<String>,
    /// Dashboard settings to save with it (start command, port, env file...).
    pub settings: aegis_config::ProjectOverrides,
}

/// The outcome of importing one app.
#[derive(Debug, Clone)]
pub struct ImportedApp {
    pub name: String,
    pub project_id: Option<String>,
    pub error: Option<String>,
    /// What to do before the first deployment.
    pub next_steps: Vec<String>,
}

/// `.git/config` and `.git/HEAD` of a checkout, read without running git.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitInfo {
    pub remote_url: Option<String>,
    pub branch: Option<String>,
}

/// Parses `.git/config`: the `origin` remote's URL, else the first remote's.
pub fn parse_git_config(text: &str) -> Option<String> {
    let mut section: Option<String> = None;
    let mut first: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') || line.is_empty() {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = Some(header.trim().to_string());
            continue;
        }
        let Some(name) = section.as_deref().and_then(|s| {
            let rest = s.strip_prefix("remote")?.trim();
            Some(rest.trim_matches('"').to_string())
        }) else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("url") {
            continue;
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value)
            .to_string();
        if name == "origin" {
            return Some(value);
        }
        first.get_or_insert(value);
    }
    first
}

/// Parses `.git/HEAD`: the branch, or `None` when detached.
pub fn parse_git_head(text: &str) -> Option<String> {
    text.trim()
        .strip_prefix("ref:")?
        .trim()
        .strip_prefix("refs/heads/")
        .map(str::to_string)
}

/// The git directory of a checkout (`.git`, or where a `.git` file points).
fn git_dir(root: &Path) -> Option<PathBuf> {
    let dot = root.join(".git");
    let meta = std::fs::symlink_metadata(&dot).ok()?;
    if meta.is_dir() {
        return Some(dot);
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let target = text.trim().strip_prefix("gitdir:")?.trim();
    let path = Path::new(target);
    Some(if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    })
}

/// Reads a checkout's remote and branch (read-only, without git).
pub fn read_git_info(root: &Path) -> Option<GitInfo> {
    let dir = git_dir(root)?;
    let branch = std::fs::read_to_string(dir.join("HEAD"))
        .ok()
        .and_then(|h| parse_git_head(&h));
    // Linked worktrees keep the config in the main repository.
    let common = std::fs::read_to_string(dir.join("commondir"))
        .ok()
        .map(|c| dir.join(c.trim()))
        .unwrap_or_else(|| dir.clone());
    let remote_url = std::fs::read_to_string(common.join("config"))
        .ok()
        .and_then(|c| parse_git_config(&c));
    Some(GitInfo { remote_url, branch })
}

/// Removes `user:password@` from an HTTPS URL. Returns whether it did.
fn strip_credentials(url: &str) -> (String, bool) {
    let redacted = aegis_builder::redact_url(url);
    if redacted == url {
        return (url.to_string(), false);
    }
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let end = rest.find('/').unwrap_or(rest.len());
            let host = &rest[rest[..end].rfind('@').map_or(0, |i| i + 1)..];
            (format!("{}://{}", scheme, host), true)
        }
        None => (url.to_string(), false),
    }
}

/// A repository URL comparable across forms (`git@github.com:o/r.git`,
/// `https://github.com/o/r`).
pub(crate) fn normalize_repo(url: &str) -> String {
    if let Some(remote) = aegis_ssh::RemoteUrl::parse(url) {
        if let Some((owner, repo)) = remote.github_repo() {
            return format!("github.com/{}/{}", owner, repo).to_ascii_lowercase();
        }
        let path = remote.path.trim_end_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        return format!("{}/{}", remote.host, path);
    }
    let url = url.trim().trim_end_matches('/');
    url.strip_suffix(".git").unwrap_or(url).to_string()
}

fn is_candidate(dir: &Path) -> bool {
    MARKERS.iter().any(|m| dir.join(m).is_file())
}

/// Sub-apps of a monorepo: `apps/*`, `packages/*`, `services/*` with their
/// own package.json and a start script (or another app marker).
pub fn monorepo_apps(root: &Path) -> Vec<PathBuf> {
    let mut apps = Vec::new();
    for group in WORKSPACE_DIRS {
        let Ok(entries) = std::fs::read_dir(root.join(group)) else {
            continue;
        };
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .filter(|p| {
                if p.join("package.json").is_file() {
                    aegis_engine::start_hint::package_start_script(p).is_some()
                } else {
                    is_candidate(p)
                }
            })
            .collect();
        found.sort();
        apps.extend(found);
    }
    apps
}

/// The `.env` next to an app: names (never values), or why it can't be read.
fn inspect_env(path: &Path) -> (bool, Vec<String>, Option<ScanWarning>) {
    use std::io::Read;
    let file = std::fs::File::open(path).and_then(|f| {
        let mut bytes = Vec::new();
        f.take(aegis_config::envfile::MAX_ENV_FILE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    });
    match file {
        Ok(bytes) => {
            let names = String::from_utf8(bytes)
                .ok()
                .and_then(|t| aegis_config::envfile::parse(&t).ok())
                .map(|vars| vars.names().map(str::to_string).collect())
                .unwrap_or_default();
            (true, names, None)
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            let message = aegis_config::envfile::EnvFileError::PermissionDenied {
                path: path.display().to_string(),
                user: aegis_config::envfile::daemon_user(),
            }
            .to_string();
            (
                false,
                Vec::new(),
                Some(ScanWarning {
                    code: "env_unreadable",
                    message,
                }),
            )
        }
        Err(e) => (
            false,
            Vec::new(),
            Some(ScanWarning {
                code: "env_unreadable",
                message: format!("Cannot read {}: {}", path.display(), e),
            }),
        ),
    }
}

/// Resolved allowed folders (those that exist).
fn resolved(dirs: &[PathBuf]) -> Vec<PathBuf> {
    dirs.iter()
        .filter_map(|d| std::fs::canonicalize(d).ok())
        .collect()
}

fn inside_any(path: &Path, dirs: &[PathBuf]) -> bool {
    dirs.iter().any(|d| path.starts_with(d))
}

/// An app folder found by [`walk`], with the checkout it belongs to.
pub type FoundApp = (PathBuf, Option<PathBuf>);

/// What [`walk`] found.
#[derive(Debug, Default)]
pub struct Walk {
    pub apps: Vec<FoundApp>,
    /// A limit was hit.
    pub truncated: bool,
    /// Folders not looked into, and why.
    pub skipped: Vec<String>,
}

/// Walks `root` (already resolved and allowed) for app folders.
pub fn walk(root: &Path, allowed: &[PathBuf]) -> Walk {
    let mut found: Vec<FoundApp> = Vec::new();
    let mut skipped = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut queue: VecDeque<(PathBuf, usize, Option<PathBuf>)> = VecDeque::new();
    // The checkout `root` itself may be in (scanning one app's folder).
    let root_repo = root
        .ancestors()
        .take_while(|a| inside_any(a, allowed))
        .find(|a| a.join(".git").exists())
        .map(Path::to_path_buf);
    queue.push_back((root.to_path_buf(), 0, root_repo));
    let mut entries_seen = 0usize;
    let mut truncated = false;
    while let Some((dir, depth, repo)) = queue.pop_front() {
        if !seen.insert(dir.clone()) {
            continue;
        }
        let repo = if dir.join(".git").exists() {
            Some(dir.clone())
        } else {
            repo
        };
        if is_candidate(&dir) {
            let subapps = monorepo_apps(&dir);
            // Next to sub-apps, a package.json without a start script is
            // the monorepo's root, not an app.
            let root_is_app = subapps.is_empty()
                || aegis_engine::start_hint::package_start_script(&dir).is_some()
                || MARKERS
                    .iter()
                    .filter(|m| **m != "package.json")
                    .any(|m| dir.join(m).is_file());
            if root_is_app {
                found.push((dir.clone(), repo.clone()));
            }
            for app in subapps {
                found.push((app, repo.clone()));
            }
            if found.len() >= MAX_CANDIDATES {
                truncated = true;
                break;
            }
            // An app's own folders are not searched for more apps.
            continue;
        }
        if depth >= MAX_DEPTH {
            continue;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                skipped.push(format!("{}: {}", dir.display(), e));
                continue;
            }
        };
        let mut children = Vec::new();
        for entry in entries.flatten() {
            entries_seen += 1;
            if entries_seen > MAX_ENTRIES {
                truncated = true;
                break;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_symlink() {
                match std::fs::canonicalize(&path) {
                    Ok(target) if target.is_dir() && inside_any(&target, allowed) => {
                        children.push(target)
                    }
                    Ok(target) if target.is_dir() => skipped.push(format!(
                        "{}: a symbolic link to {}, outside the allowed folders",
                        path.display(),
                        target.display()
                    )),
                    _ => {}
                }
            } else if file_type.is_dir() {
                children.push(path);
            }
        }
        children.sort();
        for child in children {
            queue.push_back((child, depth + 1, repo.clone()));
        }
        if truncated {
            break;
        }
    }
    Walk {
        apps: found,
        truncated,
        skipped,
    }
}

impl ControlPlane {
    /// Whether apps may have env files (`[apps] env_file_dirs` is set).
    pub fn env_files_enabled(&self) -> bool {
        !self.inner.settings.env_file_dirs.is_empty()
    }

    /// The folders scans may look in: `[apps] import_dirs` and
    /// `[apps] env_file_dirs`.
    pub fn import_dirs(&self) -> Vec<PathBuf> {
        let settings = &self.inner.settings;
        let mut dirs = settings.import_dirs.clone();
        for dir in &settings.env_file_dirs {
            if !dirs.contains(dir) {
                dirs.push(dir.clone());
            }
        }
        dirs
    }

    /// Scans a folder on the server for apps (see the module docs).
    pub async fn scan_folder(&self, path: &str) -> ControlResult<ScanResult> {
        let allowed_dirs = self.import_dirs();
        if allowed_dirs.is_empty() {
            return Err(ControlError::FailedPrecondition(
                "Scanning is turned off on this server: list the folders it may look in in the daemon configuration, e.g. [apps] import_dirs = [\"/opt/github\"], and restart the daemon".into(),
            ));
        }
        let path = path.trim();
        if path.is_empty() || !Path::new(path).is_absolute() || path.contains('\0') {
            return Err(ControlError::InvalidArgument(
                "Give the absolute path of a folder, e.g. /opt/github".into(),
            ));
        }
        let allowed = resolved(&allowed_dirs);
        let root = std::fs::canonicalize(path)
            .map_err(|e| ControlError::InvalidArgument(format!("Cannot open {}: {}", path, e)))?;
        if !inside_any(&root, &allowed) || !root.is_dir() {
            return Err(ControlError::InvalidArgument(format!(
                "{} is not inside a folder scans may look in ({}); symbolic links are resolved before this check",
                path,
                allowed_dirs
                    .iter()
                    .map(|d| d.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let this = self.clone();
        let root_text = root.display().to_string();
        let result = tokio::task::spawn_blocking(move || {
            let Walk {
                apps: found,
                truncated,
                skipped,
            } = walk(&root, &allowed);
            let candidates = found
                .into_iter()
                .map(|(dir, repo)| this.describe_candidate(&dir, repo.as_deref()))
                .collect();
            (candidates, truncated, skipped)
        })
        .await
        .map_err(|e| ControlError::Internal(anyhow::anyhow!("Scan failed: {}", e)))?;
        Ok(ScanResult {
            root: root_text,
            candidates: result.0,
            truncated: result.1,
            skipped: result.2,
        })
    }

    fn describe_candidate(&self, dir: &Path, repo: Option<&Path>) -> ScanCandidate {
        let mut warnings = Vec::new();
        let config = ResolvedProjectConfig::load(dir)
            .unwrap_or_else(|_| ResolvedProjectConfig::resolve(dir, None));
        let source_subdir = repo
            .filter(|r| *r != dir)
            .and_then(|r| dir.strip_prefix(r).ok())
            .map(|rel| {
                rel.components()
                    .map(|c| c.as_os_str().to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join("/")
            });
        let git = repo.and_then(read_git_info).unwrap_or_default();
        let (repository_url, had_credentials) = match &git.remote_url {
            Some(url) => {
                let (clean, stripped) = strip_credentials(url);
                (Some(clean), stripped)
            }
            None => (None, false),
        };
        if had_credentials {
            warnings.push(ScanWarning {
                code: "credentials_in_url",
                message: "The checkout's remote URL contains credentials; they are not imported. Use a deploy key instead.".into(),
            });
        }
        let remote = repository_url
            .as_deref()
            .and_then(aegis_ssh::RemoteUrl::parse);
        let (remote_kind, deploy_key) = match &remote {
            Some(r) if r.github_repo().is_some() && r.is_ssh() => ("github", "required"),
            Some(r) if r.github_repo().is_some() => ("github", "check"),
            Some(r) if r.is_ssh() => ("ssh", "required"),
            Some(_) => ("https", "unknown"),
            None => ("none", "no"),
        };
        match (deploy_key, &repository_url) {
            ("required", Some(url)) => warnings.push(ScanWarning {
                code: "ssh_remote",
                message: format!(
                    "{} is an SSH remote: the app needs a deploy key (Settings → Repository after import) before it can be deployed",
                    url
                ),
            }),
            ("check", _) => warnings.push(ScanWarning {
                code: "private_repo",
                message: "If this GitHub repository is private, the app needs a deploy key: use Check to find out".into(),
            }),
            _ => {}
        }
        if repository_url.is_none() {
            warnings.push(ScanWarning {
                code: "no_remote",
                message: if repo.is_some() {
                    "The checkout has no remote; the app will copy this folder at each deployment"
                        .into()
                } else {
                    "Not a git checkout; the app will copy this folder at each deployment".into()
                },
            });
        }
        if repo.is_some() && git.branch.is_none() && repository_url.is_some() {
            warnings.push(ScanWarning {
                code: "detached_head",
                message: "The checkout is not on a branch; main is assumed".into(),
            });
        }

        let start_hint = config.start_hint.clone();
        let mut start_command = config.start_command.clone();
        let mut port = config.port;
        let mut health_check_url = config.health_check_url.clone();
        if let Some(hint) = &start_hint {
            warnings.push(ScanWarning {
                code: "hardcoded_port",
                message: format!(
                    "The start script ({}) fixes port {}; Aegis gives each release its port in $PORT, so the start command becomes: {}",
                    hint.script, hint.port, hint.suggested_start
                ),
            });
            start_command = hint.suggested_start.clone();
            if let (Some(old), Some(url)) = (port, health_check_url.as_deref()) {
                health_check_url =
                    Some(url.replacen(&format!(":{}", old), &format!(":{}", hint.port), 1));
            }
            port = Some(hint.port);
        }
        let port_in_use = port.filter(|p| crate::ports::port_in_use(*p)).map(|p| {
            let owner = crate::ports::describe_listener(p);
            let text = match owner {
                Some(owner) => format!("port {} is in use by {} (probably the current deployment)", p, owner),
                None => format!("port {} is in use (probably the current deployment)", p),
            };
            warnings.push(ScanWarning {
                code: "port_in_use",
                message: format!(
                    "{}; stop it before the first deployment, or map a domain so Aegis runs the app on a port of its own",
                    text
                ),
            });
            text
        });

        let env_path = dir.join(".env");
        let has_env = std::fs::symlink_metadata(&env_path).is_ok();
        let (env_file_readable, env_names, env_warning) = if has_env {
            inspect_env(&env_path)
        } else {
            (false, Vec::new(), None)
        };
        warnings.extend(env_warning);
        let env_text = env_path.display().to_string();
        let env_file_allowed = has_env
            && aegis_config::envfile::validate_setting(
                &env_text,
                &self.inner.settings.env_file_dirs,
            )
            .is_ok();
        if has_env && !env_file_allowed {
            warnings.push(ScanWarning {
                code: "env_outside_allowed",
                message: format!(
                    "{} can't be the app's Env file: its folder is not in [apps] env_file_dirs. Add it there, or import the file on the app's Environment card",
                    env_text
                ),
            });
        }

        let registered_as = self.registered_as(
            repository_url.as_deref(),
            git.branch.as_deref(),
            source_subdir.as_deref(),
            dir,
        );
        // Monorepo apps are named after the checkout and the app (`shop-web`).
        let leaf = |p: &Path| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        };
        let name = match (repo, &source_subdir) {
            (Some(repo), Some(_)) => format!("{}-{}", leaf(repo), leaf(dir)),
            _ => config.name.clone(),
        };
        ScanCandidate {
            path: dir.display().to_string(),
            repo_root: repo.map(|r| r.display().to_string()),
            source_subdir,
            name,
            runtime: config.runtime.clone(),
            install_command: config.install_command.clone(),
            build_command: config.build_command.clone(),
            start_command,
            port,
            health_check_url,
            repository_url,
            branch: git.branch,
            remote_kind,
            deploy_key,
            env_file: has_env.then_some(env_text),
            env_file_readable,
            env_names,
            env_file_allowed,
            start_hint,
            port_in_use,
            registered_as,
            warnings,
        }
    }

    /// The registered app with the same source, if any.
    fn registered_as(
        &self,
        repository_url: Option<&str>,
        branch: Option<&str>,
        subdir: Option<&str>,
        dir: &Path,
    ) -> Option<String> {
        let wanted_repo = repository_url.map(normalize_repo);
        let dir_text = dir.display().to_string();
        self.inner
            .projection
            .get_projects()
            .into_iter()
            .find(|p| {
                let same_dir = p.source_dir.as_deref() == Some(dir_text.as_str());
                let same_repo = !p.repository_url.is_empty()
                    && wanted_repo.as_deref() == Some(normalize_repo(&p.repository_url).as_str())
                    && branch.is_none_or(|b| b == p.branch)
                    && p.source_subdir.as_deref() == subdir;
                same_dir || same_repo
            })
            .map(|p| p.name)
    }

    /// Registers several apps without deploying them. Each app is imported
    /// on its own: one failing doesn't stop the others.
    pub async fn import_apps(&self, apps: Vec<ImportApp>) -> ControlResult<Vec<ImportedApp>> {
        if apps.is_empty() {
            return Err(ControlError::InvalidArgument(
                "Select at least one app".into(),
            ));
        }
        if apps.len() > MAX_IMPORT {
            return Err(ControlError::InvalidArgument(format!(
                "Import at most {} apps at a time",
                MAX_IMPORT
            )));
        }
        let mut taken: HashSet<String> = self
            .inner
            .projection
            .get_projects()
            .into_iter()
            .map(|p| p.name)
            .collect();
        let mut results = Vec::new();
        for app in apps {
            let name = app.name.trim().to_string();
            let outcome = if taken.contains(&name) {
                Err(ControlError::InvalidArgument(format!(
                    "An app named '{}' already exists",
                    name
                )))
            } else {
                self.import_one(&app).await
            };
            match outcome {
                Ok((id, steps)) => {
                    taken.insert(name.clone());
                    results.push(ImportedApp {
                        name,
                        project_id: Some(id),
                        error: None,
                        next_steps: steps,
                    });
                }
                Err(e) => results.push(ImportedApp {
                    name,
                    project_id: None,
                    error: Some(e.to_string()),
                    next_steps: Vec::new(),
                }),
            }
        }
        let imported: Vec<_> = results
            .iter()
            .filter_map(|r| Some(json!({"project_id": r.project_id.as_ref()?, "name": r.name})))
            .collect();
        if !imported.is_empty() {
            self.record(
                "AppsImported",
                json!({"count": imported.len(), "apps": imported}),
            )
            .await?;
        }
        Ok(results)
    }

    async fn import_one(&self, app: &ImportApp) -> ControlResult<(String, Vec<String>)> {
        let has_repo = app
            .repository_url
            .as_deref()
            .is_some_and(|u| !u.trim().is_empty());
        if has_repo == app.source_dir.is_some() {
            return Err(ControlError::InvalidArgument(
                "Give each app a repository URL or a directory, not both".into(),
            ));
        }
        // Validate settings before anything is recorded.
        let settings = crate::validate_settings(app.settings.clone())?;
        if let Some(path) = &settings.env_file {
            aegis_config::envfile::validate_setting(path, &self.inner.settings.env_file_dirs)
                .map_err(|e| ControlError::InvalidArgument(e.to_string()))?;
        }
        let project = self
            .register_project(RegisterProject {
                project_id: None,
                name: app.name.trim().to_string(),
                repository_url: app.repository_url.clone().unwrap_or_default(),
                branch: app.branch.clone().unwrap_or_default(),
                runtime: app.runtime.clone().filter(|r| !r.is_empty()),
                source_dir: app.source_dir.clone(),
                source_subdir: app.source_subdir.clone(),
            })
            .await?;
        if !settings.is_empty() {
            self.update_project_settings(project.id, settings.clone())
                .await?;
        }
        let mut steps = Vec::new();
        if let Some(url) = has_repo.then_some(project.repository_url.as_str()) {
            let ssh = aegis_ssh::RemoteUrl::parse(url).is_some_and(|r| r.is_ssh());
            let github = aegis_ssh::RemoteUrl::parse(url).and_then(|r| r.github_repo());
            if ssh || github.is_some() {
                steps.push(format!(
                    "{}: generate a deploy key in Settings → Repository and add it to the repository (read-only){}",
                    if ssh { "Needed" } else { "If the repository is private" },
                    aegis_ssh::github_deploy_keys_page(url)
                        .map(|p| format!(" at {}", p))
                        .unwrap_or_default()
                ));
            }
        }
        match &settings.env_file {
            Some(path) => steps.push(format!("Env file: {} (read at every build and start)", path)),
            None => steps.push(
                "Set the Env file or Variables/Secrets on the Environment card if the app needs them"
                    .into(),
            ),
        }
        steps.push(
            "Map a domain (Settings → Domains) so each release gets its own port and deployments switch without downtime"
                .into(),
        );
        if let Some(port) = settings.port.filter(|p| crate::ports::port_in_use(*p)) {
            steps.push(format!(
                "Port {} is in use: stop the old process (e.g. `pm2 stop <name>`) before the first deployment, or map a domain first",
                port
            ));
        }
        steps.push("Deploy from the app's page when ready".into());
        Ok((project.id.to_string(), steps))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_config_and_head() {
        let config = r#"
[core]
	repositoryformatversion = 0
[remote "upstream"]
	url = https://github.com/other/fork.git
[remote "origin"]
	url = git@github.com:acme/img-man.git
	fetch = +refs/heads/*:refs/remotes/origin/*
[branch "main"]
	remote = origin
"#;
        assert_eq!(
            parse_git_config(config).as_deref(),
            Some("git@github.com:acme/img-man.git")
        );
        assert_eq!(
            parse_git_config("[remote \"fork\"]\n url = \"https://x/y.git\"\n").as_deref(),
            Some("https://x/y.git")
        );
        assert_eq!(parse_git_config("[core]\nbare = false\n"), None);
        assert_eq!(
            parse_git_head("ref: refs/heads/prod\n").as_deref(),
            Some("prod")
        );
        assert_eq!(
            parse_git_head("4b825dc642cb6eb9a060e54bf8d69288fbee4904\n"),
            None
        );
    }

    #[test]
    fn credentials_and_normalisation() {
        assert_eq!(
            strip_credentials("https://ghp_x@github.com/o/r.git"),
            ("https://github.com/o/r.git".into(), true)
        );
        assert_eq!(
            strip_credentials("git@github.com:o/r.git"),
            ("git@github.com:o/r.git".into(), false)
        );
        assert_eq!(
            normalize_repo("git@github.com:Acme/App.git"),
            "github.com/acme/app"
        );
        assert_eq!(
            normalize_repo("https://github.com/acme/app"),
            "github.com/acme/app"
        );
        assert_eq!(normalize_repo("/srv/repo.git"), "/srv/repo");
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn walking_finds_apps_within_limits() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        // Two apps at depth 1, one at depth 2, one too deep.
        write(
            &root.join("api/package.json"),
            r#"{"scripts":{"start":"node a.js"}}"#,
        );
        write(&root.join("api/node_modules/dep/package.json"), "{}");
        write(&root.join("py/requirements.txt"), "flask");
        write(&root.join("group/svc/go.mod"), "module x");
        write(&root.join("a/b/too-deep/package.json"), "{}");
        // Skipped folders.
        write(&root.join("node_modules/x/package.json"), "{}");
        write(&root.join(".hidden/package.json"), "{}");
        write(&root.join("dist/package.json"), "{}");
        // A monorepo: the root has no start script; two apps have one.
        write(&root.join("mono/.git/HEAD"), "ref: refs/heads/main\n");
        write(
            &root.join("mono/package.json"),
            r#"{"workspaces":["apps/*"]}"#,
        );
        write(
            &root.join("mono/apps/web/package.json"),
            r#"{"scripts":{"start":"next start -p 4000"}}"#,
        );
        write(
            &root.join("mono/apps/worker/package.json"),
            r#"{"scripts":{"start":"node w.js"}}"#,
        );
        write(
            &root.join("mono/packages/lib/package.json"),
            r#"{"name":"lib"}"#,
        );

        let allowed = vec![root.clone()];
        let Walk {
            apps: found,
            truncated,
            ..
        } = walk(&root, &allowed);
        let mut dirs: Vec<String> = found
            .iter()
            .map(|(d, _)| {
                d.strip_prefix(&root)
                    .unwrap()
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .collect();
        dirs.sort();
        assert_eq!(
            dirs,
            vec![
                "api",
                "group/svc",
                "mono/apps/web",
                "mono/apps/worker",
                "py"
            ]
        );
        assert!(!truncated);
        let web = found.iter().find(|(d, _)| d.ends_with("web")).unwrap();
        assert_eq!(web.1.as_deref(), Some(root.join("mono").as_path()));
        assert_eq!(monorepo_apps(&root.join("mono")).len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_out_of_the_allowed_folders_are_not_followed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write(&outside.path().join("secret-app/package.json"), "{}");
        write(&root.join("real/inner/package.json"), "{}");
        std::os::unix::fs::symlink(outside.path(), root.join("escape")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("alias")).unwrap();
        let Walk {
            apps: found,
            skipped,
            ..
        } = walk(&root, std::slice::from_ref(&root));
        assert!(found.iter().all(|(d, _)| d.starts_with(&root)), "{found:?}");
        assert!(skipped
            .iter()
            .any(|s| s.contains("outside the allowed folders")));
        // The alias resolves to an allowed folder already seen: listed once.
        assert_eq!(found.len(), 1, "{found:?}");
    }

    #[test]
    fn entry_limit_truncates() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for i in 0..(MAX_ENTRIES + 10) {
            std::fs::create_dir(root.join(format!("d{}", i))).unwrap();
        }
        let truncated = walk(&root, std::slice::from_ref(&root)).truncated;
        assert!(truncated);
    }

    #[test]
    fn env_names_only() {
        let tmp = tempfile::TempDir::new().unwrap();
        let env = tmp.path().join(".env");
        std::fs::write(&env, "PORT=4000\nDATABASE_URL=postgres://u:hunter2@db/x\n").unwrap();
        let (readable, names, warning) = inspect_env(&env);
        assert!(readable);
        assert_eq!(names, vec!["DATABASE_URL".to_string(), "PORT".to_string()]);
        assert!(warning.is_none());
        assert!(!format!("{:?}", names).contains("hunter2"));
    }
}
