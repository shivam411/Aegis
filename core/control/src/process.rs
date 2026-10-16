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
    /// project id/name meaning that project's current process (all of its
    /// instances when it runs several; a restart then goes one instance at
    /// a time).
    pub async fn process_action(
        &self,
        key: &str,
        action: ProcessAction,
    ) -> ControlResult<ProcessState> {
        if let Some(result) = self.project_instances_action(key, action).await {
            return result;
        }
        let process = self.resolve_process(key)?;
        let id = process.id;
        let lock = self.project_lock(process.project_id);
        let _guard = lock.lock().await;

        let project = self
            .inner
            .projection
            .get_project(&process.project_id)
            .ok_or_else(|| ControlError::NotFound("Process has no project".into()))?;
        let is_current = project.live_processes().contains(&id);
        let several = project.live_processes().len() > 1;
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
                let port = this.restart_port(&id);
                let port = this
                    .start_release_process(project.id, id, &release, &dir, &config, port)
                    .await
                    .map_err(ControlError::Internal)?;
                if several {
                    this.sync_routes(project.id);
                } else {
                    this.route_upstream(project.id, port);
                }
                Ok(())
            }
        };

        let running = self.is_running(&id);
        match action {
            ProcessAction::Stop if running => {
                if several {
                    // One instance of several: the others keep serving.
                    let others: Vec<u16> = project
                        .live_processes()
                        .iter()
                        .filter(|other| **other != id && self.is_running(other))
                        .filter_map(|other| {
                            self.inner.process_ports.lock().unwrap().get(other)?.port
                        })
                        .collect();
                    if let Some(ingress) = self.ingress() {
                        ingress.set_upstreams(&project.id.to_string(), others);
                    }
                }
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
                // Pick up secrets changed since the process started, keeping
                // the port the proxy routes to.
                if let Some(config) = &config {
                    let mut env = self.process_env(process.project_id, config);
                    let port = self.inner.process_ports.lock().unwrap().get(&id).copied();
                    if let Some(super::ProcessPort {
                        port: Some(port),
                        proxied: true,
                    }) = port
                    {
                        env.insert("PORT".to_string(), port.to_string());
                    }
                    self.inner.supervisor.set_env(&id, env);
                }
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
                let port = self.inner.process_ports.lock().unwrap().get(&id).copied();
                let candidate = self.inner.candidates.lock().unwrap().contains(&id);
                let instance = self
                    .inner
                    .process_instance
                    .lock()
                    .unwrap()
                    .get(&id)
                    .copied();
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
                        "port": port.and_then(|p| p.port),
                        "proxied": port.is_some_and(|p| p.proxied),
                        "candidate": candidate,
                        "instance": instance,
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
            // Every instance of the live release.
            let processes: Vec<ProcessState> = project
                .live_processes()
                .iter()
                .filter_map(|id| self.inner.projection.get_process(id))
                .collect();
            let several = processes.len() > 1;
            for process in processes {
                if self.recover_process(&project, process, several).await {
                    restarted += 1;
                }
            }
            if several || self.is_proxied(&project.id) {
                restarted += self.top_up_instances(&project).await;
            }
        }
        restarted
    }

    /// Restarts one recorded process of a project's live release, unless
    /// its operator stopped it. Returns whether it was started.
    async fn recover_process(
        &self,
        project: &aegis_projection::ProjectState,
        process: ProcessState,
        several: bool,
    ) -> bool {
        if process.desired != "running" || self.inner.supervisor.get(&process.id).is_some() {
            return false;
        }
        // Processes from before releases were tracked can't be rebuilt.
        let Some(version) = process.release_version.clone() else {
            return false;
        };
        let dir = self.switcher(project.id).get_version_dir(&version);
        if !dir.is_dir() {
            tracing::warn!(project = %project.name, version = %version, "Release directory missing; not restarting");
            return false;
        }
        if let Some(pid) = process.pid {
            kill_orphan(pid, &dir).await;
        }
        let config = match ResolvedProjectConfig::load(&dir) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(project = %project.name, error = %e, "Invalid aegis.toml in release; not restarting");
                return false;
            }
        };
        if let Some(number) = process.instance {
            self.inner
                .process_instance
                .lock()
                .unwrap()
                .insert(process.id, number);
        }
        let port = self.restart_port(&process.id);
        match self
            .start_release_process(project.id, process.id, &version, &dir, &config, port)
            .await
        {
            Ok(port) => {
                if several {
                    self.sync_routes(project.id);
                } else {
                    self.route_upstream(project.id, port);
                }
                tracing::info!(project = %project.name, version = %version, "Restored app process");
                true
            }
            Err(e) => {
                tracing::error!(project = %project.name, error = %e, "Failed to restore app process");
                false
            }
        }
    }

    /// Stops all apps for a daemon shutdown. They are marked as still wanted,
    /// so [`ControlPlane::recover`] starts them again on the next boot.
    pub async fn shutdown(&self) {
        self.inner.shutting_down.store(true, Ordering::SeqCst);
        // Keep the minute of metrics in progress.
        self.inner.history.flush().await;
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
