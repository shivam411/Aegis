//! Deployments and rollbacks.
//!
//! A deployment builds the new release in its own directory while the live
//! release keeps serving. Only after a successful build does the cutover
//! happen: stop the old process, start the new one, and wait for it to become
//! healthy. If it doesn't, the old release is started again.
//!
//! When the built-in proxy is enabled and the app has a domain, the cutover
//! is zero-downtime instead: each release gets its own port, the new one
//! starts next to the old one, and the proxy flips to it once it is healthy
//! (see `cutover_side_by_side`). Otherwise both releases would need the same
//! port, so there is a short window between stopping the old process and the
//! new one accepting connections.
//!
//! Apps with several instances, and the Rolling and BlueGreen strategies,
//! are handled in `rollout.rs`; `cutover` hands them over there.

use crate::{ControlError, ControlPlane, ControlResult, SUPPORTED_STRATEGIES};
use aegis_builder::{
    BuildFailure, BuildPipeline, BuildRequest, PipelineStage, SourceSpec, StageReporter,
    StageStatus,
};
use aegis_engine::ResolvedProjectConfig;
use aegis_health::{HealthChecker, HttpHealthChecker, TcpHealthChecker};
use aegis_process::{ProcessSpec, ProcessStatus, RestartPolicy, StopReason};
use aegis_projection::{ProjectState, ReleaseState};
use aegis_release::ReleaseSwitcher;
use aegis_types::{DeploymentId, Event, ProcessId, ProjectId, ReleaseId};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default)]
pub struct DeployRequest {
    pub project_id: ProjectId,
    pub branch: Option<String>,
    pub strategy: Option<String>,
    /// Copy the code from this directory.
    pub source_dir: Option<PathBuf>,
    /// Clone the code from this repository (at `commit`, else `branch`).
    pub repository_url: Option<String>,
    pub commit: Option<String>,
    /// Release name; generated from the time when omitted.
    pub version: Option<String>,
    /// Who or what asked, e.g. "cli" or "ScheduledAutoDeploy".
    pub trigger: String,
}

/// A built release that can be started.
pub(crate) struct TargetRelease {
    pub release_id: ReleaseId,
    pub version: String,
    pub dir: PathBuf,
    pub config: ResolvedProjectConfig,
    /// The deployment this cutover is part of (none for rollbacks).
    pub deployment_id: Option<DeploymentId>,
}

/// How a cutover ended when the new release didn't come up.
pub(crate) struct CutoverFailure {
    pub reason: String,
    /// Last output of the new release's process.
    pub log_tail: Vec<String>,
    /// The previous release is serving again.
    pub restored: Option<String>,
}

fn validate_strategy(strategy: &str) -> ControlResult<()> {
    if SUPPORTED_STRATEGIES.contains(&strategy) {
        return Ok(());
    }
    Err(ControlError::InvalidArgument(format!(
        "Unsupported strategy '{}'. Supported: {}. GracefulSwitch is zero-downtime for apps served by the built-in proxy; Rolling and BlueGreen also need the built-in proxy.",
        strategy,
        SUPPORTED_STRATEGIES.join(", ")
    )))
}

pub(crate) fn drain_for(strategy: &str, config: &ResolvedProjectConfig) -> Duration {
    if strategy == "Immediate" {
        Duration::ZERO
    } else {
        config.drain_timeout
    }
}

struct StageRecorder<'a> {
    control: &'a ControlPlane,
    project_id: ProjectId,
    deployment_id: DeploymentId,
}

#[async_trait]
impl StageReporter for StageRecorder<'_> {
    async fn report(&self, stage: PipelineStage, status: StageStatus, detail: Option<String>) {
        let _ = self
            .control
            .record(
                &format!("BuildStage{}", stage.as_str()),
                json!({
                    "deployment_id": self.deployment_id.to_string(),
                    "project_id": self.project_id.to_string(),
                    "stage": stage.as_str(),
                    "status": status.as_str(),
                    "detail": detail,
                }),
            )
            .await;
    }
}

impl ControlPlane {
    /// Validates a deployment request and queues it. The deployment itself
    /// runs in the background; follow it via events or `get_deployment`.
    pub async fn queue_deployment(&self, req: DeployRequest) -> ControlResult<DeploymentId> {
        if let Some(strategy) = &req.strategy {
            validate_strategy(strategy)?;
            if let Some(project) = self.inner.projection.get_project(&req.project_id) {
                self.check_strategy_for(&project, strategy)
                    .map_err(ControlError::InvalidArgument)?;
            }
        }
        if let Some(version) = &req.version {
            ReleaseSwitcher::validate_version(version)
                .map_err(|e| ControlError::InvalidArgument(e.to_string()))?;
        }
        if let Some(url) = &req.repository_url {
            aegis_builder::validate_repository_url(url)
                .map_err(|e| ControlError::InvalidArgument(e.to_string()))?;
        }
        for reference in [&req.branch, &req.commit].into_iter().flatten() {
            aegis_builder::validate_git_ref(reference)
                .map_err(|e| ControlError::InvalidArgument(e.to_string()))?;
        }
        if let Some(dir) = &req.source_dir {
            if !dir.is_dir() {
                return Err(ControlError::InvalidArgument(format!(
                    "Source directory {} does not exist",
                    dir.display()
                )));
            }
        }

        if self.inner.projection.get_project(&req.project_id).is_none() {
            self.auto_register(&req).await?;
        }

        let deployment_id = DeploymentId::new();
        self.record(
            "DeploymentQueued",
            json!({
                "deployment_id": deployment_id.to_string(),
                "project_id": req.project_id.to_string(),
                "branch": req.branch,
                "strategy": req.strategy,
                "source_dir": req.source_dir.map(|d| d.to_string_lossy().to_string()),
                "repository_url": req.repository_url,
                "commit_sha": req.commit,
                "version": req.version,
                "trigger_source": req.trigger,
            }),
        )
        .await?;
        Ok(deployment_id)
    }

    /// Registers a project on first deploy when its source directory has an
    /// `aegis.toml` naming this project id.
    async fn auto_register(&self, req: &DeployRequest) -> ControlResult<()> {
        let unknown = || {
            ControlError::NotFound(format!(
                "Unknown project {}. Run 'aegis init' first.",
                req.project_id
            ))
        };
        let dir = req.source_dir.as_ref().ok_or_else(unknown)?;
        let file = aegis_config::ProjectFile::load_from_dir(dir)
            .map_err(|e| ControlError::InvalidArgument(format!("Invalid aegis.toml: {}", e)))?
            .ok_or_else(unknown)?;
        if file.project.id.as_deref() != Some(req.project_id.to_string().as_str()) {
            return Err(unknown());
        }
        let config = ResolvedProjectConfig::resolve(dir, Some(&file));
        self.register_project(crate::RegisterProject {
            project_id: Some(req.project_id),
            name: config.name,
            repository_url: req.repository_url.clone().unwrap_or_default(),
            branch: req.branch.clone().unwrap_or_else(|| "main".into()),
            runtime: Some(config.runtime),
            source_dir: Some(dir.clone()),
        })
        .await?;
        Ok(())
    }

    pub(crate) fn spawn_dispatcher(&self) {
        let this = self.clone();
        let mut rx = self.inner.store.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) if event.event_type == "DeploymentQueued" => {
                        let this = this.clone();
                        tokio::spawn(async move { this.run_deployment(event).await });
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::error!(
                            skipped = n,
                            "Deployment dispatcher lagged; queued deployments may have been missed"
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    /// Records a failed deployment. Recording errors are only logged.
    async fn fail_deployment(&self, dep: DeploymentId, project: ProjectId, extra: Value) {
        let mut payload = json!({
            "deployment_id": dep.to_string(),
            "project_id": project.to_string(),
        });
        if let (Some(obj), Value::Object(more)) = (payload.as_object_mut(), extra) {
            obj.extend(more);
        }
        if let Err(e) = self.record("DeploymentFailed", payload).await {
            tracing::error!(error = %e, "Failed to record DeploymentFailed");
        }
    }

    async fn run_deployment(&self, event: Event) {
        let payload: Value = serde_json::from_str(&event.payload_json).unwrap_or_default();
        let str_of = |k: &str| {
            payload
                .get(k)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let Some(project_id) = str_of("project_id").and_then(|s| s.parse::<ProjectId>().ok())
        else {
            tracing::error!(event_id = %event.id, "DeploymentQueued without a valid project_id");
            return;
        };
        // Older clients don't send a deployment id; derive a fresh one.
        let deployment_id = str_of("deployment_id")
            .and_then(|s| s.parse().ok())
            .unwrap_or_default();

        let lock = self.project_lock(project_id);
        let _guard = lock.lock().await;

        if self
            .inner
            .shutting_down
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            self.fail_deployment(
                deployment_id,
                project_id,
                json!({"reason": "The daemon is shutting down"}),
            )
            .await;
            return;
        }
        let Some(project) = self.inner.projection.get_project(&project_id) else {
            self.fail_deployment(
                deployment_id,
                project_id,
                json!({"reason": format!("Unknown project {}", project_id)}),
            )
            .await;
            return;
        };

        // The request's strategy, else the dashboard setting, else the default.
        let strategy = str_of("strategy")
            .or_else(|| project.settings.strategy.clone().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| "GracefulSwitch".to_string());
        if let Err(e) = validate_strategy(&strategy) {
            self.fail_deployment(deployment_id, project_id, json!({"reason": e.to_string()}))
                .await;
            return;
        }
        if let Err(reason) = self.check_strategy_for(&project, &strategy) {
            self.fail_deployment(deployment_id, project_id, json!({"reason": reason}))
                .await;
            return;
        }
        let branch = str_of("branch").unwrap_or_else(|| project.branch.clone());
        let source = match self.pick_source(&project, &payload, &branch) {
            Ok(s) => s,
            Err(reason) => {
                self.fail_deployment(deployment_id, project_id, json!({"reason": reason}))
                    .await;
                return;
            }
        };
        let switcher = self.switcher(project_id);
        let version = match str_of("version") {
            Some(v) => v,
            None => generate_version(&switcher),
        };
        // Events can also arrive via EmitEvent, bypassing queue_deployment's checks.
        if let Err(e) = ReleaseSwitcher::validate_version(&version) {
            self.fail_deployment(deployment_id, project_id, json!({"reason": e.to_string()}))
                .await;
            return;
        }
        let release_dir = switcher.get_version_dir(&version);

        tracing::info!(deployment_id = %deployment_id, project = %project.name, version = %version, "Deployment started");
        let _ = self
            .record(
                "DeploymentStarted",
                json!({
                    "deployment_id": deployment_id.to_string(),
                    "project_id": project_id.to_string(),
                    "version": version,
                    "strategy": strategy,
                }),
            )
            .await;

        let reporter = StageRecorder {
            control: self,
            project_id,
            deployment_id,
        };
        let request = BuildRequest {
            project_id,
            deployment_id,
            version: version.clone(),
            branch: branch.clone(),
            source,
            release_dir: release_dir.clone(),
            log_path: self.deployment_log_path(project_id, deployment_id),
            overrides: project.settings.clone(),
        };
        let output = match BuildPipeline::new(&self.inner.artifact_store, &reporter)
            .run(&request)
            .await
        {
            Ok(o) => o,
            Err(BuildFailure {
                stage,
                message,
                log_tail,
            }) => {
                // Only remove what this build created; a name clash is left alone.
                if !message.contains("already exists") {
                    let _ = tokio::fs::remove_dir_all(&release_dir).await;
                }
                self.fail_deployment(
                    deployment_id,
                    project_id,
                    json!({
                        "stage": stage.as_str(),
                        "reason": message,
                        "log_tail": log_tail,
                        "log_path": request.log_path.to_string_lossy(),
                    }),
                )
                .await;
                return;
            }
        };

        let release = &output.release;
        let _ = self
            .record(
                "ReleaseCreated",
                json!({
                    "release_id": release.id.to_string(),
                    "project_id": project_id.to_string(),
                    "deployment_id": deployment_id.to_string(),
                    "version": version,
                    "commit_sha": release.commit_sha,
                    "commit_message": release.commit_message,
                    "author": release.author,
                    "branch": branch,
                    "runtime": release.build_metadata.runtime,
                    "checksum": release.checksums.values().next(),
                    "artifacts": release.artifacts.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
                    "path": release_dir.to_string_lossy(),
                    "build_duration_ms": release.build_metadata.build_duration_ms,
                }),
            )
            .await;

        reporter
            .report(PipelineStage::Promote, StageStatus::Started, None)
            .await;
        let target = TargetRelease {
            release_id: release.id,
            version: version.clone(),
            dir: release_dir,
            config: output.config,
            deployment_id: Some(deployment_id),
        };
        match self.cutover(&project, &target, &strategy).await {
            Ok(()) => {
                reporter
                    .report(PipelineStage::Promote, StageStatus::Success, None)
                    .await;
                let _ = self
                    .record(
                        "DeploymentCompleted",
                        json!({
                            "deployment_id": deployment_id.to_string(),
                            "project_id": project_id.to_string(),
                            "release_id": release.id.to_string(),
                            "version": version,
                            "status": "Success",
                        }),
                    )
                    .await;
                tracing::info!(deployment_id = %deployment_id, version = %version, "Deployment completed");
                self.prune_releases(project_id, target.config.max_retained_versions)
                    .await;
            }
            Err(failure) => {
                reporter
                    .report(
                        PipelineStage::Promote,
                        StageStatus::Failed,
                        Some(failure.reason.clone()),
                    )
                    .await;
                self.fail_deployment(
                    deployment_id,
                    project_id,
                    json!({
                        "stage": "Promote",
                        "reason": failure.reason,
                        "log_tail": failure.log_tail,
                        "rolled_back_to": failure.restored,
                    }),
                )
                .await;
                if let Some(restored) = failure.restored {
                    let _ = self
                        .record(
                            "DeploymentRolledBack",
                            json!({
                                "deployment_id": deployment_id.to_string(),
                                "project_id": project_id.to_string(),
                                "target_version": restored,
                                "reason": "New release failed its health check",
                            }),
                        )
                        .await;
                }
            }
        }
    }

    fn pick_source(
        &self,
        project: &ProjectState,
        payload: &Value,
        branch: &str,
    ) -> Result<SourceSpec, String> {
        let get = |k: &str| {
            payload
                .get(k)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        };
        // "working_dir" is what older CLIs sent.
        if let Some(dir) = get("source_dir").or_else(|| get("working_dir")) {
            return Ok(SourceSpec::LocalDir(PathBuf::from(dir)));
        }
        let reference = get("commit_sha").unwrap_or(branch).to_string();
        if let Some(url) = get("repository_url") {
            return Ok(SourceSpec::Git {
                url: url.to_string(),
                reference,
            });
        }
        if let Some(dir) = &project.source_dir {
            return Ok(SourceSpec::LocalDir(PathBuf::from(dir)));
        }
        if !project.repository_url.is_empty() {
            return Ok(SourceSpec::Git {
                url: project.repository_url.clone(),
                reference,
            });
        }
        Err(format!(
            "Project '{}' has no source directory or repository URL",
            project.name
        ))
    }

    /// Builds the process spec for running a release. `proxied_port` is the
    /// port allocated for the release behind the proxy; it replaces `PORT`
    /// (even one set in `[env]`), so two releases can run side by side.
    pub(crate) fn process_spec(
        &self,
        project_id: ProjectId,
        process_id: ProcessId,
        dir: &Path,
        config: &ResolvedProjectConfig,
        proxied_port: Option<u16>,
    ) -> ProcessSpec {
        let mut env = self.process_env(project_id, config);
        if let Some(port) = proxied_port {
            env.insert("PORT".to_string(), port.to_string());
        }
        // Container apps run through a launcher (see docker.rs).
        let runtime = self.runtime_launcher(project_id, dir, config);
        ProcessSpec {
            project_id,
            command: runtime
                .as_ref()
                .map_or_else(|| config.start_command.clone(), |(_, what)| what.clone()),
            cwd: dir.to_path_buf(),
            env,
            restart_policy: RestartPolicy::parse(&config.restart_policy)
                .unwrap_or(RestartPolicy::Always),
            log_file: Some(self.process_log_path(project_id, process_id)),
            launcher: runtime.map(|(launcher, _)| launcher),
        }
    }

    /// Starts a release's process under a known id and remembers its release
    /// and port. With `proxied_port`, the app listens there instead of on its
    /// configured port. Returns the port the app was told to use.
    pub(crate) async fn start_release_process(
        &self,
        project_id: ProjectId,
        process_id: ProcessId,
        version: &str,
        dir: &Path,
        config: &ResolvedProjectConfig,
        proxied_port: Option<u16>,
    ) -> anyhow::Result<Option<u16>> {
        self.inner
            .process_release
            .lock()
            .unwrap()
            .insert(process_id, version.to_string());
        let spec = self.process_spec(project_id, process_id, dir, config, proxied_port);
        let port = spec.env.get("PORT").and_then(|p| p.parse().ok());
        self.inner.process_ports.lock().unwrap().insert(
            process_id,
            crate::ProcessPort {
                port,
                proxied: proxied_port.is_some(),
            },
        );
        self.inner
            .supervisor
            .start_with_id(process_id, spec)
            .await
            .map(|_| port)
    }

    /// The proxied port to restart a recorded process on: the one it had,
    /// if it was proxied (or a fresh one if that port is taken now).
    pub(crate) fn restart_port(&self, process_id: &ProcessId) -> Option<u16> {
        let live = self
            .inner
            .process_ports
            .lock()
            .unwrap()
            .get(process_id)
            .copied();
        let (port, proxied) = match live {
            Some(p) => (p.port, p.proxied),
            None => {
                let state = self.inner.projection.get_process(process_id)?;
                (state.port, state.proxied)
            }
        };
        if !proxied {
            return None;
        }
        match port {
            Some(p) if crate::domains::port_is_free(p) => Some(p),
            _ => crate::domains::allocate_port().ok(),
        }
    }

    /// Replaces the project's running release with `target`, rolling back to
    /// the previous release if the new one doesn't become healthy.
    pub(crate) async fn cutover(
        &self,
        project: &ProjectState,
        target: &TargetRelease,
        strategy: &str,
    ) -> Result<(), CutoverFailure> {
        // A BlueGreen standby of an older release is no longer a rollback
        // target once another release goes live.
        self.drop_standby(project.id, "replaced").await;
        if let Some(result) = self.cutover_instances(project, target, strategy).await {
            return result;
        }
        if self.is_proxied(&project.id) {
            return self.cutover_side_by_side(project, target, strategy).await;
        }
        let project_id = project.id;
        let previous = project.current_process.and_then(|id| {
            let state = self.inner.projection.get_process(&id)?;
            let version = state.release_version.clone()?;
            let dir = self.switcher(project_id).get_version_dir(&version);
            Some((id, version, dir))
        });
        let previous_running = previous
            .as_ref()
            .is_some_and(|(id, _, _)| self.is_running(id));

        // 1. Stop the old process.
        if let (Some((id, _, dir)), true) = (&previous, previous_running) {
            let drain = ResolvedProjectConfig::load(dir)
                .map(|c| drain_for(strategy, &c))
                .unwrap_or_else(|_| drain_for(strategy, &target.config));
            if let Err(e) = self
                .inner
                .supervisor
                .stop(id, drain, StopReason::Replaced)
                .await
            {
                return Err(CutoverFailure {
                    reason: format!("Could not stop the running release: {}", e),
                    log_tail: Vec::new(),
                    restored: None,
                });
            }
        }

        // 2. Start the new one and wait for it to be healthy.
        let new_id = ProcessId::new();
        let started = self
            .start_release_process(
                project_id,
                new_id,
                &target.version,
                &target.dir,
                &target.config,
                None,
            )
            .await;
        let (health, new_port) = match started {
            Ok(port) => (self.wait_healthy(&new_id, &target.config, None).await, port),
            Err(e) => (Err(format!("Failed to start: {}", e)), None),
        };

        match health {
            Ok(()) => {
                self.route_upstream(project_id, new_port);
                self.promote(project, target, new_id, new_port).await;
                Ok(())
            }
            Err(reason) => {
                tracing::warn!(version = %target.version, reason = %reason, "New release is unhealthy");
                if self.inner.supervisor.get(&new_id).is_some() {
                    let _ = self
                        .inner
                        .supervisor
                        .stop(&new_id, Duration::from_secs(2), StopReason::Replaced)
                        .await;
                }
                let log_tail = self
                    .inner
                    .supervisor
                    .logs(&new_id, 20)
                    .into_iter()
                    .map(|l| l.line)
                    .collect();
                let mut restored = None;
                if let (Some((id, version, dir)), true) = (previous, previous_running) {
                    match ResolvedProjectConfig::load(&dir) {
                        Ok(config) => {
                            let port = self.restart_port(&id);
                            match self
                                .start_release_process(
                                    project_id, id, &version, &dir, &config, port,
                                )
                                .await
                            {
                                Ok(port) => {
                                    self.route_upstream(project_id, port);
                                    restored = Some(version)
                                }
                                Err(e) => {
                                    tracing::error!(error = %e, "Failed to restart previous release")
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "Previous release config is unreadable")
                        }
                    }
                }
                Err(CutoverFailure {
                    reason,
                    log_tail,
                    restored,
                })
            }
        }
    }

    /// Makes `target` the live release: release pointer, then `ReleasePromoted`
    /// (which also makes `process_id` the project's current process).
    async fn promote(
        &self,
        project: &ProjectState,
        target: &TargetRelease,
        process_id: ProcessId,
        port: Option<u16>,
    ) {
        if let Err(e) = self
            .switcher(project.id)
            .switch_to_version(&target.version)
            .await
        {
            tracing::error!(error = %e, "Failed to update current release pointer");
        }
        let _ = self
            .record(
                "ReleasePromoted",
                json!({
                    "release_id": target.release_id.to_string(),
                    "project_id": project.id.to_string(),
                    "version": target.version,
                    "previous_version": project.current_release,
                    "process_id": process_id.to_string(),
                    "port": port,
                }),
            )
            .await;
    }

    /// Zero-downtime cutover behind the proxy: the new release starts on its
    /// own port while the old one keeps serving. Once it is healthy, the
    /// proxy is flipped to it in one step; then the old process finishes its
    /// in-flight requests and is stopped. If the new release never gets
    /// healthy, it is stopped and the old one simply keeps serving.
    async fn cutover_side_by_side(
        &self,
        project: &ProjectState,
        target: &TargetRelease,
        strategy: &str,
    ) -> Result<(), CutoverFailure> {
        let project_id = project.id;
        let previous = project.current_process.filter(|id| self.is_running(id));
        let port = crate::domains::allocate_port().map_err(|e| CutoverFailure {
            reason: format!("No free port for the new release: {}", e),
            log_tail: Vec::new(),
            restored: None,
        })?;

        let new_id = ProcessId::new();
        self.inner.candidates.lock().unwrap().insert(new_id);
        let started = self
            .start_release_process(
                project_id,
                new_id,
                &target.version,
                &target.dir,
                &target.config,
                Some(port),
            )
            .await;
        let health = match started {
            Ok(_) => self.wait_healthy(&new_id, &target.config, Some(port)).await,
            Err(e) => Err(format!("Failed to start: {}", e)),
        };

        if let Err(reason) = health {
            self.inner.candidates.lock().unwrap().remove(&new_id);
            tracing::warn!(version = %target.version, reason = %reason, "New release is unhealthy; the live release keeps serving");
            if self.inner.supervisor.get(&new_id).is_some() {
                let _ = self
                    .inner
                    .supervisor
                    .stop(&new_id, Duration::from_secs(2), StopReason::Replaced)
                    .await;
            }
            let log_tail = self
                .inner
                .supervisor
                .logs(&new_id, 20)
                .into_iter()
                .map(|l| l.line)
                .collect();
            let restored = previous
                .filter(|id| self.is_running(id))
                .and_then(|_| project.current_release.clone());
            return Err(CutoverFailure {
                reason,
                log_tail,
                restored,
            });
        }

        // The flip: every request from now on goes to the new release.
        self.route_upstream(project_id, Some(port));
        self.inner.candidates.lock().unwrap().remove(&new_id);
        self.promote(project, target, new_id, Some(port)).await;

        if let Some(old) = previous {
            let drain = project
                .current_release
                .as_ref()
                .and_then(|v| {
                    ResolvedProjectConfig::load(&self.switcher(project_id).get_version_dir(v)).ok()
                })
                .map(|c| drain_for(strategy, &c))
                .unwrap_or_else(|| drain_for(strategy, &target.config));
            let old_port = self
                .inner
                .process_ports
                .lock()
                .unwrap()
                .get(&old)
                .and_then(|p| p.port);
            let started_draining = Instant::now();
            if let (Some(old_port), Some(ingress)) = (old_port, self.ingress()) {
                if old_port != port && !ingress.wait_idle(old_port, drain).await {
                    tracing::warn!(
                        port = old_port,
                        "Old release still had requests in flight after the drain timeout"
                    );
                }
            }
            let remaining = drain.saturating_sub(started_draining.elapsed());
            if let Err(e) = self
                .inner
                .supervisor
                .stop(
                    &old,
                    remaining.max(Duration::from_secs(1)),
                    StopReason::Replaced,
                )
                .await
            {
                tracing::error!(error = %e, "Failed to stop the previous release");
            }
        }
        Ok(())
    }

    pub(crate) fn is_running(&self, id: &ProcessId) -> bool {
        self.inner.supervisor.get(id).is_some_and(|p| {
            matches!(
                p.status,
                ProcessStatus::Running | ProcessStatus::Backoff | ProcessStatus::Stopping
            )
        })
    }

    /// Waits until the process answers its health check (or, without a
    /// health URL, stays up for the liveness window).
    pub(crate) async fn wait_healthy(
        &self,
        id: &ProcessId,
        config: &ResolvedProjectConfig,
        proxied_port: Option<u16>,
    ) -> Result<(), String> {
        let started = Instant::now();
        let deadline = started + config.health_check_timeout;
        let settings = &self.inner.settings;
        // Behind the proxy, probe the port this release was given.
        let health_url = config
            .health_check_url
            .as_ref()
            .map(|url| match proxied_port {
                Some(port) => crate::domains::rewrite_health_port(url, config.port, port),
                None => url.clone(),
            });
        let checker: Option<(Box<dyn HealthChecker>, String)> =
            health_url
                .as_ref()
                .map(|url| match url.strip_prefix("tcp://") {
                    Some(target) => (
                        Box::new(TcpHealthChecker::new(2000)) as Box<dyn HealthChecker>,
                        target.to_string(),
                    ),
                    None => (
                        Box::new(HttpHealthChecker::new(2000)) as Box<dyn HealthChecker>,
                        url.clone(),
                    ),
                });
        let window = settings.liveness_window.min(config.health_check_timeout);

        loop {
            let Some(info) = self.inner.supervisor.get(id) else {
                return Err("Process disappeared during startup".to_string());
            };
            match info.status {
                ProcessStatus::Running => {}
                _ => {
                    let code = info
                        .last_exit_code
                        .map_or("a signal".to_string(), |c| format!("code {}", c));
                    return Err(format!(
                        "Process exited with {} during startup (start command: {})",
                        code, config.start_command
                    ));
                }
            }
            match &checker {
                Some((checker, target)) => match checker.check_health(target).await {
                    Ok(true) => return Ok(()),
                    Ok(false) => {}
                    Err(e) => return Err(format!("Health check misconfigured: {}", e)),
                },
                None if started.elapsed() >= window => return Ok(()),
                None => {}
            }
            if Instant::now() >= deadline {
                return Err(match &health_url {
                    Some(url) => format!(
                        "Not healthy after {}s: no successful response from {}",
                        config.health_check_timeout.as_secs(),
                        url
                    ),
                    None => "Process did not stay up".to_string(),
                });
            }
            tokio::time::sleep(settings.health_poll_interval).await;
        }
    }

    /// Keeps the active release plus the newest inactive ones, up to `max`.
    async fn prune_releases(&self, project_id: ProjectId, max: usize) {
        // Keep the live release and the ones most recently live before it:
        // those are the rollback targets. Builds that never went live go first.
        let keep: Vec<String> = self
            .inner
            .projection
            .releases_by_recency(&project_id)
            .into_iter()
            .filter(|r| matches!(r.status.as_str(), "Active" | "Inactive"))
            .take(max.max(1))
            .map(|r| r.version)
            .collect();
        match self.switcher(project_id).prune_versions(&keep).await {
            Ok(removed) => {
                for version in removed {
                    let _ = self
                        .record(
                            "ReleaseArchived",
                            json!({"project_id": project_id.to_string(), "version": version}),
                        )
                        .await;
                }
            }
            Err(e) => tracing::warn!(error = %e, "Release pruning failed"),
        }
    }

    /// Switches the project back to an earlier release without rebuilding.
    /// Without `version`, targets the release that was live before the current one.
    pub async fn rollback(
        &self,
        project_id: ProjectId,
        version: Option<String>,
    ) -> ControlResult<String> {
        let lock = self.project_lock(project_id);
        let _guard = lock.lock().await;
        let project = self
            .inner
            .projection
            .get_project(&project_id)
            .ok_or_else(|| ControlError::NotFound(format!("Unknown project {}", project_id)))?;
        let current = project.current_release.clone().ok_or_else(|| {
            ControlError::FailedPrecondition(format!(
                "Project '{}' has no live release to roll back from",
                project.name
            ))
        })?;
        let switcher = self.switcher(project_id);
        let releases = self.inner.projection.get_project_releases(&project_id);
        let target: ReleaseState = match &version {
            Some(v) => releases
                .iter()
                .rev()
                .find(|r| &r.version == v)
                .cloned()
                .ok_or_else(|| ControlError::NotFound(format!("Unknown release '{}'", v)))?,
            // The release that was live most recently before the current one.
            None => self
                .inner
                .projection
                .releases_by_recency(&project_id)
                .iter()
                .filter(|r| r.version != current && r.status == "Inactive")
                .find(|r| switcher.get_version_dir(&r.version).is_dir())
                .cloned()
                .ok_or_else(|| {
                    ControlError::FailedPrecondition(
                        "No earlier release is available on disk to roll back to".into(),
                    )
                })?,
        };
        if target.version == current && self.current_is_running(&project) {
            return Err(ControlError::FailedPrecondition(format!(
                "Release '{}' is already live",
                current
            )));
        }
        let dir = switcher.get_version_dir(&target.version);
        if !dir.is_dir()
            || !self
                .inner
                .artifact_store
                .verify(&target.id)
                .await
                .unwrap_or(false)
        {
            return Err(ControlError::FailedPrecondition(format!(
                "Release '{}' is no longer on disk (pruned by retention)",
                target.version
            )));
        }
        let config = ResolvedProjectConfig::load(&dir).map_err(|e| {
            ControlError::FailedPrecondition(format!(
                "Release '{}' has an invalid aegis.toml: {}",
                target.version, e
            ))
        })?;

        self.record(
            "RollbackRequested",
            json!({
                "project_id": project_id.to_string(),
                "from_version": current,
                "target_version": target.version,
            }),
        )
        .await?;
        let target_release = TargetRelease {
            release_id: target.id,
            version: target.version.clone(),
            dir,
            config,
            deployment_id: None,
        };
        match self.rollback_cutover(&project, &target_release).await {
            Ok(instant) => {
                self.record(
                    "RollbackCompleted",
                    json!({
                        "project_id": project_id.to_string(),
                        "from_version": current,
                        "to_version": target.version,
                        "instant": instant,
                    }),
                )
                .await?;
                Ok(target.version)
            }
            Err(failure) => {
                self.record(
                    "RollbackFailed",
                    json!({
                        "project_id": project_id.to_string(),
                        "target_version": target.version,
                        "reason": failure.reason,
                        "restored": failure.restored,
                    }),
                )
                .await?;
                Err(ControlError::FailedPrecondition(format!(
                    "Rollback to '{}' failed: {}{}",
                    target.version,
                    failure.reason,
                    failure
                        .restored
                        .map(|v| format!(" ('{}' is serving again)", v))
                        .unwrap_or_default()
                )))
            }
        }
    }

    fn current_is_running(&self, project: &ProjectState) -> bool {
        project
            .current_process
            .is_some_and(|id| self.is_running(&id))
    }
}

/// A sortable, unique release name such as `20260928-051203`.
fn generate_version(switcher: &ReleaseSwitcher) -> String {
    let base = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let mut version = base.clone();
    let mut n = 2;
    while switcher.get_version_dir(&version).exists() {
        version = format!("{}-{}", base, n);
        n += 1;
    }
    version
}
