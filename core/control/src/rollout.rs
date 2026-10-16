//! Several instances per app, and the Rolling and BlueGreen strategies.
//!
//! An app with `[run] instances = N` runs N copies of its release, each a
//! supervised process with its own `PORT`, log file and cgroup. The built-in
//! proxy spreads requests over the healthy ones, so several instances (and
//! Rolling and BlueGreen) need the app to be served by the proxy: `[proxy]`
//! enabled and a domain mapped to the app.
//!
//! - **GracefulSwitch** (and **Immediate**) start the whole new set next to
//!   the old one, switch the proxy to it in one step once every new instance
//!   is healthy, then drain and stop the old set (Immediate doesn't drain).
//! - **Rolling** replaces instances a batch at a time (`rolling_batch`,
//!   default 1): start new ones, wait until they are healthy, add them to the
//!   proxy, then take old ones out of the proxy, drain and stop them. If a new
//!   instance doesn't get healthy, the rollout stops and the old set is
//!   restored. Every step is recorded (`RolloutStepCompleted`).
//! - **BlueGreen** starts the full new set, switches the proxy atomically,
//!   and keeps the old set running for `blue_green_keep_secs` (default 300):
//!   a rollback to it in that window is an instant switch back. After the
//!   window, or when another release goes live, the old set is stopped.
//!
//! The live set of instances is recorded in `ReleasePromoted`
//! (`process_ids`) and `InstanceSetChanged`, so a daemon restart brings back
//! the same number of instances.

use crate::deploy::{drain_for, CutoverFailure, TargetRelease};
use crate::{ControlError, ControlPlane, ControlResult};
use aegis_config::{strategy_needs_proxy, validate_instances, validate_strategy};
use aegis_engine::ResolvedProjectConfig;
use aegis_process::StopReason;
use aegis_projection::{ProcessState, ProjectState};
use aegis_types::{ProcessId, ProjectId, ReleaseId};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Time allowed for stopping instances that never served traffic.
const QUICK_STOP: Duration = Duration::from_secs(2);

static STANDBY_GENERATION: AtomicU64 = AtomicU64::new(1);

/// One step of a rolling update: start `start` new instances, then stop
/// `stop` old ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RollingStep {
    pub start: usize,
    pub stop: usize,
}

/// Plans a rolling update from `old` running instances to `new` ones,
/// starting at most `batch` new instances per step. Old instances are
/// stopped one for one as new ones come up; any left over when the last
/// new instance is up (scaling down) are stopped in that last step.
pub(crate) fn plan_rolling(old: usize, new: usize, batch: usize) -> Vec<RollingStep> {
    let batch = batch.max(1);
    let mut steps = Vec::new();
    let (mut started, mut remaining_old) = (0, old);
    while started < new {
        let start = batch.min(new - started);
        started += start;
        let stop = if started >= new {
            remaining_old
        } else {
            start.min(remaining_old)
        };
        remaining_old -= stop;
        steps.push(RollingStep { start, stop });
    }
    if remaining_old > 0 {
        steps.push(RollingStep {
            start: 0,
            stop: remaining_old,
        });
    }
    steps
}

/// Why `strategy` with `instances` can't be used for app `name`, if it
/// can't. `proxied`: the built-in proxy is on and the app has a domain.
pub(crate) fn check_requirements(
    strategy: &str,
    instances: u32,
    proxied: bool,
    name: &str,
) -> Result<(), String> {
    validate_strategy(strategy)?;
    validate_instances(instances)?;
    if proxied {
        return Ok(());
    }
    let how = format!(
        "enable [proxy] in the daemon's config and map a domain to '{}' first",
        name
    );
    if strategy_needs_proxy(strategy) {
        return Err(format!(
            "{} deployments need the app to be served by the built-in proxy: {} (GracefulSwitch, the default, works without it)",
            strategy, how
        ));
    }
    if instances > 1 {
        return Err(format!(
            "Running {} instances needs the app to be served by the built-in proxy, which spreads requests over them: {} (or set instances to 1)",
            instances, how
        ));
    }
    Ok(())
}

/// A running instance of an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Instance {
    pub id: ProcessId,
    pub port: Option<u16>,
    /// 1-based.
    pub number: u32,
}

impl Instance {
    fn json(&self) -> Value {
        json!({"process_id": self.id.to_string(), "instance": self.number, "port": self.port})
    }
}

fn ids(instances: &[Instance]) -> Vec<String> {
    instances.iter().map(|i| i.id.to_string()).collect()
}

fn ports(instances: &[Instance]) -> Vec<u16> {
    instances.iter().filter_map(|i| i.port).collect()
}

/// A BlueGreen standby: the previous release's instances, still running.
#[derive(Debug, Clone)]
pub(crate) struct Standby {
    pub version: String,
    pub instances: Vec<Instance>,
    pub until: chrono::DateTime<chrono::Utc>,
    pub drain: Duration,
    generation: u64,
}

/// One instance of an app as shown by the API.
#[derive(Debug, Clone)]
pub struct InstanceView {
    pub process: ProcessState,
    /// CPU used (cores) at the latest sample, while it runs.
    pub cpu_cores: Option<f64>,
    /// Memory used (bytes) at the latest sample, while it runs.
    pub memory_bytes: Option<u64>,
}

/// The previous release kept running after a BlueGreen switch.
#[derive(Debug, Clone)]
pub struct StandbyView {
    pub version: String,
    pub process_ids: Vec<ProcessId>,
    /// RFC 3339 time when it is stopped.
    pub until: String,
}

impl ControlPlane {
    /// Why `strategy` can't be used for `project` right now, if it can't.
    pub(crate) fn check_strategy_for(
        &self,
        project: &ProjectState,
        strategy: &str,
    ) -> Result<(), String> {
        check_requirements(strategy, 1, self.is_proxied(&project.id), &project.name)
    }

    /// Why `project` can't run `instances` instances right now, if it can't.
    pub(crate) fn check_instances_for(
        &self,
        project: &ProjectState,
        instances: u32,
    ) -> Result<(), String> {
        check_requirements(
            "GracefulSwitch",
            instances,
            self.is_proxied(&project.id),
            &project.name,
        )
    }

    fn port_of(&self, id: &ProcessId) -> Option<u16> {
        self.inner
            .process_ports
            .lock()
            .unwrap()
            .get(id)
            .and_then(|p| p.port)
    }

    fn instance_number(&self, id: &ProcessId) -> Option<u32> {
        self.inner
            .process_instance
            .lock()
            .unwrap()
            .get(id)
            .copied()
            .or_else(|| self.inner.projection.get_process(id)?.instance)
    }

    /// The running instances of the live release, in instance order.
    pub(crate) fn live_instances(&self, project: &ProjectState) -> Vec<Instance> {
        project
            .live_processes()
            .into_iter()
            .filter(|id| self.is_running(id))
            .enumerate()
            .map(|(i, id)| Instance {
                id,
                port: self.port_of(&id),
                number: self.instance_number(&id).unwrap_or(i as u32 + 1),
            })
            .collect()
    }

    /// Points the proxy at every running instance of the project's live set.
    pub(crate) fn sync_routes(&self, project_id: ProjectId) {
        let (Some(ingress), Some(project)) = (
            self.ingress(),
            self.inner.projection.get_project(&project_id),
        ) else {
            return;
        };
        let ports = ports(&self.live_instances(&project));
        ingress.set_upstreams(&project_id.to_string(), ports);
    }

    fn set_routes(&self, project_id: ProjectId, ports: Vec<u16>) {
        if let Some(ingress) = self.ingress() {
            ingress.set_upstreams(&project_id.to_string(), ports);
        }
    }

    pub(crate) fn instance_views(&self, project: &ProjectState) -> Vec<InstanceView> {
        let usage = self.inner.process_usage.lock().unwrap().clone();
        project
            .live_processes()
            .iter()
            .enumerate()
            .filter_map(|(i, id)| {
                let mut process = self.process_view(id)?;
                if process.instance.is_none() {
                    process.instance = self.instance_number(id).or(Some(i as u32 + 1));
                }
                let running = process.status == "Running";
                let (cpu, memory) = usage
                    .get(id)
                    .filter(|_| running)
                    .map_or((None, None), |(c, m)| (Some(*c), Some(*m)));
                Some(InstanceView {
                    process,
                    cpu_cores: cpu,
                    memory_bytes: memory,
                })
            })
            .collect()
    }

    pub(crate) fn standby_view(&self, project_id: &ProjectId) -> Option<StandbyView> {
        self.inner
            .standby
            .lock()
            .unwrap()
            .get(project_id)
            .map(|s| StandbyView {
                version: s.version.clone(),
                process_ids: s.instances.iter().map(|i| i.id).collect(),
                until: s.until.to_rfc3339(),
            })
    }

    /// A free port for a new instance, not one already handed out.
    fn allocate_instance_port(&self, taken: &mut HashSet<u16>) -> std::io::Result<u16> {
        let in_use: HashSet<u16> = self
            .inner
            .process_ports
            .lock()
            .unwrap()
            .values()
            .filter_map(|p| p.port)
            .collect();
        for _ in 0..20 {
            let port = crate::domains::allocate_port()?;
            if !in_use.contains(&port) && taken.insert(port) {
                return Ok(port);
            }
        }
        Err(std::io::Error::other("no distinct free port found"))
    }

    /// Starts a new instance of `target` next to the live ones. It is a
    /// candidate (outside the live set) until promoted.
    async fn start_instance(
        &self,
        project_id: ProjectId,
        version: &str,
        dir: &std::path::Path,
        config: &ResolvedProjectConfig,
        number: u32,
        taken: &mut HashSet<u16>,
    ) -> Result<Instance, (Option<ProcessId>, String)> {
        let port = self
            .allocate_instance_port(taken)
            .map_err(|e| (None, format!("No free port for a new instance: {}", e)))?;
        let id = ProcessId::new();
        self.inner.candidates.lock().unwrap().insert(id);
        self.inner
            .process_instance
            .lock()
            .unwrap()
            .insert(id, number);
        self.start_release_process(project_id, id, version, dir, config, Some(port))
            .await
            .map_err(|e| (Some(id), format!("Failed to start: {}", e)))?;
        Ok(Instance {
            id,
            port: Some(port),
            number,
        })
    }

    /// Waits until every instance is healthy. On the first failure, returns
    /// the instance and why.
    async fn wait_all_healthy(
        &self,
        instances: &[Instance],
        config: &ResolvedProjectConfig,
    ) -> Result<(), (ProcessId, String)> {
        let mut set = tokio::task::JoinSet::new();
        for instance in instances {
            let (this, config, instance) = (self.clone(), config.clone(), *instance);
            set.spawn(async move {
                let result = this.wait_healthy(&instance.id, &config, instance.port).await;
                (instance, result)
            });
        }
        let mut failure = None;
        while let Some(joined) = set.join_next().await {
            if let Ok((instance, Err(reason))) = joined {
                if failure.is_none() {
                    failure = Some((
                        instance.id,
                        format!("Instance {}: {}", instance.number, reason),
                    ));
                    set.abort_all();
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }

    /// Stops processes (in parallel), giving each `timeout` to exit.
    async fn stop_processes(&self, ids: &[ProcessId], timeout: Duration, reason: StopReason) {
        let mut set = tokio::task::JoinSet::new();
        for id in ids {
            if self.inner.supervisor.get(id).is_none() {
                continue;
            }
            let (this, id) = (self.clone(), *id);
            set.spawn(async move {
                if let Err(e) = this.inner.supervisor.stop(&id, timeout, reason).await {
                    tracing::error!(process = %id, error = %e, "Failed to stop instance");
                }
            });
        }
        while set.join_next().await.is_some() {}
    }

    /// Waits (up to `drain` in total) until nothing is proxied to these
    /// instances any more, then stops them. Take them out of the proxy first.
    async fn drain_and_stop(&self, instances: &[Instance], drain: Duration) {
        let deadline = Instant::now() + drain;
        if let Some(ingress) = self.ingress() {
            for port in ports(instances) {
                let left = deadline.saturating_duration_since(Instant::now());
                if !ingress.wait_idle(port, left).await {
                    tracing::warn!(
                        port,
                        "Instance still had requests in flight after the drain timeout"
                    );
                }
            }
        }
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_secs(1));
        let ids: Vec<ProcessId> = instances.iter().map(|i| i.id).collect();
        self.stop_processes(&ids, remaining, StopReason::Replaced)
            .await;
    }

    fn log_tail(&self, id: &ProcessId) -> Vec<String> {
        self.inner
            .supervisor
            .logs(id, 20)
            .into_iter()
            .map(|l| l.line)
            .collect()
    }

    fn release_drain(&self, project: &ProjectState, strategy: &str, fallback: &ResolvedProjectConfig) -> Duration {
        project
            .current_release
            .as_ref()
            .and_then(|v| {
                ResolvedProjectConfig::load(&self.switcher(project.id).get_version_dir(v)).ok()
            })
            .map(|c| drain_for(strategy, &c))
            .unwrap_or_else(|| drain_for(strategy, fallback))
    }

    /// Makes a set of instances the live release.
    async fn promote_set(
        &self,
        project: &ProjectState,
        release_id: Option<ReleaseId>,
        version: &str,
        instances: &[Instance],
    ) {
        if let Err(e) = self.switcher(project.id).switch_to_version(version).await {
            tracing::error!(error = %e, "Failed to update current release pointer");
        }
        {
            let mut candidates = self.inner.candidates.lock().unwrap();
            for instance in instances {
                candidates.remove(&instance.id);
            }
        }
        let first = instances.first();
        let _ = self
            .record(
                "ReleasePromoted",
                json!({
                    "release_id": release_id.map(|r| r.to_string()),
                    "project_id": project.id.to_string(),
                    "version": version,
                    "previous_version": project.current_release,
                    "process_id": first.map(|i| i.id.to_string()),
                    "port": first.and_then(|i| i.port),
                    "process_ids": ids(instances),
                    "ports": ports(instances),
                }),
            )
            .await;
    }

    /// The multi-instance cutover, when this deployment needs it: Rolling or
    /// BlueGreen, more than one instance, or more than one running now.
    /// `None` means the single-instance cutover applies.
    pub(crate) async fn cutover_instances(
        &self,
        project: &ProjectState,
        target: &TargetRelease,
        strategy: &str,
    ) -> Option<Result<(), CutoverFailure>> {
        let wanted = target.config.instances;
        let old = self.live_instances(project);
        if !(strategy_needs_proxy(strategy) || wanted != 1 || old.len() > 1) {
            return None;
        }
        let proxied = self.is_proxied(&project.id);
        if let Err(reason) = check_requirements(strategy, wanted, proxied, &project.name) {
            if !proxied && wanted == 1 && !strategy_needs_proxy(strategy) {
                // Back to one instance without the proxy (its domain was
                // removed): keep the first instance for the usual cutover.
                let extra: Vec<ProcessId> = old.iter().skip(1).map(|i| i.id).collect();
                let drain = self.release_drain(project, strategy, &target.config);
                self.stop_processes(&extra, drain, StopReason::Replaced)
                    .await;
                return None;
            }
            return Some(Err(CutoverFailure {
                reason,
                log_tail: Vec::new(),
                restored: None,
            }));
        }
        Some(match strategy {
            "Rolling" => self.rolling(project, target, old).await,
            _ => self.switch_set(project, target, strategy, old).await,
        })
    }

    /// GracefulSwitch, Immediate and BlueGreen: the full new set next to the
    /// old one, then one switch.
    async fn switch_set(
        &self,
        project: &ProjectState,
        target: &TargetRelease,
        strategy: &str,
        old: Vec<Instance>,
    ) -> Result<(), CutoverFailure> {
        let project_id = project.id;
        let mut taken = HashSet::new();
        let mut new = Vec::new();
        let mut failure = None;
        for number in 1..=target.config.instances {
            match self
                .start_instance(
                    project_id,
                    &target.version,
                    &target.dir,
                    &target.config,
                    number,
                    &mut taken,
                )
                .await
            {
                Ok(instance) => new.push(instance),
                Err((id, reason)) => {
                    failure = Some((id, reason));
                    break;
                }
            }
        }
        if failure.is_none() {
            failure = self
                .wait_all_healthy(&new, &target.config)
                .await
                .err()
                .map(|(id, reason)| (Some(id), reason));
        }
        if let Some((failed, reason)) = failure {
            tracing::warn!(version = %target.version, reason = %reason, "New instances are unhealthy; the live release keeps serving");
            let mut all: Vec<ProcessId> = new.iter().map(|i| i.id).collect();
            all.extend(failed);
            self.stop_processes(&all, QUICK_STOP, StopReason::Replaced)
                .await;
            self.forget_candidates(&all);
            let restored = (!old.is_empty() && old.iter().any(|i| self.is_running(&i.id)))
                .then(|| project.current_release.clone())
                .flatten();
            return Err(CutoverFailure {
                log_tail: failed.map(|id| self.log_tail(&id)).unwrap_or_default(),
                reason,
                restored,
            });
        }

        // The switch: every request from now on goes to the new set.
        self.set_routes(project_id, ports(&new));
        self.promote_set(project, Some(target.release_id), &target.version, &new)
            .await;
        let _ = self
            .record(
                "TrafficSwitched",
                json!({
                    "deployment_id": target.deployment_id.map(|d| d.to_string()),
                    "project_id": project_id.to_string(),
                    "strategy": strategy,
                    "from_version": project.current_release,
                    "to_version": target.version,
                    "instances": new.iter().map(Instance::json).collect::<Vec<_>>(),
                    "previous_instances": old.iter().map(Instance::json).collect::<Vec<_>>(),
                }),
            )
            .await;

        let keep = target.config.blue_green_keep;
        let drain = self.release_drain(project, strategy, &target.config);
        if strategy == "BlueGreen" && !old.is_empty() && !keep.is_zero() {
            if let Some(version) = project.current_release.clone() {
                self.keep_standby(project_id, version, old, keep, drain, target)
                    .await;
                return Ok(());
            }
        }
        self.drain_and_stop(&old, drain).await;
        Ok(())
    }

    fn forget_candidates(&self, ids: &[ProcessId]) {
        let mut candidates = self.inner.candidates.lock().unwrap();
        for id in ids {
            candidates.remove(id);
        }
    }

    /// Rolling: replace the instances a batch at a time.
    async fn rolling(
        &self,
        project: &ProjectState,
        target: &TargetRelease,
        old: Vec<Instance>,
    ) -> Result<(), CutoverFailure> {
        let project_id = project.id;
        let plan = plan_rolling(
            old.len(),
            target.config.instances as usize,
            target.config.rolling_batch as usize,
        );
        let drain = self.release_drain(project, "Rolling", &target.config);
        let mut taken = HashSet::new();
        let mut remaining_old: std::collections::VecDeque<Instance> = old.iter().copied().collect();
        let mut stopped_old: Vec<Instance> = Vec::new();
        let mut serving_new: Vec<Instance> = Vec::new();
        let mut next_number = 1;

        for (index, step) in plan.iter().enumerate() {
            let mut batch = Vec::new();
            let mut failure = None;
            if self.inner.shutting_down.load(Ordering::SeqCst) {
                failure = Some((None, "The daemon is shutting down".to_string()));
            }
            for _ in 0..step.start {
                if failure.is_some() {
                    break;
                }
                match self
                    .start_instance(
                        project_id,
                        &target.version,
                        &target.dir,
                        &target.config,
                        next_number,
                        &mut taken,
                    )
                    .await
                {
                    Ok(instance) => batch.push(instance),
                    Err((id, reason)) => failure = Some((id, reason)),
                }
                next_number += 1;
            }
            if failure.is_none() {
                failure = self
                    .wait_all_healthy(&batch, &target.config)
                    .await
                    .err()
                    .map(|(id, reason)| (Some(id), reason));
            }
            if let Some((failed, reason)) = failure {
                let mut unhealthy: Vec<ProcessId> = batch.iter().map(|i| i.id).collect();
                unhealthy.extend(failed);
                let log_tail = failed.map(|id| self.log_tail(&id)).unwrap_or_default();
                let restored = self
                    .abort_rollout(
                        project,
                        target,
                        index + 1,
                        plan.len(),
                        &reason,
                        &unhealthy,
                        &serving_new,
                        &remaining_old,
                        &stopped_old,
                    )
                    .await;
                return Err(CutoverFailure {
                    reason: format!(
                        "Rolling update stopped at step {} of {}: {}",
                        index + 1,
                        plan.len(),
                        reason
                    ),
                    log_tail,
                    restored,
                });
            }

            // The new batch takes requests next to the old instances...
            serving_new.extend(batch.iter().copied());
            let mut serving: Vec<Instance> = remaining_old.iter().copied().collect();
            serving.extend(serving_new.iter().copied());
            self.set_routes(project_id, ports(&serving));
            // ...then old instances stop getting new requests, finish the
            // ones they have, and stop.
            let stopping: Vec<Instance> = remaining_old.drain(..step.stop).collect();
            if !stopping.is_empty() {
                let mut serving: Vec<Instance> = remaining_old.iter().copied().collect();
                serving.extend(serving_new.iter().copied());
                self.set_routes(project_id, ports(&serving));
                self.drain_and_stop(&stopping, drain).await;
                stopped_old.extend(stopping.iter().copied());
            }
            let _ = self
                .record(
                    "RolloutStepCompleted",
                    json!({
                        "deployment_id": target.deployment_id.map(|d| d.to_string()),
                        "project_id": project_id.to_string(),
                        "strategy": "Rolling",
                        "version": target.version,
                        "step": index + 1,
                        "steps": plan.len(),
                        "started": batch.iter().map(Instance::json).collect::<Vec<_>>(),
                        "stopped": stopping.iter().map(Instance::json).collect::<Vec<_>>(),
                        "new_serving": serving_new.len(),
                        "old_serving": remaining_old.len(),
                    }),
                )
                .await;
        }

        self.set_routes(project_id, ports(&serving_new));
        self.promote_set(
            project,
            Some(target.release_id),
            &target.version,
            &serving_new,
        )
        .await;
        Ok(())
    }

    /// Puts the old set back after a failed rolling step: restarts the old
    /// instances already stopped, switches the proxy back to the old set,
    /// then drains and stops the new instances. Returns the release serving
    /// again, if any.
    #[allow(clippy::too_many_arguments)]
    async fn abort_rollout(
        &self,
        project: &ProjectState,
        target: &TargetRelease,
        step: usize,
        steps: usize,
        reason: &str,
        unhealthy: &[ProcessId],
        serving_new: &[Instance],
        remaining_old: &std::collections::VecDeque<Instance>,
        stopped_old: &[Instance],
    ) -> Option<String> {
        let project_id = project.id;
        tracing::warn!(version = %target.version, step, reason, "Rolling update failed; restoring the previous instances");
        // Never got traffic.
        self.stop_processes(unhealthy, QUICK_STOP, StopReason::Replaced)
            .await;
        self.forget_candidates(unhealthy);

        // Bring back the old instances that were already replaced, on their
        // own ports, before taking the new ones out of the proxy.
        let mut old_serving: Vec<Instance> = remaining_old.iter().copied().collect();
        let previous = project.current_release.clone().and_then(|version| {
            let dir = self.switcher(project_id).get_version_dir(&version);
            ResolvedProjectConfig::load(&dir)
                .ok()
                .map(|config| (version, dir, config))
        });
        let mut restarted = Vec::new();
        // While the daemon shuts down nothing new is started; the recorded
        // live set is started again on the next boot.
        let shutting_down = self.inner.shutting_down.load(Ordering::SeqCst);
        if let (Some((version, dir, config)), false) = (&previous, shutting_down) {
            for instance in stopped_old {
                let port = self.restart_port(&instance.id);
                self.inner
                    .process_instance
                    .lock()
                    .unwrap()
                    .insert(instance.id, instance.number);
                match self
                    .start_release_process(project_id, instance.id, version, dir, config, port)
                    .await
                {
                    Ok(port) => restarted.push(Instance {
                        id: instance.id,
                        port,
                        number: instance.number,
                    }),
                    Err(e) => {
                        tracing::error!(error = %e, "Failed to restart a previous instance")
                    }
                }
            }
            if let Err((id, e)) = self.wait_all_healthy(&restarted, config).await {
                tracing::error!(process = %id, error = %e, "A restored instance is not healthy");
            }
        }
        old_serving.extend(restarted.iter().copied());
        old_serving.sort_by_key(|i| i.number);
        let old_running: Vec<Instance> = old_serving
            .into_iter()
            .filter(|i| self.is_running(&i.id))
            .collect();

        if old_running.is_empty() {
            // Nothing of the old release is left: keep what serves.
            let mut serving: Vec<Instance> = serving_new.to_vec();
            serving.retain(|i| self.is_running(&i.id));
            self.set_routes(project_id, ports(&serving));
        } else {
            self.set_routes(project_id, ports(&old_running));
            let drain = self.release_drain(project, "Rolling", &target.config);
            self.drain_and_stop(serving_new, drain).await;
            let ids: Vec<ProcessId> = serving_new.iter().map(|i| i.id).collect();
            self.forget_candidates(&ids);
        }
        let _ = self
            .record(
                "RolloutAborted",
                json!({
                    "deployment_id": target.deployment_id.map(|d| d.to_string()),
                    "project_id": project_id.to_string(),
                    "strategy": "Rolling",
                    "version": target.version,
                    "step": step,
                    "steps": steps,
                    "reason": reason,
                    "restored_instances": old_running.iter().map(Instance::json).collect::<Vec<_>>(),
                }),
            )
            .await;
        (!old_running.is_empty())
            .then(|| project.current_release.clone())
            .flatten()
    }

    /// Keeps the old set running after a BlueGreen switch, as an instant
    /// rollback target, and stops it when `keep` has passed.
    async fn keep_standby(
        &self,
        project_id: ProjectId,
        version: String,
        old: Vec<Instance>,
        keep: Duration,
        drain: Duration,
        target: &TargetRelease,
    ) {
        {
            // Outside the live set: their restarts must not make them current.
            let mut candidates = self.inner.candidates.lock().unwrap();
            for instance in &old {
                candidates.insert(instance.id);
            }
        }
        let generation = STANDBY_GENERATION.fetch_add(1, Ordering::SeqCst);
        let until = chrono::Utc::now()
            + chrono::Duration::from_std(keep).unwrap_or_else(|_| chrono::Duration::seconds(300));
        let _ = self
            .record(
                "StandbyKept",
                json!({
                    "deployment_id": target.deployment_id.map(|d| d.to_string()),
                    "project_id": project_id.to_string(),
                    "version": version,
                    "process_ids": ids(&old),
                    "until": until.to_rfc3339(),
                }),
            )
            .await;
        self.inner.standby.lock().unwrap().insert(
            project_id,
            Standby {
                version,
                instances: old,
                until,
                drain,
                generation,
            },
        );
        let this = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(keep).await;
            let lock = this.project_lock(project_id);
            let _guard = lock.lock().await;
            let current = this
                .inner
                .standby
                .lock()
                .unwrap()
                .get(&project_id)
                .map(|s| s.generation);
            if current == Some(generation) {
                this.drop_standby(project_id, "expired").await;
            }
        });
    }

    /// Stops a project's BlueGreen standby, if it has one. Call with the
    /// project lock held.
    pub(crate) async fn drop_standby(&self, project_id: ProjectId, reason: &str) {
        let Some(standby) = self.inner.standby.lock().unwrap().remove(&project_id) else {
            return;
        };
        let all: Vec<ProcessId> = standby.instances.iter().map(|i| i.id).collect();
        self.stop_processes(&all, standby.drain, StopReason::Replaced)
            .await;
        self.forget_candidates(&all);
        let _ = self
            .record(
                "StandbyStopped",
                json!({
                    "project_id": project_id.to_string(),
                    "version": standby.version,
                    "process_ids": ids(&standby.instances),
                    "reason": reason,
                }),
            )
            .await;
    }

    /// The cutover of a rollback: an instant switch back to a BlueGreen
    /// standby when the target is the release it holds, else a normal
    /// cutover. Returns whether it was instant.
    pub(crate) async fn rollback_cutover(
        &self,
        project: &ProjectState,
        target: &TargetRelease,
    ) -> Result<bool, CutoverFailure> {
        let standby = self.inner.standby.lock().unwrap().get(&project.id).cloned();
        if let Some(standby) = standby.filter(|s| {
            s.version == target.version && s.instances.iter().all(|i| self.is_running(&i.id))
        }) {
            self.inner.standby.lock().unwrap().remove(&project.id);
            let live = self.live_instances(project);
            // The switch back.
            self.set_routes(project.id, ports(&standby.instances));
            self.promote_set(
                project,
                Some(target.release_id),
                &target.version,
                &standby.instances,
            )
            .await;
            let _ = self
                .record(
                    "TrafficSwitched",
                    json!({
                        "project_id": project.id.to_string(),
                        "strategy": "BlueGreen",
                        "instant_rollback": true,
                        "from_version": project.current_release,
                        "to_version": target.version,
                        "instances": standby.instances.iter().map(Instance::json).collect::<Vec<_>>(),
                        "previous_instances": live.iter().map(Instance::json).collect::<Vec<_>>(),
                    }),
                )
                .await;
            let drain = self.release_drain(project, "BlueGreen", &target.config);
            self.drain_and_stop(&live, drain).await;
            return Ok(true);
        }
        // Rolling and BlueGreen need the proxy; if the app has lost its
        // domain since, roll back the plain way.
        let mut strategy = target.config.strategy.clone();
        if strategy_needs_proxy(&strategy) && !self.is_proxied(&project.id) {
            strategy = "GracefulSwitch".into();
        }
        self.cutover(project, target, &strategy).await.map(|_| false)
    }

    /// Start, stop or restart every instance of a project with several.
    /// `None` when the project runs one instance (the usual path applies).
    pub(crate) async fn project_instances_action(
        &self,
        key: &str,
        action: crate::ProcessAction,
    ) -> Option<ControlResult<ProcessState>> {
        if key.parse::<ProcessId>().is_ok_and(|id| self.inner.projection.get_process(&id).is_some()) {
            return None;
        }
        let project = self.resolve_project(key).ok()?;
        if project.live_processes().len() < 2 {
            return None;
        }
        let lock = self.project_lock(project.id);
        let _guard = lock.lock().await;
        let project = self.inner.projection.get_project(&project.id)?;
        Some(self.instances_action(&project, action).await)
    }

    async fn instances_action(
        &self,
        project: &ProjectState,
        action: crate::ProcessAction,
    ) -> ControlResult<ProcessState> {
        use crate::ProcessAction;
        let ids = project.live_processes();
        let first = *ids.first().ok_or_else(|| {
            ControlError::FailedPrecondition("The project has no instances".into())
        })?;
        let release = project.current_release.clone().ok_or_else(|| {
            ControlError::FailedPrecondition("The project has no live release".into())
        })?;
        let dir = self.switcher(project.id).get_version_dir(&release);
        let config = ResolvedProjectConfig::load(&dir).ok();
        let drain = config
            .as_ref()
            .map(|c| c.drain_timeout)
            .unwrap_or(Duration::from_secs(10));
        for id in &ids {
            self.record(
                &format!("Process{}Requested", action.as_str()),
                json!({"process_id": id.to_string(), "project_id": project.id.to_string()}),
            )
            .await?;
        }
        match action {
            ProcessAction::Stop => {
                self.drop_standby(project.id, "stopped").await;
                self.set_routes(project.id, Vec::new());
                for id in &ids {
                    if self.is_running(id) {
                        self.inner
                            .supervisor
                            .stop(id, drain, StopReason::Requested)
                            .await?;
                    } else {
                        self.record(
                            "ProcessStopped",
                            json!({
                                "process_id": id.to_string(),
                                "project_id": project.id.to_string(),
                                "reason": "requested",
                            }),
                        )
                        .await?;
                    }
                }
            }
            ProcessAction::Start => {
                let config = config.ok_or_else(|| {
                    ControlError::FailedPrecondition(
                        "The release for this app is no longer on disk".into(),
                    )
                })?;
                let mut started = 0;
                for (i, id) in ids.iter().enumerate() {
                    if self.is_running(id) {
                        continue;
                    }
                    let number = self.instance_number(id).unwrap_or(i as u32 + 1);
                    self.inner
                        .process_instance
                        .lock()
                        .unwrap()
                        .insert(*id, number);
                    let port = self.restart_port(id);
                    self.start_release_process(project.id, *id, &release, &dir, &config, port)
                        .await
                        .map_err(ControlError::Internal)?;
                    started += 1;
                }
                if started == 0 {
                    return Err(ControlError::FailedPrecondition(
                        "All instances are already running".into(),
                    ));
                }
                self.sync_routes(project.id);
            }
            ProcessAction::Restart => {
                let config = config.ok_or_else(|| {
                    ControlError::FailedPrecondition(
                        "The release for this app is no longer on disk".into(),
                    )
                })?;
                self.rolling_restart(project, &release, &dir, &config, drain)
                    .await?;
            }
        }
        self.process_view(&first)
            .ok_or_else(|| ControlError::Internal(anyhow::anyhow!("Process vanished")))
    }

    /// Restarts the instances one at a time: out of the proxy, drain,
    /// restart, wait until healthy, back into the proxy. Stopped instances
    /// are started. Stops at the first instance that doesn't come back.
    async fn rolling_restart(
        &self,
        project: &ProjectState,
        release: &str,
        dir: &std::path::Path,
        config: &ResolvedProjectConfig,
        drain: Duration,
    ) -> ControlResult<()> {
        let ids = project.live_processes();
        for (i, id) in ids.iter().enumerate() {
            let number = self.instance_number(id).unwrap_or(i as u32 + 1);
            let instance_port = self.port_of(id);
            // Out of the proxy first, so no request is cut off.
            let others: Vec<u16> = ids
                .iter()
                .filter(|other| *other != id && self.is_running(other))
                .filter_map(|other| self.port_of(other))
                .collect();
            if !others.is_empty() {
                self.set_routes(project.id, others);
                if let (Some(port), Some(ingress)) = (instance_port, self.ingress()) {
                    ingress.wait_idle(port, drain).await;
                }
            }
            let port = if self.is_running(id) {
                // Pick up secrets changed since it started, keeping its port.
                let mut env = self.process_env(project.id, config);
                if let Some(port) = instance_port.filter(|_| self.is_proxied(&project.id)) {
                    env.insert("PORT".to_string(), port.to_string());
                }
                self.inner.supervisor.set_env(id, env);
                self.inner.supervisor.restart(id, drain).await?;
                instance_port
            } else {
                self.inner
                    .process_instance
                    .lock()
                    .unwrap()
                    .insert(*id, number);
                let port = self.restart_port(id);
                self.start_release_process(project.id, *id, release, dir, config, port)
                    .await
                    .map_err(ControlError::Internal)?
            };
            let health = self.wait_healthy(id, config, port).await;
            self.sync_routes(project.id);
            if let Err(reason) = health {
                return Err(ControlError::FailedPrecondition(format!(
                    "Instance {} did not come back healthy after the restart ({}); the remaining instances were not restarted",
                    number, reason
                )));
            }
        }
        Ok(())
    }

    /// After a daemon start: starts new instances when fewer are running
    /// than the live release asks for (e.g. some were replaced by a rollout
    /// the daemon didn't finish), and records the set.
    pub(crate) async fn top_up_instances(&self, project: &ProjectState) -> usize {
        let Some(version) = project.current_release.clone() else {
            return 0;
        };
        let dir = self.switcher(project.id).get_version_dir(&version);
        let Ok(config) = ResolvedProjectConfig::load(&dir) else {
            return 0;
        };
        let live = self.live_instances(project);
        let wanted = config.instances as usize;
        let recorded = project.live_processes();
        if live.is_empty()
            || !self.is_proxied(&project.id)
            || validate_instances(config.instances).is_err()
            || (live.len() >= wanted && recorded.len() == live.len())
        {
            return 0;
        }
        let mut set: Vec<Instance> = live.into_iter().take(wanted).collect();
        let mut taken: HashSet<u16> = ports(&set).into_iter().collect();
        let used: HashSet<u32> = set.iter().map(|i| i.number).collect();
        let mut numbers = (1..).filter(|n| !used.contains(n));
        let mut added = 0;
        while set.len() < wanted {
            let number = numbers.next().unwrap_or(1);
            match self
                .start_instance(project.id, &version, &dir, &config, number, &mut taken)
                .await
            {
                Ok(instance) => {
                    set.push(instance);
                    added += 1;
                }
                Err((id, e)) => {
                    tracing::error!(project = %project.name, error = %e, "Failed to start a missing instance");
                    if let Some(id) = id {
                        self.forget_candidates(&[id]);
                    }
                    break;
                }
            }
        }
        if let Err((_, e)) = self.wait_all_healthy(&set, &config).await {
            tracing::warn!(project = %project.name, error = %e, "A restored instance is not healthy yet");
        }
        set.sort_by_key(|i| i.number);
        let all: Vec<ProcessId> = set.iter().map(|i| i.id).collect();
        self.forget_candidates(&all);
        let _ = self
            .record(
                "InstanceSetChanged",
                json!({
                    "project_id": project.id.to_string(),
                    "version": version,
                    "process_ids": ids(&set),
                    "ports": ports(&set),
                    "reason": "recovered",
                }),
            )
            .await;
        self.sync_routes(project.id);
        added
    }

    /// Remembers the latest per-process usage from the sampler.
    pub(crate) fn note_process_usage(&self, usage: Vec<(ProcessId, f64, u64)>) {
        let mut map = self.inner.process_usage.lock().unwrap();
        map.clear();
        for (id, cpu, memory) in usage {
            map.insert(id, (cpu, memory));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steps(plan: &[RollingStep]) -> Vec<(usize, usize)> {
        plan.iter().map(|s| (s.start, s.stop)).collect()
    }

    #[test]
    fn rolling_replaces_one_for_one() {
        assert_eq!(steps(&plan_rolling(3, 3, 1)), vec![(1, 1), (1, 1), (1, 1)]);
        assert_eq!(steps(&plan_rolling(3, 3, 2)), vec![(2, 2), (1, 1)]);
        assert_eq!(steps(&plan_rolling(3, 3, 5)), vec![(3, 3)]);
        // A batch of 0 is treated as 1.
        assert_eq!(steps(&plan_rolling(2, 2, 0)), vec![(1, 1), (1, 1)]);
    }

    #[test]
    fn rolling_scales_up_and_down() {
        // Up: old ones go one for one, then only starts.
        assert_eq!(
            steps(&plan_rolling(2, 4, 1)),
            vec![(1, 1), (1, 1), (1, 0), (1, 0)]
        );
        // Down: the last step stops what is left.
        assert_eq!(steps(&plan_rolling(4, 2, 1)), vec![(1, 1), (1, 3)]);
        // Nothing running yet.
        assert_eq!(steps(&plan_rolling(0, 3, 2)), vec![(2, 0), (1, 0)]);
        // Every plan starts `new` and stops `old` in total.
        for old in 0..6 {
            for new in 1..6 {
                for batch in 1..4 {
                    let plan = plan_rolling(old, new, batch);
                    assert_eq!(plan.iter().map(|s| s.start).sum::<usize>(), new);
                    assert_eq!(plan.iter().map(|s| s.stop).sum::<usize>(), old);
                    assert!(plan.iter().all(|s| s.start <= batch));
                    // Never fewer instances serving than before the step,
                    // until the last step (which may scale down).
                    let mut serving = old;
                    for (i, s) in plan.iter().enumerate() {
                        serving = serving + s.start - s.stop;
                        if i + 1 < plan.len() {
                            assert!(serving >= old.min(new), "{old} {new} {batch}: {plan:?}");
                        }
                    }
                    assert_eq!(serving, new);
                }
            }
        }
    }

    #[test]
    fn strategies_needing_the_proxy_are_refused_without_it() {
        assert!(check_requirements("GracefulSwitch", 1, false, "web").is_ok());
        assert!(check_requirements("Immediate", 1, false, "web").is_ok());
        let err = check_requirements("Rolling", 1, false, "web").unwrap_err();
        assert!(err.contains("built-in proxy"), "{err}");
        assert!(err.contains("map a domain to 'web'"), "{err}");
        assert!(check_requirements("BlueGreen", 3, false, "web").is_err());
        let err = check_requirements("GracefulSwitch", 3, false, "web").unwrap_err();
        assert!(err.contains("Running 3 instances"), "{err}");
        for strategy in ["GracefulSwitch", "Immediate", "Rolling", "BlueGreen"] {
            assert!(check_requirements(strategy, 3, true, "web").is_ok());
        }
        assert!(check_requirements("Canary", 1, true, "web").is_err());
        assert!(check_requirements("Rolling", 0, true, "web").is_err());
        assert!(check_requirements("Rolling", 17, true, "web").is_err());
    }
}
