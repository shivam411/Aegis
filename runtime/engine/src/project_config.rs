//! The effective settings for building and running a project: values from
//! `aegis.toml`, with gaps filled from runtime detection.

use crate::DetectorPipeline;
use aegis_config::ProjectFile;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

const DEFAULT_HEALTH_TIMEOUT_SECS: u64 = 30;
const DEFAULT_DRAIN_TIMEOUT_SECS: u64 = 10;
const DEFAULT_BUILD_TIMEOUT_SECS: u64 = 30 * 60;

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedProjectConfig {
    pub project_id: Option<String>,
    pub name: String,
    pub runtime: String,
    pub install_command: Option<String>,
    pub build_command: Option<String>,
    pub test_command: Option<String>,
    pub start_command: String,
    pub build_timeout: Duration,
    pub strategy: String,
    pub port: Option<u16>,
    /// `None` means the release is healthy as long as its process stays up.
    pub health_check_url: Option<String>,
    pub health_check_timeout: Duration,
    pub drain_timeout: Duration,
    pub max_retained_versions: usize,
    pub restart_policy: String,
    pub env: BTreeMap<String, String>,
}

/// Treats empty or whitespace-only commands as "not set".
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

impl ResolvedProjectConfig {
    /// Combines an optional `aegis.toml` with detection results for `dir`.
    pub fn resolve(dir: &Path, file: Option<&ProjectFile>) -> Self {
        let detected = DetectorPipeline::new().detect_all(dir);
        let defaults = DetectorPipeline::new().to_project_file(&detected, "");
        let file = file.cloned().unwrap_or_default();

        let pick = |explicit: Option<String>, fallback: Option<String>| match explicit {
            Some(v) => non_empty(Some(v)),
            None => non_empty(fallback),
        };

        let port = file.deploy.port.or(defaults.deploy.port);
        let health_check_url = match file.deploy.health_check_url {
            Some(url) => non_empty(Some(url)),
            None => match file.deploy.port {
                // A custom port without a URL: probe that port, not the detector's.
                Some(p) => Some(format!(
                    "http://127.0.0.1:{}{}",
                    p, detected.health_endpoint
                )),
                None => defaults.deploy.health_check_url,
            },
        };

        Self {
            project_id: non_empty(file.project.id),
            name: non_empty(file.project.name).unwrap_or(detected.project_name.clone()),
            runtime: non_empty(file.project.runtime).unwrap_or(detected.runtime_engine.clone()),
            install_command: pick(file.build.install_command, detected.install_command.clone()),
            build_command: pick(
                file.build.build_command,
                Some(detected.build_command.clone()),
            ),
            test_command: non_empty(file.build.test_command),
            start_command: non_empty(file.build.start_command)
                .unwrap_or(detected.start_command.clone()),
            build_timeout: Duration::from_secs(
                file.build
                    .timeout_secs
                    .unwrap_or(DEFAULT_BUILD_TIMEOUT_SECS),
            ),
            strategy: non_empty(file.deploy.strategy).unwrap_or_else(|| "GracefulSwitch".into()),
            port,
            health_check_url,
            health_check_timeout: Duration::from_secs(
                file.deploy
                    .health_check_timeout_secs
                    .unwrap_or(DEFAULT_HEALTH_TIMEOUT_SECS),
            ),
            drain_timeout: Duration::from_secs(
                file.deploy
                    .drain_timeout_secs
                    .unwrap_or(DEFAULT_DRAIN_TIMEOUT_SECS),
            ),
            max_retained_versions: file.deploy.max_retained_versions.unwrap_or(2).max(1),
            restart_policy: non_empty(file.deploy.restart_policy)
                .unwrap_or_else(|| "always".into()),
            env: file.env,
        }
    }

    /// Reads `dir/aegis.toml` (if present) and resolves it.
    pub fn load(dir: &Path) -> Result<Self, anyhow::Error> {
        let file = ProjectFile::load_from_dir(dir)?;
        Ok(Self::resolve(dir, file.as_ref()))
    }

    /// Environment for the app process: `[env]` plus `PORT` when known.
    pub fn process_env(&self) -> BTreeMap<String, String> {
        let mut env = self.env.clone();
        if let Some(port) = self.port {
            env.entry("PORT".to_string())
                .or_insert_with(|| port.to_string());
        }
        env
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_defaults_without_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        let cfg = ResolvedProjectConfig::load(dir.path()).unwrap();
        assert_eq!(cfg.runtime, "Node.js");
        assert_eq!(
            cfg.install_command.as_deref(),
            Some("npm install --no-audit --no-fund")
        );
        assert_eq!(cfg.start_command, "npm start");
        assert_eq!(cfg.port, Some(3000));
        assert_eq!(
            cfg.health_check_url.as_deref(),
            Some("http://127.0.0.1:3000/health")
        );
        assert_eq!(cfg.process_env().get("PORT").unwrap(), "3000");
        assert_eq!(cfg.restart_policy, "always");
    }

    #[test]
    fn test_file_overrides_and_empty_values() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        std::fs::write(
            dir.path().join("aegis.toml"),
            r#"
[project]
id = "abc"
[build]
install_command = ""
start_command = "node server.js"
test_command = "npm test"
[deploy]
port = 4000
drain_timeout_secs = 3
[env]
PORT = "5000"
"#,
        )
        .unwrap();
        let cfg = ResolvedProjectConfig::load(dir.path()).unwrap();
        assert_eq!(cfg.project_id.as_deref(), Some("abc"));
        assert_eq!(cfg.install_command, None);
        assert_eq!(cfg.start_command, "node server.js");
        assert_eq!(cfg.test_command.as_deref(), Some("npm test"));
        assert_eq!(
            cfg.health_check_url.as_deref(),
            Some("http://127.0.0.1:4000/health")
        );
        assert_eq!(cfg.drain_timeout, Duration::from_secs(3));
        // An explicit PORT in [env] wins over [deploy].port.
        assert_eq!(cfg.process_env().get("PORT").unwrap(), "5000");

        std::fs::write(
            dir.path().join("aegis.toml"),
            "[deploy]\nhealth_check_url = \"\"\n",
        )
        .unwrap();
        let cfg = ResolvedProjectConfig::load(dir.path()).unwrap();
        assert_eq!(cfg.health_check_url, None);
    }
}
