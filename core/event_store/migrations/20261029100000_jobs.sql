-- Scheduled and one-off commands of an app ("cron jobs").
CREATE TABLE jobs (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    -- Five-field cron expression (UTC) or @hourly, @daily, @weekly, @monthly
    schedule TEXT NOT NULL,
    command TEXT NOT NULL,
    timeout_secs INTEGER NOT NULL DEFAULT 3600,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    UNIQUE (project_id, name)
);

CREATE TABLE job_runs (
    id TEXT PRIMARY KEY NOT NULL,
    job_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    -- schedule, manual
    trigger TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    -- running, success, failed, timeout, skipped
    status TEXT NOT NULL,
    exit_code INTEGER,
    -- The last part of the combined output.
    output_tail TEXT NOT NULL DEFAULT ''
);
CREATE INDEX job_runs_job ON job_runs (job_id, started_at);
