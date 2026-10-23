//! An app's environment and where each variable comes from.
//!
//! From lowest to highest precedence:
//!
//! 1. the env file on the server (Settings → Env file), read at every
//!    build and every process start, never stored;
//! 2. `[env]` in the release's aegis.toml;
//! 3. dashboard Variables (merged into `[env]` when a release is built);
//! 4. dashboard Secrets (process start only, never at build time);
//! 5. `PORT`, which Aegis sets (an env file's `PORT` is ignored).
//!
//! Build commands get 1-3. Nothing here ever returns, logs or records a
//! value from the env file or a secret.

use crate::{ControlError, ControlPlane, ControlResult};
use aegis_config::envfile::{self, EnvVars};
use aegis_engine::ResolvedProjectConfig;
use aegis_types::ProjectId;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

/// The variable Aegis always sets itself.
const RESERVED: &str = "PORT";

/// Where a variable's value comes from, lowest precedence first.
pub const SOURCES: [&str; 4] = ["env_file", "aegis_toml", "variable", "secret"];

/// The env file's state right now (it is read for this).
#[derive(Debug, Clone, PartialEq)]
pub struct EnvFileStatus {
    pub path: String,
    pub readable: bool,
    /// Why it can't be used (no values in it).
    pub error: Option<String>,
    /// The names it defines.
    pub names: Vec<String>,
}

/// One variable the app gets.
#[derive(Debug, Clone, PartialEq)]
pub struct EnvSource {
    pub name: String,
    /// `env_file`, `aegis_toml`, `variable`, `secret`, or `aegis` (`PORT`).
    pub source: &'static str,
    /// Lower-precedence sources that also set it (and are overridden).
    pub overrides: Vec<&'static str>,
    /// Whether build commands see it (from the env file, `[env]` or a
    /// Variable; secrets are not given to builds).
    pub build: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnvOverview {
    /// `[apps] env_file_dirs`; empty means env files are off.
    pub env_file_dirs: Vec<PathBuf>,
    pub env_file: Option<EnvFileStatus>,
    /// Sorted by name.
    pub variables: Vec<EnvSource>,
}

/// How to import one variable from a pasted `.env`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportAs {
    Variable,
    Secret,
    Skip,
}

impl ImportAs {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "variable" => Some(Self::Variable),
            "secret" => Some(Self::Secret),
            "skip" => Some(Self::Skip),
            _ => None,
        }
    }
}

/// A variable found in a pasted `.env`, with a suggestion (no value).
#[derive(Debug, Clone, PartialEq)]
pub struct ImportCandidate {
    pub name: String,
    /// `variable`, `secret` or `skip`.
    pub suggested: &'static str,
    pub reason: &'static str,
    pub multiline: bool,
    /// `variable` or `secret` if the app already has it.
    pub exists_as: Option<&'static str>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportResult {
    pub variables: Vec<String>,
    pub secrets: Vec<String>,
    pub skipped: Vec<String>,
}

/// A user:password in a URL (`scheme://user:pass@host`).
fn url_has_credentials(value: &str) -> bool {
    let Some((_, rest)) = value.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    authority
        .rsplit_once('@')
        .is_some_and(|(userinfo, _)| !userinfo.is_empty())
}

/// Suggests how to store a variable: as a Secret when the name or value
/// looks sensitive, else as a Variable.
pub fn suggest_import(name: &str, value: &str) -> (&'static str, &'static str) {
    let upper = name.to_ascii_uppercase();
    if ["NEXT_PUBLIC_", "VITE_", "REACT_APP_", "PUBLIC_"]
        .iter()
        .any(|p| upper.starts_with(p))
    {
        return ("variable", "Public: bundled into the client at build time");
    }
    if name == RESERVED {
        return ("skip", "PORT is set by Aegis");
    }
    if value.contains('\n') {
        return ("secret", "Multi-line values can only be secrets");
    }
    if [
        "KEY",
        "SECRET",
        "TOKEN",
        "PASSWORD",
        "PASSWD",
        "PRIVATE",
        "CREDENTIAL",
        "AUTH",
        "SALT",
        "SIGNATURE",
    ]
    .iter()
    .any(|w| upper.contains(w))
    {
        return ("secret", "The name suggests a credential");
    }
    if url_has_credentials(value) {
        return ("secret", "A URL with a user name or password");
    }
    ("variable", "Looks like plain configuration")
}

impl ControlPlane {
    /// The project's env file, read now (`None` without one).
    fn read_project_env_file(
        &self,
        project_id: &ProjectId,
    ) -> Option<(String, Result<EnvVars, envfile::EnvFileError>)> {
        let path = self
            .inner
            .projection
            .get_project(project_id)?
            .settings
            .env_file?;
        let read = envfile::read_env_file(&path, &self.inner.settings.env_file_dirs);
        Some((path, read))
    }

    /// The environment for a release's process: the env file, `[env]`
    /// (including dashboard variables) and `PORT`, then secrets, which win
    /// over plain variables. Fails if the project has an env file that
    /// can't be read: starting without its values would be worse.
    pub(crate) fn process_env(
        &self,
        project_id: ProjectId,
        config: &ResolvedProjectConfig,
    ) -> ControlResult<HashMap<String, String>> {
        let mut env: HashMap<String, String> = match self.read_project_env_file(&project_id) {
            Some((_, Ok(vars))) => vars
                .into_inner()
                .into_iter()
                .filter(|(name, _)| name != RESERVED)
                .collect(),
            Some((_, Err(e))) => {
                return Err(ControlError::FailedPrecondition(format!("Env file: {}", e)))
            }
            None => HashMap::new(),
        };
        env.extend(config.process_env());
        env.extend(self.inner.secrets.env(&project_id.to_string()));
        Ok(env)
    }

    /// Every variable the app gets and where it comes from. Names only.
    pub fn env_overview(&self, project_id: &ProjectId) -> ControlResult<EnvOverview> {
        let project = self
            .inner
            .projection
            .get_project(project_id)
            .ok_or_else(|| ControlError::NotFound(format!("Unknown project {}", project_id)))?;
        let mut by_name: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
        let mut add = |names: Vec<String>, source: usize| {
            for name in names {
                by_name.entry(name).or_default().insert(source);
            }
        };

        let env_file = self
            .read_project_env_file(project_id)
            .map(|(path, read)| match read {
                Ok(vars) => EnvFileStatus {
                    path,
                    readable: true,
                    error: None,
                    names: vars.names().map(str::to_string).collect(),
                },
                Err(e) => EnvFileStatus {
                    path,
                    readable: false,
                    error: Some(e.to_string()),
                    names: Vec::new(),
                },
            });
        if let Some(status) = &env_file {
            add(status.names.clone(), 0);
        }
        let variables: BTreeSet<&String> = project.settings.env.keys().collect();
        // `[env]` of the file the next release starts from. A live release's
        // file already has the dashboard variables merged in.
        if let Ok((base, file)) = self.base_project_file(&project) {
            add(
                file.env
                    .keys()
                    .filter(|k| base != "release" || !variables.contains(k))
                    .cloned()
                    .collect(),
                1,
            );
        }
        add(variables.into_iter().cloned().collect(), 2);
        add(
            self.inner
                .secrets
                .list(&project_id.to_string())
                .into_iter()
                .map(|s| s.name)
                .collect(),
            3,
        );

        let variables = by_name
            .into_iter()
            .map(|(name, sources)| {
                // An env file's PORT is ignored: Aegis sets it.
                let file_port = name == RESERVED && sources.contains(&0);
                let effective: Vec<usize> = sources
                    .iter()
                    .copied()
                    .filter(|&s| !(file_port && s == 0))
                    .collect();
                let winner = effective.last().copied();
                EnvSource {
                    source: winner.map_or("aegis", |s| SOURCES[s]),
                    overrides: sources
                        .iter()
                        .rev()
                        .filter(|&&s| Some(s) != winner)
                        .map(|&s| SOURCES[s])
                        .collect(),
                    build: effective.iter().any(|&s| s < 3),
                    name,
                }
            })
            .collect();
        Ok(EnvOverview {
            env_file_dirs: self.inner.settings.env_file_dirs.clone(),
            env_file,
            variables,
        })
    }

    /// Parses a pasted `.env` and suggests how to import each variable.
    pub fn preview_env_import(
        &self,
        project_id: &ProjectId,
        content: &str,
    ) -> ControlResult<Vec<ImportCandidate>> {
        let project = self
            .inner
            .projection
            .get_project(project_id)
            .ok_or_else(|| ControlError::NotFound(format!("Unknown project {}", project_id)))?;
        let vars = parse_import(content)?;
        let secrets: BTreeSet<String> = self
            .inner
            .secrets
            .list(&project_id.to_string())
            .into_iter()
            .map(|s| s.name)
            .collect();
        Ok(vars
            .iter()
            .map(|(name, value)| {
                let exists_as = if secrets.contains(name) {
                    Some("secret")
                } else if project.settings.env.contains_key(name) {
                    Some("variable")
                } else {
                    None
                };
                let multiline = value.contains('\n');
                let (mut suggested, mut reason) = suggest_import(name, value);
                // Keep what the app already has, unless that can't work.
                if let Some(kind) = exists_as {
                    if suggested != "skip" && !(kind == "variable" && multiline) {
                        suggested = kind;
                        reason = if kind == "secret" {
                            "Already a secret"
                        } else {
                            "Already a variable"
                        };
                    }
                }
                ImportCandidate {
                    name: name.to_string(),
                    suggested,
                    reason,
                    multiline,
                    exists_as,
                }
            })
            .collect())
    }

    /// Imports variables from a pasted `.env` as dashboard Variables and
    /// Secrets. Every name in `choices` must be in `content`. Importing a
    /// name as a secret removes a Variable of the same name, so no plain
    /// copy is left in future releases. Values are never recorded except
    /// as Variables (which, like every Variable, are part of the settings).
    pub async fn import_env(
        &self,
        project_id: ProjectId,
        content: &str,
        choices: &BTreeMap<String, ImportAs>,
    ) -> ControlResult<ImportResult> {
        let project = self
            .inner
            .projection
            .get_project(&project_id)
            .ok_or_else(|| ControlError::NotFound(format!("Unknown project {}", project_id)))?;
        let vars = parse_import(content)?;
        let invalid = ControlError::InvalidArgument;
        // Check everything before changing anything.
        for (name, how) in choices {
            let value = vars
                .get(name)
                .ok_or_else(|| invalid(format!("{} is not in the pasted file", name)))?;
            if *how == ImportAs::Skip {
                continue;
            }
            aegis_secrets::validate_name(name)
                .and_then(|_| aegis_secrets::validate_value(value))
                .map_err(|e| invalid(format!("{}: {}", name, e)))?;
            if *how == ImportAs::Variable && value.contains('\n') {
                return Err(invalid(format!(
                    "{} has a multi-line value; import it as a secret",
                    name
                )));
            }
        }
        let mut result = ImportResult::default();
        let mut settings = project.settings.clone();
        for (name, how) in choices {
            let value = vars.get(name).unwrap_or_default();
            match how {
                ImportAs::Skip => result.skipped.push(name.clone()),
                ImportAs::Secret => {
                    self.set_secret(project_id, name, value).await?;
                    settings.env.remove(name);
                    result.secrets.push(name.clone());
                }
                ImportAs::Variable => {
                    settings.env.insert(name.clone(), value.to_string());
                    result.variables.push(name.clone());
                }
            }
        }
        if settings != project.settings {
            self.update_project_settings(project_id, settings).await?;
        }
        Ok(result)
    }
}

fn parse_import(content: &str) -> ControlResult<EnvVars> {
    if content.len() as u64 > envfile::MAX_ENV_FILE_BYTES {
        return Err(ControlError::InvalidArgument(
            "The pasted file is larger than 1 MiB".into(),
        ));
    }
    envfile::parse(content)
        .map_err(|e| ControlError::InvalidArgument(format!("Invalid .env: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions() {
        let kind = |name: &str, value: &str| suggest_import(name, value).0;
        assert_eq!(kind("NEXT_PUBLIC_API_KEY", "abc"), "variable");
        assert_eq!(kind("NODE_ENV", "production"), "variable");
        assert_eq!(kind("NEXTAUTH_SECRET", "x"), "secret");
        assert_eq!(kind("GCP_PRIVATE_KEY", "x"), "secret");
        assert_eq!(kind("GITHUB_TOKEN", "x"), "secret");
        assert_eq!(kind("DB_PASSWORD", "x"), "secret");
        assert_eq!(
            kind("MONGODB_URI", "mongodb+srv://app:pw@cluster0/db"),
            "secret"
        );
        assert_eq!(kind("REDIS_URL", "redis://:pw@host:6379"), "secret");
        assert_eq!(kind("PUBLIC_SITE_URL", "https://example.com"), "variable");
        assert_eq!(
            kind("API_BASE_URL", "https://api.example.com/v1?x=a@b"),
            "variable"
        );
        assert_eq!(kind("CERT", "line1\nline2"), "secret");
        assert_eq!(kind("PORT", "3000"), "skip");
    }

    #[test]
    fn credentials_in_urls() {
        assert!(url_has_credentials("postgres://u:p@h/db"));
        assert!(url_has_credentials("https://token@github.com/x"));
        assert!(!url_has_credentials("https://example.com/@user"));
        assert!(!url_has_credentials("not a url"));
    }
}
