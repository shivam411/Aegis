//! Container apps (`[run] runtime = "docker"`): the launcher their
//! processes run with, live resource limits through `docker update`,
//! Docker's status for capabilities and diagnostics, and cleanup of
//! release images and leftover containers.
//!
//! Everything else (health checks, graceful switch, proxy cutover,
//! rollback, restart policy, logs) is shared with process apps: the
//! supervisor runs a container like a process (see `aegis_docker`).

use crate::ControlPlane;
use aegis_config::ResourceLimits;
use aegis_docker::{ContainerSettings, DockerLauncher, DockerStatus};
use aegis_engine::ResolvedProjectConfig;
use aegis_process::{Launcher, ProcessStatus};
use aegis_projection::ProjectState;
use aegis_resources::{BackendKind, Capabilities};
use aegis_types::{ProcessId, ProjectId};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// How often Docker's availability is re-checked for the capabilities view.
const STATUS_REFRESH: Duration = Duration::from_secs(60);

impl ControlPlane {
    /// The launcher for a release that runs in a container, with a
    /// description for views (`docker: <image>`); `None` for process apps.
    /// `dir` is the release directory, named after the release version.
    pub(crate) fn runtime_launcher(
        &self,
        project_id: ProjectId,
        dir: &Path,
        config: &ResolvedProjectConfig,
    ) -> Option<(Arc<dyn Launcher>, String)> {
        if !config.is_docker() {
            return None;
        }
        let release = dir.file_name()?.to_string_lossy().to_string();
        let image = aegis_docker::args::image_tag(&project_id, &release);
        let resources = self.inner.resources.clone();
        let launcher = DockerLauncher::new(
            self.inner.docker.clone(),
            ContainerSettings {
                project_id,
                release,
                image: image.clone(),
                run: config.run.clone(),
            },
            Arc::new(move || resources.limits(&project_id)),
        );
        Some((Arc::new(launcher), format!("docker: {}", image)))
    }

    /// Docker's status as last checked (refreshed every minute).
    pub fn docker_status(&self) -> Option<DockerStatus> {
        self.inner.docker.status()
    }

    /// Checks Docker now.
    pub async fn check_docker(&self) -> DockerStatus {
        self.inner.docker.check().await
    }

    /// Whether the project's live release runs in a container.
    pub fn project_uses_docker(&self, project: &ProjectState) -> bool {
        if let Some(process) = project
            .current_process
            .and_then(|id| self.inner.projection.get_process(&id))
        {
            return process.runtime.as_deref() == Some("docker");
        }
        project
            .current_release
            .as_ref()
            .map(|v| self.switcher(project.id).get_version_dir(v))
            .and_then(|dir| ResolvedProjectConfig::load(&dir).ok())
            .is_some_and(|c| c.is_docker())
    }

    /// What limits Docker can enforce for container apps.
    pub(crate) fn docker_capabilities(&self, status: Option<&DockerStatus>) -> Capabilities {
        let available = status.is_some_and(|s| s.available);
        Capabilities {
            backend: BackendKind::Docker,
            cpu: available,
            memory: available,
            pids: available,
            location: None,
            reason: match status {
                Some(s) if s.available => None,
                Some(s) => s.reason.clone(),
                None => Some("Docker has not been checked yet".into()),
            },
        }
    }

    /// The limits this project can use: Docker's for container apps, the
    /// host's cgroups otherwise.
    pub fn project_capabilities(&self, project: &ProjectState) -> Capabilities {
        if self.project_uses_docker(project) {
            self.docker_capabilities(self.docker_status().as_ref())
        } else {
            self.resource_capabilities()
        }
    }

    /// Applies limits to the project's running containers at once. Returns
    /// how many containers were updated.
    pub(crate) async fn apply_docker_limits(
        &self,
        project_id: ProjectId,
        limits: &ResourceLimits,
    ) -> Result<usize, String> {
        let host = self.inner.host.latest();
        let containers: Vec<String> = self
            .inner
            .supervisor
            .list_processes()
            .into_iter()
            .filter(|p| p.project_id == project_id && p.status == ProcessStatus::Running)
            .filter_map(|p| p.runtime.and_then(|r| r.container))
            .collect();
        let mut errors = Vec::new();
        for name in &containers {
            if let Err(e) = self
                .inner
                .docker
                .update_limits(name, limits, host.cpu_count, host.memory_total_bytes)
                .await
            {
                errors.push(format!("{}: {}", name, e));
            }
        }
        if errors.is_empty() {
            Ok(containers.len())
        } else {
            Err(errors.join("; "))
        }
    }

    /// Keeps Docker's status fresh and removes the images of releases that
    /// retention archived.
    pub(crate) fn spawn_docker_janitor(&self) {
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                let Some(inner) = weak.upgrade() else { return };
                let docker = inner.docker.clone();
                drop(inner);
                docker.check().await;
                tokio::time::sleep(STATUS_REFRESH).await;
            }
        });
        let docker = self.inner.docker.clone();
        let mut rx = self.inner.store.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) if event.event_type == "ReleaseArchived" => {
                        let payload: Value =
                            serde_json::from_str(&event.payload_json).unwrap_or_default();
                        let (Some(project), Some(version)) = (
                            payload
                                .get("project_id")
                                .and_then(|v| v.as_str())
                                .and_then(|s| s.parse::<ProjectId>().ok()),
                            payload.get("version").and_then(|v| v.as_str()),
                        ) else {
                            continue;
                        };
                        if !docker.status().is_some_and(|s| s.available) {
                            continue;
                        }
                        let image = aegis_docker::args::image_tag(&project, version);
                        if docker.image_exists(&image).await {
                            match docker.remove_image(&image).await {
                                Ok(()) => {
                                    tracing::info!(%image, "Removed the image of an archived release")
                                }
                                Err(e) => {
                                    tracing::warn!(%image, error = %e, "Could not remove the image of an archived release")
                                }
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    /// Removes this installation's containers that no supervised process
    /// owns, e.g. after the daemon was killed. Only runs when some app uses
    /// Docker.
    pub(crate) async fn remove_orphan_containers(&self) {
        let any_docker = self
            .inner
            .projection
            .get_processes()
            .iter()
            .any(|p| p.runtime.as_deref() == Some("docker"));
        if !any_docker {
            return;
        }
        let docker = &self.inner.docker;
        if !docker.current_status().await.available {
            return;
        }
        let Ok(containers) = docker.list_containers().await else {
            return;
        };
        for (name, process) in containers {
            let supervised = process
                .parse::<ProcessId>()
                .ok()
                .is_some_and(|id| self.inner.supervisor.get(&id).is_some());
            if !supervised {
                tracing::info!(container = %name, "Removing a container left by an earlier run");
                docker.remove_container(&name).await;
            }
        }
    }
}
