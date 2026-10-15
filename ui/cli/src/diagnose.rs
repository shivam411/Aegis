//! Reading event history for `replay`, `incident` and `investigate`.

use crate::history::{human, Ev};
use crate::short;
use aegis_projection::{DeploymentState, ProcessState, ProjectState, ProjectionEngine};
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;

/// A one-line, human description of an event.
pub fn describe(e: &Ev) -> String {
    let s = |k: &str| e.str(k);
    let dep = short(e.deployment_id());
    let or_dash = |v: &str| {
        if v.is_empty() {
            "-".to_string()
        } else {
            v.to_string()
        }
    };
    match e.event_type.as_str() {
        "ProjectCreated" => format!("project '{}' registered", s("name")),
        "ProjectSettingsUpdated" => {
            format!("settings changed: {}", fields(e.payload.get("settings")))
        }
        "ResourceLimitsChanged" => {
            format!("resource limits set: {}", fields(e.payload.get("limits")))
        }
        "ResourcePressureDetected" => format!("resource alert {}: {}", s("kind"), s("message")),
        "ScheduleConfigured" => format!(
            "daily deploy scheduled at {:02}:{:02} UTC (branch {})",
            e.i64("hour").unwrap_or(0),
            e.i64("minute").unwrap_or(0),
            s("branch")
        ),
        "DeploymentQueued" => format!(
            "deployment {} queued (trigger {}, strategy {})",
            dep,
            or_dash(s("trigger_source")),
            if s("strategy").is_empty() {
                "GracefulSwitch"
            } else {
                s("strategy")
            }
        ),
        "DeploymentStarted" => format!("deployment {} started building {}", dep, s("version")),
        t if t.starts_with("BuildStage") => {
            let detail = s("detail");
            if detail.is_empty() {
                format!("{} {}", s("stage"), s("status"))
            } else {
                format!("{} {}: {}", s("stage"), s("status"), detail)
            }
        }
        "DeploymentCompleted" | "DeploymentSucceeded" => {
            format!("deployment {} completed: {} is live", dep, s("version"))
        }
        "DeploymentFailed" => {
            let stage = s("stage");
            if stage.is_empty() {
                format!("deployment {} failed: {}", dep, s("reason"))
            } else {
                format!("deployment {} failed at {}: {}", dep, stage, s("reason"))
            }
        }
        "DeploymentRolledBack" => format!(
            "deployment {} rolled back: {} is serving again",
            dep,
            s("target_version")
        ),
        "ReleaseCreated" => {
            let sha = s("commit_sha");
            if sha.is_empty() {
                format!("release {} built", s("version"))
            } else {
                format!("release {} built from {}", s("version"), short(sha))
            }
        }
        "ReleasePromoted" => {
            let previous = s("previous_version");
            if previous.is_empty() {
                format!("release {} went live", s("version"))
            } else {
                format!(
                    "release {} went live (replacing {})",
                    s("version"),
                    previous
                )
            }
        }
        "ReleaseArchived" => format!("release {} archived", s("version")),
        "RollbackRequested" => format!(
            "rollback from {} to {} requested",
            or_dash(s("from_version")),
            or_dash(s("target_version"))
        ),
        "RollbackCompleted" => format!(
            "rolled back from {} to {}",
            or_dash(s("from_version")),
            or_dash(s("to_version"))
        ),
        "RollbackFailed" => format!(
            "rollback to {} failed: {}",
            or_dash(s("target_version")),
            s("reason")
        ),
        "ProcessStarted" => {
            let mut text = format!(
                "process started (pid {}, release {})",
                e.i64("pid").map_or("-".to_string(), |p| p.to_string()),
                or_dash(s("release_version"))
            );
            if let Some(n) = e.i64("restart_count").filter(|n| *n > 0) {
                text.push_str(&format!(", restart #{}", n));
            }
            text
        }
        "ProcessCrashed" => {
            let code = e
                .i64("exit_code")
                .map_or("unknown".to_string(), |c| c.to_string());
            if e.bool("will_restart") {
                let delay = e.i64("restart_delay_ms").unwrap_or(0);
                format!(
                    "process exited with code {}, restarting in {}",
                    code,
                    human(Duration::milliseconds(delay))
                )
            } else {
                format!("process exited with code {}, not restarting", code)
            }
        }
        "ProcessFailed" => format!("process failed: {}", s("reason")),
        "ProcessStopped" => format!("process stopped ({})", or_dash(s("reason"))),
        "ProcessStartRequested" => "start requested".to_string(),
        "ProcessStopRequested" => "stop requested".to_string(),
        "ProcessRestartRequested" => "restart requested".to_string(),
        "WebhookSecretRotated" => "webhook secret rotated".to_string(),
        other => other.to_string(),
    }
}

/// `{"a": 1, "b": null}` -> "a=1"
fn fields(v: Option<&Value>) -> String {
    let Some(Value::Object(map)) = v else {
        return "-".to_string();
    };
    let parts: Vec<String> = map
        .iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| match v {
            Value::String(s) => format!("{}={}", k, s),
            other => format!("{}={}", k, other),
        })
        .collect();
    if parts.is_empty() {
        "none (all defaults)".to_string()
    } else {
        parts.join(", ")
    }
}

/// Something went wrong.
pub fn is_problem(e: &Ev) -> bool {
    matches!(
        e.event_type.as_str(),
        "ProcessCrashed"
            | "ProcessFailed"
            | "DeploymentFailed"
            | "RollbackFailed"
            | "ResourcePressureDetected"
    )
}

/// Something changed what runs, or how.
pub fn is_change(e: &Ev) -> bool {
    matches!(
        e.event_type.as_str(),
        "ReleasePromoted"
            | "RollbackCompleted"
            | "DeploymentStarted"
            | "ProjectSettingsUpdated"
            | "ResourceLimitsChanged"
            | "ProcessStartRequested"
            | "ProcessStopRequested"
            | "ProcessRestartRequested"
    )
}

fn problem_noun(e: &Ev) -> String {
    match e.event_type.as_str() {
        "ProcessCrashed" => "Crashes".to_string(),
        "ProcessFailed" => "The process failure".to_string(),
        "DeploymentFailed" => "A failed deployment".to_string(),
        "RollbackFailed" => "A failed rollback".to_string(),
        "ResourcePressureDetected" => format!("Resource alerts ({})", e.str("kind")),
        other => other.to_string(),
    }
}

fn by(e: &Ev) -> String {
    match e.actor() {
        "" => String::new(),
        actor => format!(" by {}", actor),
    }
}

/// "+1.2s", "+2m 5s"
fn offset(from: DateTime<Utc>, to: DateTime<Utc>) -> String {
    let d = to - from;
    if d.num_milliseconds() < 100_000 {
        format!("+{:.1}s", d.num_milliseconds() as f64 / 1000.0)
    } else {
        format!("+{}", human(d))
    }
}

// ----------------------------------------------------------------------
// replay
// ----------------------------------------------------------------------

/// How long after a deployment ends crashes are still shown in its replay.
const AFTERMATH: i64 = 120;

/// A deployment's history, step by step, with time offsets from the moment
/// it was queued. `events` is the project's full history; returns `None` if
/// nothing was recorded for the deployment.
pub fn replay_lines(events: &[Ev], deployment_id: &str) -> Option<Vec<String>> {
    let own: Vec<&Ev> = events
        .iter()
        .filter(|e| e.deployment_id() == deployment_id)
        .collect();
    let (first, last) = (own.first()?, own.last()?);
    let (start, end) = (first.at, last.at);
    let mut lines = Vec::new();
    let mut stage_started: Vec<(String, DateTime<Utc>)> = Vec::new();
    let mut outcome: Option<String> = None;

    for e in events {
        let mine = e.deployment_id() == deployment_id;
        let during = e.at >= start && e.at <= end;
        let related = matches!(
            e.event_type.as_str(),
            "ReleasePromoted"
                | "ProcessStarted"
                | "ProcessStopped"
                | "ProcessCrashed"
                | "ProcessFailed"
                | "ResourcePressureDetected"
        );
        if !(mine || (during && related)) {
            continue;
        }
        let mut text = describe(e);
        if e.event_type.starts_with("BuildStage") {
            let stage = e.str("stage").to_string();
            match e.str("status") {
                "Started" => stage_started.push((stage, e.at)),
                "Success" | "Failed" => {
                    if let Some((_, t)) = stage_started.iter().rev().find(|(s, _)| *s == stage) {
                        text.push_str(&format!(" (took {})", human(e.at - *t)));
                    }
                }
                _ => {}
            }
        }
        lines.push(format!("  {:>9}  {}{}", offset(start, e.at), text, by(e)));
        match e.event_type.as_str() {
            "DeploymentFailed" => {
                for line in e.log_tail() {
                    lines.push(format!("             | {}", line));
                }
                if !e.str("log_path").is_empty() {
                    lines.push(format!(
                        "             full build log: {}",
                        e.str("log_path")
                    ));
                }
                let restored = e.str("rolled_back_to");
                let after = if !restored.is_empty() {
                    format!(" Rolled back: {} kept serving.", restored)
                } else if e.str("stage") == "Promote" {
                    " The previous release was not running, so nothing was restored.".to_string()
                } else {
                    " The live release was not changed.".to_string()
                };
                outcome = Some(format!(
                    "Failed after {}{}: {}.{}",
                    human(e.at - start),
                    match e.str("stage") {
                        "" => String::new(),
                        stage => format!(" at stage {}", stage),
                    },
                    e.str("reason"),
                    after
                ));
            }
            "DeploymentCompleted" | "DeploymentSucceeded" => {
                outcome = Some(format!(
                    "Succeeded: {} went live after {}.",
                    e.str("version"),
                    human(e.at - start)
                ));
            }
            _ => {}
        }
    }

    let aftermath: Vec<&Ev> = events
        .iter()
        .filter(|e| e.at > end && e.at <= end + Duration::seconds(AFTERMATH))
        .filter(|e| matches!(e.event_type.as_str(), "ProcessCrashed" | "ProcessFailed"))
        .collect();
    if !aftermath.is_empty() {
        lines.push(format!("  Within {}s after the deployment:", AFTERMATH));
        for e in aftermath {
            lines.push(format!("  {:>9}  {}", offset(start, e.at), describe(e)));
        }
    }
    lines.push(String::new());
    lines.push(match outcome {
        Some(o) => format!("Outcome: {}", o),
        None => "Outcome: no result recorded yet (still running, or interrupted).".to_string(),
    });
    Some(lines)
}

// ----------------------------------------------------------------------
// incident
// ----------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct Incident {
    pub process: Option<ProcessState>,
    /// Crashes, failures and resource alerts in the window.
    pub problems: Vec<Ev>,
    pub crashes: usize,
    /// The most recent failed deployment and its failure event.
    pub failed_deployment: Option<(DeploymentState, Option<Ev>)>,
    /// True when that failed deployment is also the project's latest one.
    pub latest_deployment_failed: bool,
    /// The release to roll back to, if one is still on disk.
    pub rollback_target: Option<String>,
    pub advice: Vec<String>,
}

/// Looks at a project's replayed state and history since `since`, and says
/// what is wrong and what to do about it.
pub fn analyze_incident(
    engine: &ProjectionEngine,
    project: &ProjectState,
    events: &[Ev],
    since: DateTime<Utc>,
) -> Incident {
    let p = &project.name;
    let process = project
        .current_process
        .and_then(|id| engine.get_process(&id));
    let problems: Vec<Ev> = crate::history::window(events, since)
        .iter()
        .filter(|e| is_problem(e))
        .cloned()
        .collect();
    let crashes = problems
        .iter()
        .filter(|e| e.event_type == "ProcessCrashed")
        .count();

    let deployments: Vec<DeploymentState> = engine
        .get_deployments()
        .into_iter()
        .filter(|d| d.project_id == project.id)
        .collect();
    let failed = deployments
        .iter()
        .rev()
        .find(|d| matches!(d.status.as_str(), "Failed" | "RolledBack"))
        .cloned();
    let latest_deployment_failed = match (&failed, deployments.last()) {
        (Some(f), Some(l)) => f.id == l.id,
        _ => false,
    };
    let failed_deployment = failed.map(|d| {
        let id = d.id.to_string();
        let event = events
            .iter()
            .rev()
            .find(|e| e.event_type == "DeploymentFailed" && e.deployment_id() == id)
            .cloned();
        (d, event)
    });

    let current = project.current_release.clone().unwrap_or_default();
    let rollback_target = engine
        .releases_by_recency(&project.id)
        .into_iter()
        .find(|r| r.version != current && r.status == "Inactive")
        .map(|r| r.version);

    // When the current release went live, if that was in the window.
    let promoted_at = crate::history::window(events, since)
        .iter()
        .rev()
        .find(|e| {
            matches!(
                e.event_type.as_str(),
                "ReleasePromoted" | "RollbackCompleted"
            ) && (e.str("version") == current || e.str("to_version") == current)
        })
        .map(|e| e.at);
    let first_crash_after_promotion = promoted_at.and_then(|t| {
        problems
            .iter()
            .find(|e| {
                e.at >= t && matches!(e.event_type.as_str(), "ProcessCrashed" | "ProcessFailed")
            })
            .map(|e| e.at - t)
    });

    let mut advice = Vec::new();
    let suggest_rollback = |advice: &mut Vec<String>| {
        if let Some(target) = &rollback_target {
            let why = match first_crash_after_promotion {
                Some(d) => format!(
                    " ({} went live in this window and the crashes started {} later)",
                    current,
                    human(d)
                ),
                None => String::new(),
            };
            advice.push(format!(
                "Roll back to the previous release {}{}: aegis rollback {} --version {}",
                target, why, p, target
            ));
        }
    };
    match &process {
        None => advice.push(format!(
            "{} has never been deployed: aegis deploy {}",
            p, p
        )),
        Some(proc) => match proc.status.as_str() {
            "Failed" => {
                suggest_rollback(&mut advice);
                advice.push(format!(
                    "The supervisor gave up restarting it (crash loop or spawn failure). Read why: aegis logs {} -n 100",
                    p
                ));
                advice.push(format!(
                    "After fixing the cause, redeploy (aegis deploy {}) or start it again (aegis start {}).",
                    p, p
                ));
            }
            "Backoff" => {
                suggest_rollback(&mut advice);
                advice.push(format!(
                    "It is crashing and being restarted with backoff. Read the crash output: aegis logs {} -n 100",
                    p
                ));
            }
            "Stopped" | "Exited" if proc.desired == "stopped" => advice.push(format!(
                "The process was stopped on purpose. Start it with: aegis start {}",
                p
            )),
            "Exited" | "Stopped" => advice.push(format!(
                "The process exited{} and was not restarted (check deploy.restart_policy). Start it with: aegis start {}",
                proc.last_exit_code
                    .map_or(String::new(), |c| format!(" with code {}", c)),
                p
            )),
            _ => {
                if crashes > 0 {
                    if first_crash_after_promotion.is_some() {
                        suggest_rollback(&mut advice);
                    }
                    advice.push(format!(
                        "It is running now but crashed {} time(s) in this window. Check: aegis logs {} -n 200",
                        crashes, p
                    ));
                }
            }
        },
    }

    let mut kinds: Vec<&str> = problems
        .iter()
        .filter(|e| e.event_type == "ResourcePressureDetected")
        .map(|e| e.str("kind"))
        .collect();
    kinds.sort_unstable();
    kinds.dedup();
    let limits_hint = format!(
        "(dashboard Resources tab, or PUT /api/v1/projects/{}/resources)",
        project.id
    );
    for kind in kinds {
        match kind {
            "oom_kill" => advice.push(format!(
                "The app was killed for exceeding its memory limit ({}). Raise the limit {} or reduce its memory use.",
                project
                    .resources
                    .memory_bytes
                    .map_or("unknown".to_string(), crate::bytes),
                limits_hint
            )),
            "memory_pressure" => advice.push(format!(
                "Memory use stayed near the limit. Raise it {} before it is OOM-killed.",
                limits_hint
            )),
            "cpu_throttled" => advice.push(format!(
                "The app was CPU-throttled ({} cores allowed). Raise the CPU limit {} if it is slow.",
                project
                    .resources
                    .cpu_cores
                    .map_or("unknown".to_string(), |c| c.to_string()),
                limits_hint
            )),
            other => advice.push(format!("Resource alert '{}' was raised; see the timeline.", other)),
        }
    }

    if latest_deployment_failed {
        if let Some((d, event)) = &failed_deployment {
            let stage = d.stage.clone().unwrap_or_default();
            let restored = event
                .as_ref()
                .map(|e| e.str("rolled_back_to").to_string())
                .unwrap_or_default();
            let live = if !restored.is_empty() {
                format!("{} was restored and is serving", restored)
            } else if current.is_empty() {
                "nothing is live".to_string()
            } else {
                format!("{} is still the live release", current)
            };
            advice.push(format!(
                "The latest deployment {} failed at {}: {}. {}. Fix it and redeploy (aegis deploy {}); step by step: aegis replay {}",
                short(&d.id.to_string()),
                if stage.is_empty() { "an unknown stage" } else { stage.as_str() },
                d.error.clone().unwrap_or_default(),
                live,
                p,
                short(&d.id.to_string())
            ));
        }
    }

    if advice.is_empty() {
        advice.push(
            "No problems found: the process is running and nothing failed in this window."
                .to_string(),
        );
    }

    Incident {
        process,
        problems,
        crashes,
        failed_deployment,
        latest_deployment_failed,
        rollback_target,
        advice,
    }
}

// ----------------------------------------------------------------------
// investigate
// ----------------------------------------------------------------------

/// What happened since `since`, and the change that most likely set off the
/// first problem. `events` is the full history, oldest first.
pub fn investigate_summary(events: &[Ev], since: DateTime<Utc>) -> Vec<String> {
    let recent = crate::history::window(events, since);
    let count = |pred: &dyn Fn(&Ev) -> bool| recent.iter().filter(|e| pred(e)).count();
    let crashes = count(&|e| e.event_type == "ProcessCrashed");
    let failures = count(&|e| e.event_type == "ProcessFailed");
    let failed_deploys = count(&|e| e.event_type == "DeploymentFailed");
    let alerts = count(&|e| e.event_type == "ResourcePressureDetected");
    let deploys = count(&|e| e.event_type == "DeploymentCompleted");
    let changes = count(&is_change);

    let mut summary = vec![format!(
        "{} events: {} successful deployment(s), {} failed, {} crash(es), {} crash-loop failure(s), {} resource alert(s), {} change(s).",
        recent.len(),
        deploys,
        failed_deploys,
        crashes,
        failures,
        alerts,
        changes
    )];

    let Some(first_problem) = recent.iter().find(|e| is_problem(e)) else {
        summary
            .push("No crashes, failed deployments or resource alerts in this window.".to_string());
        return summary;
    };
    let idx = events
        .iter()
        .position(|e| std::ptr::eq(e, first_problem))
        .unwrap_or(0);
    let cause = events[..idx].iter().rev().find(|e| is_change(e));
    // A deployment failure is caused by that deployment, not by an older change.
    let cause = if first_problem.event_type == "DeploymentFailed" {
        events[..idx]
            .iter()
            .rev()
            .find(|e| {
                e.event_type == "DeploymentQueued"
                    && e.deployment_id() == first_problem.deployment_id()
            })
            .or(cause)
    } else {
        cause
    };
    let noun = problem_noun(first_problem);
    match cause {
        Some(c) => {
            let gap = first_problem.at - c.at;
            let earlier = if c.at < since {
                " (before this window)"
            } else {
                ""
            };
            summary.push(format!(
                "Likely trigger: {} began {} after {}{}{}.",
                noun,
                human(gap),
                describe(c),
                by(c),
                earlier
            ));
            if gap > Duration::hours(6) {
                summary.push(
                    "That change was long before the first problem, so it may be unrelated: also consider load, dependencies or the host."
                        .to_string(),
                );
            }
        }
        None => summary.push(format!(
            "{} began at {} with no recorded change before it: look outside Aegis (load, dependencies, the host).",
            noun,
            first_problem.at.format("%Y-%m-%d %H:%M:%S UTC")
        )),
    }

    let last_problem = recent.iter().rev().find(|e| is_problem(e));
    if let Some(last) = last_problem {
        let recovery = recent.iter().rev().find(|e| {
            e.at > last.at
                && matches!(
                    e.event_type.as_str(),
                    "ReleasePromoted" | "RollbackCompleted" | "DeploymentCompleted"
                )
        });
        match recovery {
            Some(r) => summary.push(format!(
                "Last problem at {}; since then: {}{}.",
                last.at.format("%H:%M:%S"),
                describe(r),
                by(r)
            )),
            None => summary.push(format!(
                "Last problem at {} ({}); nothing has been deployed or rolled back since.",
                last.at.format("%Y-%m-%d %H:%M:%S UTC"),
                describe(last)
            )),
        }
    }
    summary
}

/// One timeline row: time, actor, description.
pub fn timeline_line(e: &Ev) -> String {
    let marker = if is_problem(e) {
        "!"
    } else if is_change(e) {
        "*"
    } else {
        " "
    };
    format!(
        "{} {}  {:<12} {}",
        marker,
        e.at.format("%Y-%m-%d %H:%M:%S"),
        if e.actor().is_empty() {
            "daemon"
        } else {
            e.actor()
        },
        describe(e)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{ev, parse_at, replay_until};
    use serde_json::json;

    const P: &str = "01900000-0000-7000-8000-000000000001";
    const D1: &str = "01900000-0000-7000-8000-0000000000d1";
    const D2: &str = "01900000-0000-7000-8000-0000000000d2";
    const R1: &str = "01900000-0000-7000-8000-0000000000a1";
    const R2: &str = "01900000-0000-7000-8000-0000000000a2";
    const PROC1: &str = "01900000-0000-7000-8000-0000000000c1";
    const PROC2: &str = "01900000-0000-7000-8000-0000000000c2";

    /// r1 deployed at 09:00, r2 deployed at 10:00 and crash-looping since.
    fn history() -> Vec<Ev> {
        vec![
            ev(
                "ProjectCreated",
                "2026-08-03T08:59:00Z",
                json!({"project_id": P, "name": "app"}),
            ),
            ev(
                "DeploymentQueued",
                "2026-08-03T09:00:00Z",
                json!({"project_id": P, "deployment_id": D1, "trigger_source": "cli", "actor": "cli"}),
            ),
            ev(
                "DeploymentStarted",
                "2026-08-03T09:00:00.100Z",
                json!({"project_id": P, "deployment_id": D1, "version": "r1"}),
            ),
            ev(
                "BuildStageBuild",
                "2026-08-03T09:00:01Z",
                json!({"project_id": P, "deployment_id": D1, "stage": "Build", "status": "Started"}),
            ),
            ev(
                "BuildStageBuild",
                "2026-08-03T09:00:03.500Z",
                json!({"project_id": P, "deployment_id": D1, "stage": "Build", "status": "Success"}),
            ),
            ev(
                "ReleaseCreated",
                "2026-08-03T09:00:04Z",
                json!({"project_id": P, "deployment_id": D1, "release_id": R1, "version": "r1"}),
            ),
            ev(
                "ProcessStarted",
                "2026-08-03T09:00:05Z",
                json!({"project_id": P, "process_id": PROC1, "pid": 10, "release_version": "r1"}),
            ),
            ev(
                "ReleasePromoted",
                "2026-08-03T09:00:06Z",
                json!({"project_id": P, "release_id": R1, "version": "r1"}),
            ),
            ev(
                "DeploymentCompleted",
                "2026-08-03T09:00:06.200Z",
                json!({"project_id": P, "deployment_id": D1, "release_id": R1, "version": "r1"}),
            ),
            ev(
                "DeploymentQueued",
                "2026-08-03T10:00:00Z",
                json!({"project_id": P, "deployment_id": D2, "trigger_source": "webhook", "actor": "webhook:github"}),
            ),
            ev(
                "DeploymentStarted",
                "2026-08-03T10:00:00.100Z",
                json!({"project_id": P, "deployment_id": D2, "version": "r2"}),
            ),
            ev(
                "ReleaseCreated",
                "2026-08-03T10:00:04Z",
                json!({"project_id": P, "deployment_id": D2, "release_id": R2, "version": "r2"}),
            ),
            ev(
                "ProcessStopped",
                "2026-08-03T10:00:05Z",
                json!({"project_id": P, "process_id": PROC1, "reason": "replaced"}),
            ),
            ev(
                "ProcessStarted",
                "2026-08-03T10:00:05.500Z",
                json!({"project_id": P, "process_id": PROC2, "pid": 20, "release_version": "r2"}),
            ),
            ev(
                "ReleasePromoted",
                "2026-08-03T10:00:06Z",
                json!({"project_id": P, "release_id": R2, "version": "r2", "previous_version": "r1"}),
            ),
            ev(
                "DeploymentCompleted",
                "2026-08-03T10:00:06.300Z",
                json!({"project_id": P, "deployment_id": D2, "release_id": R2, "version": "r2"}),
            ),
            ev(
                "ProcessCrashed",
                "2026-08-03T10:00:46Z",
                json!({"project_id": P, "process_id": PROC2, "exit_code": 1, "will_restart": true, "restart_delay_ms": 1000}),
            ),
            ev(
                "ProcessStarted",
                "2026-08-03T10:00:47Z",
                json!({"project_id": P, "process_id": PROC2, "pid": 21, "restart_count": 1}),
            ),
            ev(
                "ProcessCrashed",
                "2026-08-03T10:00:50Z",
                json!({"project_id": P, "process_id": PROC2, "exit_code": 1, "will_restart": false}),
            ),
            ev(
                "ProcessFailed",
                "2026-08-03T10:00:50.100Z",
                json!({"project_id": P, "process_id": PROC2, "reason": "Crash loop: 5 restarts in 60s"}),
            ),
        ]
    }

    fn at(s: &str) -> DateTime<Utc> {
        parse_at(s).unwrap()
    }

    #[test]
    fn describes_events() {
        let e = ev(
            "ProcessCrashed",
            "2026-08-03T10:00:46Z",
            json!({"exit_code": 137, "will_restart": true, "restart_delay_ms": 2000}),
        );
        assert_eq!(
            describe(&e),
            "process exited with code 137, restarting in 2.0s"
        );
        let e = ev(
            "ProjectSettingsUpdated",
            "2026-08-03T10:00:46Z",
            json!({"settings": {"port": 4000, "start_command": "node a.js", "build_command": null}}),
        );
        assert_eq!(
            describe(&e),
            "settings changed: port=4000, start_command=node a.js"
        );
        let e = ev(
            "ReleasePromoted",
            "2026-08-03T10:00:46Z",
            json!({"version": "r2", "previous_version": "r1"}),
        );
        assert_eq!(describe(&e), "release r2 went live (replacing r1)");
        let e = ev("SomethingNew", "2026-08-03T10:00:46Z", json!({}));
        assert_eq!(describe(&e), "SomethingNew");
    }

    #[test]
    fn replays_a_deployment_step_by_step() {
        let events = history();
        let lines = replay_lines(&events, D1).unwrap();
        let text = lines.join("\n");
        assert!(
            lines[0].contains("+0.0s") && lines[0].contains("queued (trigger cli"),
            "{}",
            text
        );
        assert!(text.contains("Build Success (took 2.5s)"), "{}", text);
        assert!(
            text.contains("+5.0s") && text.contains("process started (pid 10, release r1)"),
            "{}",
            text
        );
        assert!(
            text.contains("Outcome: Succeeded: r1 went live after 6.2s."),
            "{}",
            text
        );
        // The later deployment's events are not part of this replay.
        assert!(!text.contains("r2"), "{}", text);

        // Crashes right after the second deployment are shown as its aftermath.
        let text = replay_lines(&events, D2).unwrap().join("\n");
        assert!(
            text.contains("Within 120s after the deployment"),
            "{}",
            text
        );
        assert!(text.contains("process failed: Crash loop"), "{}", text);
        assert!(replay_lines(&events, "nope").is_none());
    }

    #[test]
    fn replays_a_failed_deployment_with_its_log() {
        let events = vec![
            ev(
                "DeploymentQueued",
                "2026-08-03T10:00:00Z",
                json!({"project_id": P, "deployment_id": D1}),
            ),
            ev(
                "BuildStageBuild",
                "2026-08-03T10:00:01Z",
                json!({"project_id": P, "deployment_id": D1, "stage": "Build", "status": "Started"}),
            ),
            ev(
                "BuildStageBuild",
                "2026-08-03T10:00:04Z",
                json!({"project_id": P, "deployment_id": D1, "stage": "Build", "status": "Failed", "detail": "exit 2"}),
            ),
            ev(
                "DeploymentFailed",
                "2026-08-03T10:00:04.100Z",
                json!({"project_id": P, "deployment_id": D1, "stage": "Build", "reason": "build command failed", "log_tail": ["error: missing ;"], "log_path": "/x/deploy.log"}),
            ),
        ];
        let text = replay_lines(&events, D1).unwrap().join("\n");
        assert!(
            text.contains("Build Failed: exit 2 (took 3.0s)"),
            "{}",
            text
        );
        assert!(text.contains("| error: missing ;"), "{}", text);
        assert!(text.contains("full build log: /x/deploy.log"), "{}", text);
        assert!(
            text.contains("Outcome: Failed after 4.1s at stage Build: build command failed. The live release was not changed."),
            "{}",
            text
        );
    }

    #[test]
    fn incident_advises_rollback_after_a_bad_release() {
        let events = history();
        let (engine, _) = replay_until(&events, None);
        let project = engine.get_projects().pop().unwrap();
        let incident = analyze_incident(&engine, &project, &events, at("2026-08-03T09:30:00Z"));
        assert_eq!(incident.process.as_ref().unwrap().status, "Failed");
        assert_eq!(incident.crashes, 2);
        assert_eq!(incident.rollback_target.as_deref(), Some("r1"));
        assert!(!incident.latest_deployment_failed);
        assert!(
            incident.advice[0].starts_with("Roll back to the previous release r1 (r2 went live in this window and the crashes started 40s later): aegis rollback app --version r1"),
            "{:?}",
            incident.advice
        );
        assert!(incident.advice.iter().any(|a| a.contains("aegis logs app")));
    }

    #[test]
    fn incident_reports_failed_deploys_and_resource_alerts() {
        let mut events = history();
        events.truncate(9); // only r1, healthy
        events.push(ev(
            "DeploymentQueued",
            "2026-08-03T11:00:00Z",
            json!({"project_id": P, "deployment_id": D2}),
        ));
        events.push(ev(
            "BuildStageBuild",
            "2026-08-03T11:00:01Z",
            json!({"project_id": P, "deployment_id": D2, "stage": "Build", "status": "Failed"}),
        ));
        events.push(ev("DeploymentFailed", "2026-08-03T11:00:02Z", json!({"project_id": P, "deployment_id": D2, "stage": "Build", "reason": "build command failed", "log_tail": ["boom"]})));
        events.push(ev(
            "ResourcePressureDetected",
            "2026-08-03T11:05:00Z",
            json!({"project_id": P, "kind": "oom_kill", "message": "killed"}),
        ));
        let (engine, _) = replay_until(&events, None);
        let project = engine.get_projects().pop().unwrap();
        let incident = analyze_incident(&engine, &project, &events, at("2026-08-03T10:30:00Z"));
        assert!(incident.latest_deployment_failed);
        let (dep, event) = incident.failed_deployment.as_ref().unwrap();
        assert_eq!(dep.error.as_deref(), Some("build command failed"));
        assert_eq!(event.as_ref().unwrap().log_tail(), vec!["boom".to_string()]);
        let advice = incident.advice.join("\n");
        assert!(
            advice.contains("killed for exceeding its memory limit"),
            "{}",
            advice
        );
        assert!(
            advice.contains("failed at Build: build command failed. r1 is still the live release."),
            "{}",
            advice
        );
        assert!(!advice.contains("Roll back"), "{}", advice);
        assert_eq!(incident.problems.len(), 2);
    }

    #[test]
    fn incident_with_nothing_wrong() {
        let mut events = history();
        events.truncate(9);
        let (engine, _) = replay_until(&events, None);
        let project = engine.get_projects().pop().unwrap();
        let incident = analyze_incident(&engine, &project, &events, at("2026-08-03T08:00:00Z"));
        assert_eq!(incident.advice.len(), 1);
        assert!(incident.advice[0].starts_with("No problems found"));
    }

    #[test]
    fn investigate_finds_the_triggering_release() {
        let events = history();
        let summary = investigate_summary(&events, at("2026-08-03T09:30:00Z"));
        assert!(
            summary[0].contains(
                "1 successful deployment(s), 0 failed, 2 crash(es), 1 crash-loop failure(s)"
            ),
            "{:?}",
            summary
        );
        assert_eq!(
            summary[1],
            "Likely trigger: Crashes began 40s after release r2 went live (replacing r1)."
        );
        assert!(
            summary[2].contains("nothing has been deployed or rolled back since"),
            "{:?}",
            summary
        );

        // The trigger can be before the window.
        let summary = investigate_summary(&events, at("2026-08-03T10:00:30Z"));
        assert!(
            summary[1].ends_with("(before this window)."),
            "{:?}",
            summary
        );

        // A quiet window.
        let summary = investigate_summary(&events[..9], at("2026-08-03T08:00:00Z"));
        assert_eq!(
            summary[1],
            "No crashes, failed deployments or resource alerts in this window."
        );
    }

    #[test]
    fn investigate_blames_the_deployment_for_its_own_failure() {
        let events = vec![
            ev(
                "ProjectSettingsUpdated",
                "2026-08-03T09:00:00Z",
                json!({"project_id": P, "settings": {"port": 1}, "actor": "user:ann"}),
            ),
            ev(
                "DeploymentQueued",
                "2026-08-03T10:00:00Z",
                json!({"project_id": P, "deployment_id": D1, "trigger_source": "cli", "actor": "cli"}),
            ),
            ev(
                "DeploymentFailed",
                "2026-08-03T10:00:30Z",
                json!({"project_id": P, "deployment_id": D1, "stage": "Test", "reason": "tests failed"}),
            ),
            ev(
                "ProcessRestartRequested",
                "2026-08-03T10:05:00Z",
                json!({"project_id": P, "actor": "cli"}),
            ),
        ];
        let summary = investigate_summary(&events, at("2026-08-03T08:00:00Z"));
        assert!(
            summary[1]
                .starts_with("Likely trigger: A failed deployment began 30s after deployment"),
            "{:?}",
            summary
        );
        assert!(summary[1].ends_with("by cli."), "{:?}", summary);
        assert_eq!(timeline_line(&events[0]).chars().next(), Some('*'));
        assert_eq!(timeline_line(&events[2]).chars().next(), Some('!'));
        assert!(timeline_line(&events[0]).contains("user:ann"));
    }

    #[test]
    fn investigate_without_a_prior_change() {
        let events = vec![ev(
            "ProcessCrashed",
            "2026-08-03T10:00:00Z",
            json!({"project_id": P, "exit_code": 1}),
        )];
        let summary = investigate_summary(&events, at("2026-08-03T08:00:00Z"));
        assert!(
            summary[1].contains("with no recorded change before it"),
            "{:?}",
            summary
        );
    }
}
