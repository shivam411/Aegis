//! Deleting an app, and changing where its code comes from.
//!
//! Deleting stops every process of the app (taking it out of the proxy
//! first, so in-flight requests drain), records `ProjectDeleted`, and then
//! removes what belongs to it: secrets, deploy key, webhook secret,
//! resource limits, and (unless asked to keep them) its release directories,
//! logs and artifact metadata. The events stay as the audit trail; the
//! projection drops the project, so its name and domains can be used again
//! right away. Stored ACME certificates are kept (as when a domain is
//! removed), so mapping the domain again doesn't need a new certificate.
//!
//! The cleanup after the event is repeated on every boot for deleted
//! projects (see [`ControlPlane::purge_deleted_projects`]), so a crash in
//! between leaves nothing behind, and recovery never starts the app again:
//! its processes are gone from the projection.

use crate::{validate_source, ControlError, ControlPlane, ControlResult};
use aegis_engine::ResolvedProjectConfig;
use aegis_process::StopReason;
use aegis_projection::{DeletedProject, ProjectState};
use aegis_types::{ProcessId, ProjectId};
use serde_json::json;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Used when neither the dashboard settings nor the live release say.
const DEFAULT_DRAIN: Duration = Duration::from_secs(10);

/// What deleting an app did.
#[derive(Debug, Clone)]
pub struct DeletedApp {
    pub project_id: String,
    pub name: String,
    /// Processes (instances, standby, candidates) that were stopped.
    pub stopped_processes: usize,
    /// Domains the proxy no longer serves for it.
    pub domains: Vec<String>,
    pub secrets_removed: u64,
    /// Release directories were kept on disk (`keep_releases`).
    pub releases_kept: bool,
    /// Where the kept releases and logs are (with `keep_releases`).
    pub kept_dir: Option<String>,
}

/// A new source for an existing app: a git repository (URL, branch and
/// optional subdirectory) or a folder on the server (and optional
/// subdirectory). Exactly one of `repository_url` and `source_dir`.
#[derive(Debug, Clone, Default)]
pub struct SourceChange {
    pub repository_url: Option<String>,
    pub branch: Option<String>,
    pub source_dir: Option<PathBuf>,
    pub source_subdir: Option<String>,
}

impl ControlPlane {
    fn active_deployment(
        &self,
        project_id: &ProjectId,
    ) -> Option<aegis_projection::DeploymentState> {
        self.inner
            .projection
            .get_deployments()
            .into_iter()
            .find(|d| {
                &d.project_id == project_id && matches!(d.status.as_str(), "Queued" | "InProgress")
            })
    }

    /// How long the app's processes get to finish in-flight requests: the
    /// dashboard setting, else the live release's aegis.toml.
    fn drain_of(&self, project: &ProjectState) -> Duration {
        if let Some(secs) = project.settings.drain_timeout_secs {
            return Duration::from_secs(secs);
        }
        project
            .current_release
            .as_ref()
            .and_then(|v| {
                ResolvedProjectConfig::load(&self.switcher(project.id).get_version_dir(v)).ok()
            })
            .map(|c| c.drain_timeout)
            .unwrap_or(DEFAULT_DRAIN)
    }

    /// Deletes an app (see the module docs). Refused while one of its
    /// deployments is queued or running, or while another operation on it
    /// (rollback, restart) holds it.
    pub async fn delete_project(
        &self,
        project_id: ProjectId,
        keep_releases: bool,
    ) -> ControlResult<DeletedApp> {
        let refuse_busy = |this: &Self, name: &str| -> ControlResult<()> {
            match this.active_deployment(&project_id) {
                Some(d) => Err(ControlError::FailedPrecondition(format!(
                    "A deployment of '{}' is {} ({}); wait until it has finished before deleting the app",
                    name,
                    if d.status == "Queued" { "queued" } else { "running" },
                    d.id
                ))),
                None => Ok(()),
            }
        };
        let project = self
            .inner
            .projection
            .get_project(&project_id)
            .ok_or_else(|| ControlError::NotFound(format!("Unknown project {}", project_id)))?;
        refuse_busy(self, &project.name)?;
        let lock = self.project_lock(project_id);
        let Ok(_guard) = lock.try_lock() else {
            return Err(ControlError::FailedPrecondition(format!(
                "'{}' is busy (a deployment, rollback or restart is in progress); try again when it has finished",
                project.name
            )));
        };
        // Re-read under the lock: a deployment may have been queued meanwhile.
        let project = self
            .inner
            .projection
            .get_project(&project_id)
            .ok_or_else(|| ControlError::NotFound(format!("Unknown project {}", project_id)))?;
        refuse_busy(self, &project.name)?;
        let key = project_id.to_string();
        let drain = self.drain_of(&project);
        let domains: Vec<String> = project.domains.iter().map(|d| d.domain.clone()).collect();

        // 1. No new requests: take the app's domains out of the proxy.
        {
            let _domains = self.inner.domain_lock.lock().await;
            if let Some(ingress) = self.ingress() {
                for domain in &domains {
                    ingress.remove_domain(domain);
                }
            }
        }
        // 2. A BlueGreen standby, then every other process it has.
        self.drop_standby(project_id, "deleted").await;
        let ids: Vec<ProcessId> = self
            .inner
            .supervisor
            .list_processes()
            .into_iter()
            .filter(|p| p.project_id == project_id)
            .map(|p| p.id)
            .collect();
        let running: Vec<ProcessId> = ids
            .iter()
            .copied()
            .filter(|id| self.is_running(id))
            .collect();
        let ports: Vec<u16> = {
            let known = self.inner.process_ports.lock().unwrap();
            running
                .iter()
                .filter_map(|id| known.get(id).and_then(|p| p.port))
                .collect()
        };
        let deadline = Instant::now() + drain;
        if let Some(ingress) = self.ingress() {
            ingress.set_upstreams(&key, Vec::new());
            for port in &ports {
                let left = deadline.saturating_duration_since(Instant::now());
                if !ingress.wait_idle(*port, left).await {
                    tracing::warn!(
                        port,
                        "Deleted app still had requests in flight after its drain timeout"
                    );
                }
            }
        }
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_secs(1));
        // "requested": should anything below fail, the processes stay
        // stopped across a daemon restart.
        self.stop_processes(&running, remaining, StopReason::Requested)
            .await;
        if let Some(still) = running.iter().find(|id| self.is_running(id)) {
            return Err(ControlError::Internal(anyhow::anyhow!(
                "Process {} of '{}' did not stop; the app was not deleted",
                still,
                project.name
            )));
        }
        for id in &ids {
            self.inner.supervisor.forget(id);
            self.inner.process_ports.lock().unwrap().remove(id);
            self.inner.process_release.lock().unwrap().remove(id);
            self.inner.process_instance.lock().unwrap().remove(id);
            self.inner.process_usage.lock().unwrap().remove(id);
            self.inner.candidates.lock().unwrap().remove(id);
        }
        if let Some(ingress) = self.ingress() {
            ingress.set_upstreams(&key, Vec::new());
        }

        // 3. Record it: from here on the app is gone (and stays gone on
        // restart). The events before it stay as its history.
        let kept_dir = keep_releases.then(|| self.project_dir(project_id));
        let versions: Vec<String> = self
            .inner
            .projection
            .get_project_releases(&project_id)
            .into_iter()
            .map(|r| r.version)
            .collect();
        self.record(
            "ProjectDeleted",
            json!({
                "project_id": key,
                "name": project.name,
                "keep_releases": keep_releases,
                "kept_dir": kept_dir.as_ref().map(|d| d.to_string_lossy().to_string()),
                "domains": domains,
                "repository_url": project.repository_url,
                "source_dir": project.source_dir,
                "stopped_process_ids": running.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
                "release_count": versions.len(),
            }),
        )
        .await?;

        // 4. Everything that belonged to it.
        let deleted = self
            .inner
            .projection
            .deleted_projects()
            .into_iter()
            .find(|d| d.id == project_id)
            .ok_or_else(|| ControlError::Internal(anyhow::anyhow!("Deletion was not recorded")))?;
        let secrets_removed = match self.purge_deleted(&deleted).await {
            Ok(n) => n,
            Err(e) => {
                tracing::error!(project = %project.name, error = %e, "Cleaning up a deleted app failed; retried on the next start");
                0
            }
        };
        if !keep_releases {
            self.remove_release_images(project_id, &versions).await;
        }
        self.inner.project_locks.lock().unwrap().remove(&project_id);
        tracing::info!(project = %project.name, keep_releases, "App deleted");
        Ok(DeletedApp {
            project_id: key,
            name: project.name,
            stopped_processes: running.len(),
            domains,
            secrets_removed,
            releases_kept: keep_releases,
            kept_dir: kept_dir.map(|d| d.to_string_lossy().to_string()),
        })
    }

    /// Removes what a deleted project left: secrets, deploy key, webhook
    /// secret, resource limits and (unless kept) its files. Idempotent.
    /// Returns how many secrets were removed.
    async fn purge_deleted(&self, deleted: &DeletedProject) -> anyhow::Result<u64> {
        let key = deleted.id.to_string();
        let secrets = self.inner.secrets.remove_project(&key).await?;
        // Kept by the auth store in the same database.
        sqlx::query("DELETE FROM webhook_secrets WHERE project_id = ?")
            .bind(&key)
            .execute(self.inner.store.pool())
            .await?;
        self.inner
            .resources
            .load_limits(deleted.id, aegis_config::ResourceLimits::default());
        if !deleted.keep_releases {
            let dir = self.project_dir(deleted.id);
            match tokio::fs::remove_dir_all(&dir).await {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    anyhow::bail!("Cannot remove {}: {}", dir.display(), e)
                }
                _ => {}
            }
            for release in &deleted.release_ids {
                self.inner.artifact_store.remove(release).await?;
            }
        }
        Ok(secrets)
    }

    /// Removes the Docker images built for a deleted container app.
    async fn remove_release_images(&self, project_id: ProjectId, versions: &[String]) {
        let docker = &self.inner.docker;
        if versions.is_empty() || !docker.status().is_some_and(|s| s.available) {
            return;
        }
        for version in versions {
            let image = aegis_docker::args::image_tag(&project_id, version);
            if docker.image_exists(&image).await {
                if let Err(e) = docker.remove_image(&image).await {
                    tracing::warn!(%image, error = %e, "Could not remove the image of a deleted app");
                }
            }
        }
    }

    /// Finishes the cleanup of deleted projects (after a crash during a
    /// deletion, or a restore). Runs on every boot from
    /// [`ControlPlane::recover`].
    pub async fn purge_deleted_projects(&self) {
        for deleted in self.inner.projection.deleted_projects() {
            if let Err(e) = self.purge_deleted(&deleted).await {
                tracing::warn!(project = %deleted.name, error = %e, "Could not clean up a deleted app");
            }
        }
    }

    /// Changes where an app's code comes from: a git repository or a folder
    /// on the server. Validated like registration; takes effect at the next
    /// deployment. The deploy key is kept (unused for a folder).
    pub async fn change_source(
        &self,
        project_id: ProjectId,
        change: SourceChange,
    ) -> ControlResult<ProjectState> {
        let project = self
            .inner
            .projection
            .get_project(&project_id)
            .ok_or_else(|| ControlError::NotFound(format!("Unknown project {}", project_id)))?;
        let url = change
            .repository_url
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .to_string();
        let branch = change
            .branch
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .to_string();
        let dir = change
            .source_dir
            .filter(|d| !d.as_os_str().is_empty() && !d.to_string_lossy().trim().is_empty());
        match (url.is_empty(), dir.is_some()) {
            (false, true) => {
                return Err(ControlError::InvalidArgument(
                    "Give a repository URL or a folder on the server, not both".into(),
                ))
            }
            (true, false) => {
                return Err(ControlError::InvalidArgument(
                    "Give a repository URL or a folder on the server".into(),
                ))
            }
            _ => {}
        }
        if dir.is_some() && !branch.is_empty() {
            return Err(ControlError::InvalidArgument(
                "A branch only applies to a repository source".into(),
            ));
        }
        let source = validate_source(dir, &url, &branch, change.source_subdir.as_deref())?;
        let branch = match (&source.source_dir, branch.is_empty()) {
            // A folder has no branch; keep the recorded one for schedules.
            (Some(_), _) => project.branch.clone(),
            (None, false) => branch,
            (None, true) if !project.branch.is_empty() => project.branch.clone(),
            (None, true) => "main".to_string(),
        };
        let unchanged = project.repository_url == url
            && project.branch == branch
            && project.source_dir == source.source_dir
            && project.source_subdir == source.source_subdir;
        if unchanged {
            return Ok(project);
        }
        self.record(
            "ProjectSourceChanged",
            json!({
                "project_id": project_id.to_string(),
                "kind": if source.source_dir.is_some() { "folder" } else { "git" },
                "repository_url": url,
                "branch": branch,
                "source_dir": source.source_dir,
                "source_subdir": source.source_subdir,
                "previous": {
                    "repository_url": project.repository_url,
                    "branch": project.branch,
                    "source_dir": project.source_dir,
                    "source_subdir": project.source_subdir,
                },
            }),
        )
        .await?;
        self.inner
            .projection
            .get_project(&project_id)
            .ok_or_else(|| ControlError::Internal(anyhow::anyhow!("Project vanished")))
    }
}
