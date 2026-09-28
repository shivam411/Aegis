//! Operator process control, lifecycle event recording, boot recovery and shutdown.

use crate::{ControlError, ControlPlane, ControlResult};
use aegis_engine::ResolvedProjectConfig;
use aegis_process::{ProcessEvent, StopReason};
use aegis_projection::ProcessState;
use aegis_types::ProcessId;
use serde_json::json;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

const SHUTDOWN_DRAIN: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessAction {
    Start,
    Stop,
    Restart,
}

impl ProcessAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Start => "Start",
            Self::Stop => "Stop",
            Self::Restart => "Restart",
        }
    }
}

impl ControlPlane {
    /// Starts, stops or restarts a process. `key` is a process id, or a
    /// project id/name meaning that project's current process.
    pub async fn process_action(
        &self,
        key: &str,
        action: ProcessAction,
    ) -> ControlResult<ProcessState> {
        let process = self.resolve_process(key)?;
        let id = process.id;
        let lock = self.project_lock(process.project_id);
        let _guard = lock.lock().await;

        let project = self
            .inner
            .projection
            .get_project(&process.project_id)
            .ok_or_else(|| ControlError::NotFound("Process has no project".into()))?;
        let is_current = project.current_process == Some(id);
        let release = process.release_version.clone();
        let dir = release
            .as_ref()
            .map(|v| self.switcher(process.project_id).get_version_dir(v));
        let config = dir
            .as_ref()
            .filter(|d| d.is_dir())
            .and_then(|d| ResolvedProjectConfig::load(d).ok());
        let drain = config
            .as_ref()
            .map(|c| c.drain_timeout)
            .unwrap_or(Duration::from_secs(10));

        self.record(
            &format!("Process{}Requested", action.as_str()),
            json!({"process_id": id.to_string(), "project_id": process.project_id.to_string()}),
        )
        .await?;

        let start = |this: &ControlPlane| {
            let (dir, config, release) = (dir.clone(), config.clone(), release.clone());
            let this = this.clone();
            async move {
                if !is_current {
                    return Err(ControlError::FailedPrecondition(
                        "This process belongs to a release that is no longer live; deploy or roll back instead".into(),
                    ));
                }
                let (Some(dir), Some(config), Some(release)) = (dir, config, release) else {
                    return Err(ControlError::FailedPrecondition(
                        "The release for this process is no longer on disk".into(),
                    ));
                };
                this.start_release_process(project.id, id, &release, &dir, &config)
                    .await
                    .map_err(ControlError::Internal)
            }
        };

        let running = self.is_running(&id);
        match action {
            ProcessAction::Stop if running => {
                self.inner
                    .supervisor
                    .stop(&id, drain, StopReason::Requested)
                    .await?;
            }
            ProcessAction::Stop => {
                // Not supervised right now; record the intent so it stays down.
                self.record(
                    "ProcessStopped",
                    json!({
                        "process_id": id.to_string(),
                        "project_id": process.project_id.to_string(),
                        "reason": "requested",
                    }),
                )
                .await?;
            }
            ProcessAction::Start if running => {
                return Err(ControlError::FailedPrecondition(
                    "Process is already running".into(),
                ))
            }
            ProcessAction::Start => start(self).await?,
            ProcessAction::Restart if running => {
                self.inner.supervisor.restart(&id, drain).await?;
            }
            ProcessAction::Restart => start(self).await?,
        }

        self.process_view(&id)
            .ok_or_else(|| ControlError::Internal(anyhow::anyhow!("Process vanished")))
    }

    pub(crate) fn spawn_process_event_recorder(&self) {
        let this = self.clone();
        let mut rx = self.inner.supervisor.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => this.record_process_event(event).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "Process event recorder lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    async fn record_process_event(&self, event: ProcessEvent) {
        let (event_type, payload) = match event {
            ProcessEvent::Started {
                id,
                project_id,
                pid,
                restart_count,
            } => {
                let info = self.inner.supervisor.get(&id);
                let release = self.inner.process_release.lock().unwrap().get(&id).cloned();
                (
                    "ProcessStarted",
                    json!({
                        "process_id": id.to_string(),
                        "project_id": project_id.to_string(),
                        "pid": pid,
                        "restart_count": restart_count,
                        "release_version": release,
                        "command": info.as_ref().map(|i| i.command.clone()),
                        "cwd": info.as_ref().map(|i| i.cwd.to_string_lossy().to_string()),
                    }),
                )
            }
            ProcessEvent::Exited {
                id,
                project_id,
                exit_code,
                will_restart,
                restart_delay,
            } => (
                "ProcessCrashed",
                json!({
                    "process_id": id.to_string(),
                    "project_id": project_id.to_string(),
                    "exit_code": exit_code,
                    "will_restart": will_restart,
                    "restart_delay_ms": restart_delay.as_millis() as u64,
                }),
            ),
            ProcessEvent::Failed {
                id,
                project_id,
                reason,
            } => (
                "ProcessFailed",
                json!({
                    "process_id": id.to_string(),
                    "project_id": project_id.to_string(),
                    "reason": reason,
                }),
            ),
            // Recorded by shutdown() itself, before the store goes away.
            ProcessEvent::Stopped {
                reason: StopReason::Shutdown,
                ..
            } => return,
            ProcessEvent::Stopped {
                id,
                project_id,
                reason,
                exit_code,
            } => (
                "ProcessStopped",
                json!({
                    "process_id": id.to_string(),
                    "project_id": project_id.to_string(),
                    "reason": reason.as_str(),
                    "exit_code": exit_code,
                }),
            ),
        };
        if let Err(e) = self.record(event_type, payload).await {
            tracing::error!(error = %e, event_type, "Failed to record process event");
        }
    }

    /// Brings the platform back to its recorded state after a daemon start:
    /// marks interrupted deployments as failed and restarts every app whose
    /// operator hasn't stopped it. Call after [`ControlPlane::start`].
    pub async fn recover(&self) -> usize {
        for dep in self.inner.projection.get_deployments() {
            if matches!(dep.status.as_str(), "Queued" | "InProgress") {
                let _ = self
                    .record(
                        "DeploymentFailed",
                        json!({
                            "deployment_id": dep.id.to_string(),
                            "project_id": dep.project_id.to_string(),
                            "reason": "Interrupted: the daemon stopped before this deployment finished",
                        }),
                    )
                    .await;
            }
        }

        let mut restarted = 0;
        for project in self.inner.projection.get_projects() {
            let Some(process) = project
                .current_process
                .and_then(|id| self.inner.projection.get_process(&id))
            else {
                continue;
            };
            if process.desired != "running" || self.inner.supervisor.get(&process.id).is_some() {
                continue;
            }
            // Processes from before releases were tracked can't be rebuilt.
            let Some(version) = process.release_version.clone() else {
                continue;
            };
            let dir = self.switcher(project.id).get_version_dir(&version);
            if !dir.is_dir() {
                tracing::warn!(project = %project.name, version = %version, "Release directory missing; not restarting");
                continue;
            }
            if let Some(pid) = process.pid {
                kill_orphan(pid, &dir).await;
            }
            let config = match ResolvedProjectConfig::load(&dir) {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(project = %project.name, error = %e, "Invalid aegis.toml in release; not restarting");
                    continue;
                }
            };
            match self
                .start_release_process(project.id, process.id, &version, &dir, &config)
                .await
            {
                Ok(()) => {
                    restarted += 1;
                    tracing::info!(project = %project.name, version = %version, "Restored app process");
                }
                Err(e) => {
                    tracing::error!(project = %project.name, error = %e, "Failed to restore app process")
                }
            }
        }
        restarted
    }

    /// Stops all apps for a daemon shutdown. They are marked as still wanted,
    /// so [`ControlPlane::recover`] starts them again on the next boot.
    pub async fn shutdown(&self) {
        self.inner.shutting_down.store(true, Ordering::SeqCst);
        let running: Vec<(ProcessId, aegis_types::ProjectId)> = self
            .inner
            .supervisor
            .list_processes()
            .into_iter()
            .filter(|p| self.is_running(&p.id))
            .map(|p| (p.id, p.project_id))
            .collect();
        self.inner
            .supervisor
            .stop_all(SHUTDOWN_DRAIN, StopReason::Shutdown)
            .await;
        for (id, project_id) in running {
            let _ = self
                .record(
                    "ProcessStopped",
                    json!({
                        "process_id": id.to_string(),
                        "project_id": project_id.to_string(),
                        "reason": "shutdown",
                    }),
                )
                .await;
        }
    }
}

/// After an unclean daemon exit, an app may still be running without a
/// supervisor. Stop it if it is really ours (same working directory) so the
/// restarted app can bind its port.
async fn kill_orphan(pid: u32, dir: &Path) {
    #[cfg(target_os = "linux")]
    {
        let Ok(cwd) = std::fs::read_link(format!("/proc/{}/cwd", pid)) else {
            return;
        };
        if dir.canonicalize().ok().as_deref() != Some(cwd.as_path()) {
            return;
        }
        tracing::warn!(
            pid,
            "Stopping orphaned app process from a previous daemon run"
        );
        // Supervised apps lead their own process group (pgid == pid).
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        for _ in 0..50 {
            if unsafe { libc::kill(pid as i32, 0) } != 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (pid, dir);
    }
}
