//! Jobs: commands that run in an app's live release, on a cron schedule or
//! on demand (migrations, cleanups, reports). A job runs in the release
//! directory with the app's environment (variables, env file, secrets), its
//! combined output's tail is kept with the run, and a run that outlives its
//! timeout is killed with its whole process group.

use crate::cron::CronSchedule;
use crate::{ControlError, ControlPlane, ControlResult};
use aegis_engine::ResolvedProjectConfig;
use aegis_types::ProjectId;
use chrono::Utc;
use serde_json::json;
use sqlx::Row;
use std::collections::HashMap;
use std::time::Duration;

/// Output kept per run.
const OUTPUT_TAIL_BYTES: usize = 32 * 1024;
/// Runs kept per job.
const RUNS_KEPT: i64 = 50;
const DEFAULT_TIMEOUT_SECS: u64 = 3600;
const MAX_TIMEOUT_SECS: u64 = 24 * 3600;

#[derive(Debug, Clone)]
pub struct Job {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub schedule: String,
    pub command: String,
    pub timeout_secs: u64,
    pub enabled: bool,
    pub created_at: String,
    /// The next scheduled run (RFC 3339, UTC), for enabled jobs.
    pub next_run: Option<String>,
    /// The latest run, if any.
    pub last_run: Option<JobRun>,
}

#[derive(Debug, Clone)]
pub struct JobRun {
    pub id: String,
    pub job_id: String,
    pub project_id: String,
    /// `schedule` or `manual`.
    pub trigger: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// `running`, `success`, `failed`, `timeout` or `skipped`.
    pub status: String,
    pub exit_code: Option<i32>,
    pub output_tail: String,
}

#[derive(Debug, Clone, Default)]
pub struct JobSpec {
    pub name: String,
    pub schedule: String,
    pub command: String,
    pub timeout_secs: Option<u64>,
    pub enabled: Option<bool>,
}

/// Fields of a job to change; `None` keeps the current value.
#[derive(Debug, Clone, Default)]
pub struct JobUpdate {
    pub name: Option<String>,
    pub schedule: Option<String>,
    pub command: Option<String>,
    pub timeout_secs: Option<u64>,
    pub enabled: Option<bool>,
}

fn invalid(message: impl Into<String>) -> ControlError {
    ControlError::InvalidArgument(message.into())
}

fn check_name(name: &str) -> ControlResult<String> {
    let name = name.trim();
    let ok = !name.is_empty()
        && name.chars().count() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-'));
    if ok {
        Ok(name.to_string())
    } else {
        Err(invalid(
            "A job name is 1-64 letters, digits, spaces, '.', '_' or '-'",
        ))
    }
}

fn check_command(command: &str) -> ControlResult<String> {
    let command = command.trim();
    if command.is_empty() || command.len() > 4000 || command.contains('\0') {
        return Err(invalid("A job command is 1-4000 characters of text"));
    }
    Ok(command.to_string())
}

fn check_timeout(secs: u64) -> ControlResult<u64> {
    if (1..=MAX_TIMEOUT_SECS).contains(&secs) {
        Ok(secs)
    } else {
        Err(invalid(format!(
            "A job's timeout is 1-{} seconds",
            MAX_TIMEOUT_SECS
        )))
    }
}

fn check_schedule(schedule: &str) -> ControlResult<String> {
    CronSchedule::parse(schedule).map_err(invalid)?;
    Ok(schedule.trim().to_string())
}

fn db(e: sqlx::Error) -> ControlError {
    ControlError::Internal(e.into())
}

fn run_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<JobRun, sqlx::Error> {
    Ok(JobRun {
        id: row.try_get("id")?,
        job_id: row.try_get("job_id")?,
        project_id: row.try_get("project_id")?,
        trigger: row.try_get("trigger")?,
        started_at: row.try_get("started_at")?,
        finished_at: row.try_get("finished_at")?,
        status: row.try_get("status")?,
        exit_code: row.try_get("exit_code")?,
        output_tail: row.try_get("output_tail")?,
    })
}

impl ControlPlane {
    fn pool(&self) -> &sqlx::SqlitePool {
        self.inner.store.pool()
    }

    fn job_from_row(&self, row: &sqlx::sqlite::SqliteRow) -> Result<Job, sqlx::Error> {
        let schedule: String = row.try_get("schedule")?;
        let enabled = row.try_get::<i64, _>("enabled")? != 0;
        let next_run = enabled
            .then(|| CronSchedule::parse(&schedule).ok())
            .flatten()
            .and_then(|c| c.next_after(Utc::now()))
            .map(|t| t.to_rfc3339());
        Ok(Job {
            id: row.try_get("id")?,
            project_id: row.try_get("project_id")?,
            name: row.try_get("name")?,
            schedule,
            command: row.try_get("command")?,
            timeout_secs: row.try_get::<i64, _>("timeout_secs")? as u64,
            enabled,
            created_at: row.try_get("created_at")?,
            next_run,
            last_run: None,
        })
    }

    async fn job_of(&self, project_id: ProjectId, job_id: &str) -> ControlResult<Job> {
        let row = sqlx::query("SELECT * FROM jobs WHERE id = ? AND project_id = ?")
            .bind(job_id)
            .bind(project_id.to_string())
            .fetch_optional(self.pool())
            .await
            .map_err(db)?
            .ok_or_else(|| ControlError::NotFound(format!("Unknown job '{}'", job_id)))?;
        self.job_from_row(&row).map_err(db)
    }

    fn job_project(&self, project_id: ProjectId) -> ControlResult<aegis_projection::ProjectState> {
        self.inner
            .projection
            .get_project(&project_id)
            .ok_or_else(|| ControlError::NotFound(format!("Unknown project {}", project_id)))
    }

    pub async fn list_jobs(&self, project_id: ProjectId) -> ControlResult<Vec<Job>> {
        self.job_project(project_id)?;
        let rows = sqlx::query("SELECT * FROM jobs WHERE project_id = ? ORDER BY name")
            .bind(project_id.to_string())
            .fetch_all(self.pool())
            .await
            .map_err(db)?;
        let mut jobs = Vec::new();
        for row in &rows {
            let mut job = self.job_from_row(row).map_err(db)?;
            job.last_run = sqlx::query(
                "SELECT * FROM job_runs WHERE job_id = ? ORDER BY started_at DESC LIMIT 1",
            )
            .bind(&job.id)
            .fetch_optional(self.pool())
            .await
            .map_err(db)?
            .map(|r| run_from_row(&r))
            .transpose()
            .map_err(db)?;
            jobs.push(job);
        }
        Ok(jobs)
    }

    pub async fn create_job(&self, project_id: ProjectId, spec: JobSpec) -> ControlResult<Job> {
        let project = self.job_project(project_id)?;
        let name = check_name(&spec.name)?;
        let schedule = check_schedule(&spec.schedule)?;
        let command = check_command(&spec.command)?;
        let timeout = check_timeout(spec.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS))?;
        let id = aegis_types::EventId::new().to_string();
        let inserted = sqlx::query(
            "INSERT INTO jobs (id, project_id, name, schedule, command, timeout_secs, enabled, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(project_id, name) DO NOTHING",
        )
        .bind(&id)
        .bind(project_id.to_string())
        .bind(&name)
        .bind(&schedule)
        .bind(&command)
        .bind(timeout as i64)
        .bind(i64::from(spec.enabled.unwrap_or(true)))
        .bind(Utc::now().to_rfc3339())
        .execute(self.pool())
        .await
        .map_err(db)?
        .rows_affected();
        if inserted == 0 {
            return Err(ControlError::FailedPrecondition(format!(
                "'{}' already has a job named '{}'",
                project.name, name
            )));
        }
        self.record(
            "JobCreated",
            json!({"project_id": project_id.to_string(), "job_id": id, "name": name, "schedule": schedule}),
        )
        .await?;
        self.job_of(project_id, &id).await
    }

    pub async fn update_job(
        &self,
        project_id: ProjectId,
        job_id: &str,
        update: JobUpdate,
    ) -> ControlResult<Job> {
        let job = self.job_of(project_id, job_id).await?;
        let name = update
            .name
            .as_deref()
            .map(check_name)
            .transpose()?
            .unwrap_or(job.name.clone());
        let schedule = update
            .schedule
            .as_deref()
            .map(check_schedule)
            .transpose()?
            .unwrap_or(job.schedule.clone());
        let command = update
            .command
            .as_deref()
            .map(check_command)
            .transpose()?
            .unwrap_or(job.command.clone());
        let timeout = update
            .timeout_secs
            .map(check_timeout)
            .transpose()?
            .unwrap_or(job.timeout_secs);
        let enabled = update.enabled.unwrap_or(job.enabled);
        let changed = sqlx::query(
            "UPDATE jobs SET name = ?, schedule = ?, command = ?, timeout_secs = ?, enabled = ?
             WHERE id = ? AND NOT EXISTS (SELECT 1 FROM jobs WHERE project_id = ? AND name = ? AND id != ?)",
        )
        .bind(&name)
        .bind(&schedule)
        .bind(&command)
        .bind(timeout as i64)
        .bind(i64::from(enabled))
        .bind(job_id)
        .bind(project_id.to_string())
        .bind(&name)
        .bind(job_id)
        .execute(self.pool())
        .await
        .map_err(db)?
        .rows_affected();
        if changed == 0 {
            return Err(ControlError::FailedPrecondition(format!(
                "There is already a job named '{}'",
                name
            )));
        }
        self.record(
            "JobUpdated",
            json!({"project_id": project_id.to_string(), "job_id": job_id, "name": name,
                   "schedule": schedule, "enabled": enabled}),
        )
        .await?;
        self.job_of(project_id, job_id).await
    }

    pub async fn delete_job(&self, project_id: ProjectId, job_id: &str) -> ControlResult<()> {
        let job = self.job_of(project_id, job_id).await?;
        sqlx::query("DELETE FROM job_runs WHERE job_id = ?")
            .bind(job_id)
            .execute(self.pool())
            .await
            .map_err(db)?;
        sqlx::query("DELETE FROM jobs WHERE id = ?")
            .bind(job_id)
            .execute(self.pool())
            .await
            .map_err(db)?;
        self.record(
            "JobDeleted",
            json!({"project_id": project_id.to_string(), "job_id": job_id, "name": job.name}),
        )
        .await?;
        Ok(())
    }

    /// Removes every job of an app that is being deleted.
    pub(crate) async fn delete_jobs(&self, project_id: ProjectId) -> Result<(), sqlx::Error> {
        let key = project_id.to_string();
        sqlx::query("DELETE FROM job_runs WHERE project_id = ?")
            .bind(&key)
            .execute(self.pool())
            .await?;
        sqlx::query("DELETE FROM jobs WHERE project_id = ?")
            .bind(&key)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn list_job_runs(
        &self,
        project_id: ProjectId,
        job_id: &str,
        limit: i64,
    ) -> ControlResult<Vec<JobRun>> {
        self.job_of(project_id, job_id).await?;
        let rows =
            sqlx::query("SELECT * FROM job_runs WHERE job_id = ? ORDER BY started_at DESC LIMIT ?")
                .bind(job_id)
                .bind(limit.clamp(1, RUNS_KEPT))
                .fetch_all(self.pool())
                .await
                .map_err(db)?;
        rows.iter()
            .map(run_from_row)
            .collect::<Result<_, _>>()
            .map_err(db)
    }

    /// Starts a job now. Returns the run (still `running`); it finishes in
    /// the background. Refused while the same job is already running.
    pub async fn run_job_now(&self, project_id: ProjectId, job_id: &str) -> ControlResult<JobRun> {
        let job = self.job_of(project_id, job_id).await?;
        self.start_job_run(job, "manual").await
    }

    async fn start_job_run(&self, job: Job, trigger: &str) -> ControlResult<JobRun> {
        let project_id: ProjectId = job
            .project_id
            .parse()
            .map_err(|_| ControlError::NotFound("Unknown project".into()))?;
        let project = self.job_project(project_id)?;
        let started_at = Utc::now().to_rfc3339();
        let run_id = aegis_types::EventId::new().to_string();
        let new_run = |status: &str, finished: bool, output: &str| JobRun {
            id: run_id.clone(),
            job_id: job.id.clone(),
            project_id: job.project_id.clone(),
            trigger: trigger.to_string(),
            started_at: started_at.clone(),
            finished_at: finished.then(|| started_at.clone()),
            status: status.to_string(),
            exit_code: None,
            output_tail: output.to_string(),
        };
        // Why this run can't happen now.
        let refusal: Option<String> = if !self
            .inner
            .running_jobs
            .lock()
            .unwrap()
            .insert(job.id.clone())
        {
            Some("The previous run of this job is still running".into())
        } else if let Some(release) = project.current_release.clone() {
            match ResolvedProjectConfig::load(&self.switcher(project_id).get_version_dir(&release))
            {
                Ok(config) if config.is_docker() => {
                    self.inner.running_jobs.lock().unwrap().remove(&job.id);
                    Some("Jobs are not supported for container apps yet".into())
                }
                Ok(_) => None,
                Err(e) => {
                    self.inner.running_jobs.lock().unwrap().remove(&job.id);
                    Some(format!("The live release has an invalid aegis.toml: {}", e))
                }
            }
        } else {
            self.inner.running_jobs.lock().unwrap().remove(&job.id);
            Some(format!("'{}' has no live release to run in", project.name))
        };
        if let Some(reason) = refusal {
            // Someone asking now gets the reason; only a missed schedule is recorded.
            if trigger == "manual" {
                return Err(ControlError::FailedPrecondition(reason));
            }
            let run = new_run("skipped", true, &reason);
            self.store_run(&run).await?;
            return Ok(run);
        }
        let run = new_run("running", false, "");
        self.store_run(&run).await?;
        self.record(
            "JobRunStarted",
            json!({"project_id": job.project_id, "job_id": job.id, "run_id": run.id,
                   "name": job.name, "trigger": trigger}),
        )
        .await?;
        let this = self.clone();
        let started = std::time::Instant::now();
        let finished_run = run.clone();
        tokio::spawn(async move {
            let (status, exit_code, output) = this.execute_job(project_id, &job).await;
            let finished_at = Utc::now().to_rfc3339();
            let _ = sqlx::query(
                "UPDATE job_runs SET status = ?, exit_code = ?, output_tail = ?, finished_at = ? WHERE id = ?",
            )
            .bind(&status)
            .bind(exit_code)
            .bind(&output)
            .bind(&finished_at)
            .bind(&finished_run.id)
            .execute(this.pool())
            .await;
            let _ = sqlx::query(
                "DELETE FROM job_runs WHERE job_id = ? AND id NOT IN
                 (SELECT id FROM job_runs WHERE job_id = ? ORDER BY started_at DESC LIMIT ?)",
            )
            .bind(&job.id)
            .bind(&job.id)
            .bind(RUNS_KEPT)
            .execute(this.pool())
            .await;
            this.inner.running_jobs.lock().unwrap().remove(&job.id);
            let _ = this
                .record(
                    "JobRunFinished",
                    json!({"project_id": job.project_id, "job_id": job.id, "run_id": finished_run.id,
                           "name": job.name, "trigger": finished_run.trigger, "status": status,
                           "exit_code": exit_code, "duration_ms": started.elapsed().as_millis() as u64}),
                )
                .await;
        });
        Ok(run)
    }

    async fn store_run(&self, run: &JobRun) -> ControlResult<()> {
        sqlx::query(
            "INSERT INTO job_runs (id, job_id, project_id, trigger, started_at, finished_at, status, exit_code, output_tail)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&run.id)
        .bind(&run.job_id)
        .bind(&run.project_id)
        .bind(&run.trigger)
        .bind(&run.started_at)
        .bind(&run.finished_at)
        .bind(&run.status)
        .bind(run.exit_code)
        .bind(&run.output_tail)
        .execute(self.pool())
        .await
        .map_err(db)?;
        Ok(())
    }

    /// Runs the command; returns (status, exit code, output tail).
    #[cfg(unix)]
    async fn execute_job(&self, project_id: ProjectId, job: &Job) -> (String, Option<i32>, String) {
        use tokio::io::AsyncReadExt;
        let fail = |message: String| ("failed".to_string(), None, message);
        let Some(project) = self.inner.projection.get_project(&project_id) else {
            return fail("The app was deleted".into());
        };
        let Some(release) = project.current_release else {
            return fail("The app has no live release".into());
        };
        let dir = self.switcher(project_id).get_version_dir(&release);
        let config = match ResolvedProjectConfig::load(&dir) {
            Ok(c) => c,
            Err(e) => return fail(format!("Invalid aegis.toml: {}", e)),
        };
        let mut env: HashMap<String, String> = match self.process_env(project_id, &config) {
            Ok(env) => env,
            Err(e) => return fail(e.to_string()),
        };
        for key in ["PATH", "HOME", "LANG", "TMPDIR"] {
            if let (false, Ok(v)) = (env.contains_key(key), std::env::var(key)) {
                env.insert(key.to_string(), v);
            }
        }
        env.insert("AEGIS_JOB".into(), job.name.clone());
        env.insert("AEGIS_RELEASE_VERSION".into(), release);
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg(format!("exec 2>&1\n{}", job.command))
            .current_dir(&dir)
            .env_clear()
            .envs(env)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return fail(format!("Could not start the command: {}", e)),
        };
        let pid = child.id();
        let mut stdout = child.stdout.take().expect("piped");
        let timeout = Duration::from_secs(job.timeout_secs);
        let mut tail: Vec<u8> = Vec::new();
        let result = tokio::time::timeout(timeout, async {
            let mut buf = [0u8; 8192];
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        tail.extend_from_slice(&buf[..n]);
                        if tail.len() > OUTPUT_TAIL_BYTES * 2 {
                            tail.drain(..tail.len() - OUTPUT_TAIL_BYTES);
                        }
                    }
                }
            }
            child.wait().await
        })
        .await;
        let (status, code) = match result {
            Ok(Ok(exit)) => (
                if exit.success() { "success" } else { "failed" },
                exit.code(),
            ),
            Ok(Err(e)) => {
                tail.extend_from_slice(format!("\n[wait failed: {}]", e).as_bytes());
                ("failed", None)
            }
            Err(_) => {
                if let Some(pid) = pid {
                    // Kill the command and everything it started.
                    unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                }
                let _ = child.wait().await;
                tail.extend_from_slice(
                    format!("\n[killed after the {}s timeout]", job.timeout_secs).as_bytes(),
                );
                ("timeout", None)
            }
        };
        if tail.len() > OUTPUT_TAIL_BYTES {
            tail.drain(..tail.len() - OUTPUT_TAIL_BYTES);
        }
        (
            status.to_string(),
            code,
            String::from_utf8_lossy(&tail).into_owned(),
        )
    }

    #[cfg(not(unix))]
    async fn execute_job(
        &self,
        _project_id: ProjectId,
        _job: &Job,
    ) -> (String, Option<i32>, String) {
        ("failed".into(), None, "Jobs need a Unix host".into())
    }

    /// Starts due jobs once a minute (UTC) and marks runs that a restart
    /// interrupted as failed.
    pub(crate) fn spawn_job_scheduler(&self) {
        let weak = std::sync::Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            if let Some(inner) = weak.upgrade() {
                let this = ControlPlane { inner };
                let _ = sqlx::query(
                    "UPDATE job_runs SET status = 'failed', finished_at = ?, output_tail = output_tail || char(10) || '[the daemon restarted during this run]' WHERE status = 'running'",
                )
                .bind(Utc::now().to_rfc3339())
                .execute(this.pool())
                .await;
            }
            let mut last_minute = Utc::now().timestamp() / 60;
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                let Some(inner) = weak.upgrade() else { return };
                let this = ControlPlane { inner };
                if this
                    .inner
                    .shutting_down
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    return;
                }
                let now = Utc::now();
                let minute = now.timestamp() / 60;
                if minute <= last_minute {
                    continue;
                }
                last_minute = minute;
                let Ok(rows) = sqlx::query("SELECT * FROM jobs WHERE enabled = 1")
                    .fetch_all(this.pool())
                    .await
                else {
                    continue;
                };
                for row in &rows {
                    let Ok(job) = this.job_from_row(row) else {
                        continue;
                    };
                    let due = CronSchedule::parse(&job.schedule).is_ok_and(|c| c.matches(now));
                    if due {
                        if let Err(e) = this.start_job_run(job, "schedule").await {
                            tracing::warn!(error = %e, "Could not start a scheduled job");
                        }
                    }
                }
            }
        });
    }
}
