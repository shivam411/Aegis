//! Where an app's code comes from: per-app SSH deploy keys for private
//! repositories, and repository access checks (`git ls-remote`).
//!
//! A deploy key's private half is sealed in the secrets store (same key file
//! as secrets, bound to the project), decrypted only to write the 0600 file
//! git uses while a clone or `ls-remote` runs (see `aegis_ssh`), and never
//! returned by the API or written to events, logs or releases. Events name
//! the key by its fingerprint.

use crate::{ControlError, ControlPlane, ControlResult};
use aegis_builder::SourceSpec;
use aegis_types::ProjectId;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use aegis_secrets::DeployKeyInfo;

/// How long an access check may take.
const LS_REMOTE_TIMEOUT: Duration = Duration::from_secs(20);

/// Where the short-lived deploy key files are written.
pub(crate) fn ssh_run_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("run").join("ssh")
}

/// With a deploy key, a GitHub HTTPS URL is cloned over SSH (deploy keys
/// only work there). Other sources are unchanged.
pub(crate) fn with_deploy_key_url(source: SourceSpec, has_key: bool) -> SourceSpec {
    match source {
        SourceSpec::Git { url, reference } if has_key => {
            let is_https = aegis_ssh::RemoteUrl::parse(&url).is_some_and(|r| !r.is_ssh());
            let url = match aegis_ssh::github_ssh_url(&url) {
                Some(ssh) if is_https => ssh,
                _ => url,
            };
            SourceSpec::Git { url, reference }
        }
        other => other,
    }
}

/// The result of checking whether a repository can be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessCheck {
    /// The repository could be listed.
    pub ok: bool,
    /// The URL that was checked (an SSH URL when a deploy key was used).
    pub url: String,
    /// Whether the deploy key was used.
    pub with_deploy_key: bool,
    /// Branch names seen (only when `ok`).
    pub branches: Vec<String>,
    /// Whether the app's branch exists (when known).
    pub branch_found: Option<bool>,
    /// git's error, when not `ok`.
    pub error: Option<String>,
    /// What to do next, in words.
    pub hint: Option<String>,
}

/// Explains common git/ssh failures.
fn hint_for(error: &str, with_key: bool, url: &str) -> String {
    let page = aegis_ssh::github_deploy_keys_page(url);
    if error.contains("Host key verification failed") || error.contains("No ED25519 host key") {
        return "The server's SSH host key is not known to Aegis. GitHub's keys are built in; for another git server add its key to the file named by [git] known_hosts_file in the daemon configuration (e.g. `ssh-keyscan -t ed25519 git.example.com`, after checking the fingerprint with the server's admin).".into();
    }
    if error.contains("Permission denied") || error.contains("publickey") {
        return match page {
            Some(page) => format!(
                "The repository did not accept the deploy key. Add the public key at {} (leave \"Allow write access\" unticked), then test again.",
                page
            ),
            None => "The server did not accept the deploy key. Add the public key to the repository's deploy keys (read-only), then test again.".into(),
        };
    }
    if error.contains("Repository not found")
        || error.contains("could not read Username")
        || error.contains("Authentication failed")
        || error.contains("terminal prompts disabled")
    {
        return if with_key {
            "The repository was not found with this key: check the URL and that the key was added to this repository.".into()
        } else {
            "The repository is private or does not exist. Generate a deploy key for the app and add it to the repository (read-only).".into()
        };
    }
    "git could not read the repository; check the URL and the server's network access.".into()
}

impl ControlPlane {
    fn known_project(
        &self,
        project_id: &ProjectId,
    ) -> ControlResult<aegis_projection::ProjectState> {
        self.inner
            .projection
            .get_project(project_id)
            .ok_or_else(|| ControlError::NotFound(format!("Unknown project {}", project_id)))
    }

    /// A project's deploy key (public half).
    pub fn deploy_key(&self, project_id: &ProjectId) -> Option<DeployKeyInfo> {
        self.inner.secrets.deploy_key(&project_id.to_string())
    }

    /// Generates a project's deploy key. An existing key is only replaced
    /// with `replace` (the old one stops working once removed from GitHub).
    pub async fn create_deploy_key(
        &self,
        project_id: ProjectId,
        replace: bool,
    ) -> ControlResult<DeployKeyInfo> {
        let project = self.known_project(&project_id)?;
        let previous = self.deploy_key(&project_id);
        if previous.is_some() && !replace {
            return Err(ControlError::NeedsConfirmation(format!(
                "'{}' already has a deploy key; generating a new one replaces it (add the new public key to the repository and remove the old one)",
                project.name
            )));
        }
        let comment = format!(
            "aegis:{}",
            project
                .name
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || "-_.".contains(*c))
                .collect::<String>()
        );
        let key = aegis_ssh::generate_deploy_key(&comment).map_err(|e| anyhow::anyhow!(e))?;
        let info = self
            .inner
            .secrets
            .set_deploy_key(
                &project_id.to_string(),
                &key.private_openssh,
                &key.public_openssh,
                &key.fingerprint,
            )
            .await?;
        self.record(
            "DeployKeyCreated",
            json!({
                "project_id": project_id.to_string(),
                "fingerprint": info.fingerprint,
                "replaced": previous.map(|p| p.fingerprint),
            }),
        )
        .await?;
        Ok(info)
    }

    /// Deletes a project's deploy key.
    pub async fn delete_deploy_key(&self, project_id: ProjectId) -> ControlResult<()> {
        self.known_project(&project_id)?;
        let removed = self
            .inner
            .secrets
            .remove_deploy_key(&project_id.to_string())
            .await?
            .ok_or_else(|| ControlError::NotFound("This app has no deploy key".into()))?;
        self.record(
            "DeployKeyDeleted",
            json!({
                "project_id": project_id.to_string(),
                "fingerprint": removed.fingerprint,
            }),
        )
        .await?;
        Ok(())
    }

    /// The project's deploy key, ready for git.
    pub(crate) fn ssh_auth(&self, project_id: &ProjectId) -> Option<aegis_ssh::SshAuth> {
        let (private, info) = self
            .inner
            .secrets
            .deploy_key_private(&project_id.to_string())?;
        Some(aegis_ssh::SshAuth::new(
            private,
            info.fingerprint,
            ssh_run_dir(&self.inner.settings.data_dir),
            self.inner.settings.known_hosts_file.clone(),
        ))
    }

    /// Checks that the project's repository can be read with its deploy key
    /// (`git ls-remote`). The result carries git's error, never key material.
    pub async fn test_deploy_key(&self, project_id: ProjectId) -> ControlResult<AccessCheck> {
        let project = self.known_project(&project_id)?;
        let auth = self.ssh_auth(&project_id).ok_or_else(|| {
            ControlError::FailedPrecondition(
                "This app has no deploy key; generate one first".into(),
            )
        })?;
        if project.repository_url.is_empty() {
            return Err(ControlError::FailedPrecondition(
                "This app has no repository URL".into(),
            ));
        }
        let url = aegis_ssh::url_for_deploy_key(&project.repository_url).ok_or_else(|| {
            ControlError::FailedPrecondition(format!(
                "A deploy key only works for SSH remotes (or GitHub URLs, which are converted); {} is neither",
                aegis_builder::redact_url(&project.repository_url)
            ))
        })?;
        let result = aegis_builder::ls_remote(&url, Some(&auth), LS_REMOTE_TIMEOUT).await;
        Ok(access_check(url, true, Some(&project.branch), result))
    }

    /// Whether a repository can be read without credentials. Only GitHub
    /// repositories are checked (as `https://github.com/<owner>/<repo>.git`),
    /// so this can't be pointed at other hosts.
    pub async fn check_public_access(
        &self,
        repository_url: &str,
        branch: Option<&str>,
    ) -> ControlResult<AccessCheck> {
        aegis_builder::validate_repository_url(repository_url)
            .map_err(|e| ControlError::InvalidArgument(e.to_string()))?;
        let url = aegis_ssh::github_https_url(repository_url).ok_or_else(|| {
            ControlError::InvalidArgument(
                "Only GitHub repository URLs can be checked (github.com/<owner>/<repo>)".into(),
            )
        })?;
        let result = aegis_builder::ls_remote(&url, None, LS_REMOTE_TIMEOUT).await;
        Ok(access_check(url, false, branch, result))
    }
}

fn access_check(
    url: String,
    with_deploy_key: bool,
    branch: Option<&str>,
    result: Result<aegis_builder::RemoteRefs, String>,
) -> AccessCheck {
    match result {
        Ok(refs) => {
            let branch_found = branch.map(|b| refs.branches.iter().any(|x| x == b));
            let hint = match (branch_found, branch) {
                (Some(false), Some(b)) => Some(format!(
                    "The repository can be read, but it has no branch '{}'",
                    b
                )),
                _ => None,
            };
            AccessCheck {
                ok: true,
                url,
                with_deploy_key,
                branches: refs.branches,
                branch_found,
                error: None,
                hint,
            }
        }
        Err(error) => AccessCheck {
            ok: false,
            hint: Some(hint_for(&error, with_deploy_key, &url)),
            url,
            with_deploy_key,
            branches: Vec::new(),
            branch_found: None,
            error: Some(error),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_github_urls_are_cloned_over_ssh_with_a_key() {
        let git = |url: &str| SourceSpec::Git {
            url: url.into(),
            reference: "main".into(),
        };
        assert_eq!(
            with_deploy_key_url(git("https://github.com/o/r.git"), true),
            git("git@github.com:o/r.git")
        );
        assert_eq!(
            with_deploy_key_url(git("https://github.com/o/r.git"), false),
            git("https://github.com/o/r.git")
        );
        assert_eq!(
            with_deploy_key_url(git("ssh://git@host/o/r.git"), true),
            git("ssh://git@host/o/r.git")
        );
        assert_eq!(
            with_deploy_key_url(git("https://gitlab.com/o/r.git"), true),
            git("https://gitlab.com/o/r.git")
        );
        let dir = SourceSpec::LocalDir(PathBuf::from("/srv/app"));
        assert_eq!(with_deploy_key_url(dir.clone(), true), dir);
    }

    #[test]
    fn access_hints() {
        let failed = |e: &str, key: bool| {
            access_check(
                "git@github.com:o/r.git".into(),
                key,
                None,
                Err(e.to_string()),
            )
        };
        let c = failed("git@github.com: Permission denied (publickey).", true);
        assert!(!c.ok);
        assert!(c
            .hint
            .unwrap()
            .contains("https://github.com/o/r/settings/keys/new"));
        let c = failed(
            "fatal: could not read Username for 'https://github.com': terminal prompts disabled",
            false,
        );
        assert!(c.hint.unwrap().contains("deploy key"));
        let c = failed("Host key verification failed.", true);
        assert!(c.hint.unwrap().contains("known_hosts_file"));
        let ok = access_check(
            "u".into(),
            true,
            Some("prod"),
            Ok(aegis_builder::RemoteRefs {
                branches: vec!["main".into()],
            }),
        );
        assert!(ok.ok);
        assert_eq!(ok.branch_found, Some(false));
        assert!(ok.hint.unwrap().contains("prod"));
    }
}
