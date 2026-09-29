-- One row per series (an app, or "host") per minute: averages and peaks of
-- the 2-second samples, kept for the configured retention (default 7 days).
CREATE TABLE IF NOT EXISTS metrics_rollup (
    series TEXT NOT NULL,
    minute INTEGER NOT NULL,          -- unix seconds, start of the minute
    samples INTEGER NOT NULL,
    cpu_avg REAL NOT NULL,            -- cores
    cpu_max REAL NOT NULL,
    memory_avg INTEGER NOT NULL,      -- bytes
    memory_max INTEGER NOT NULL,
    cpu_limit REAL,
    memory_limit INTEGER,
    throttled_avg REAL NOT NULL,      -- share of CPU periods throttled, 0-1
    PRIMARY KEY (series, minute)
);
CREATE INDEX IF NOT EXISTS idx_metrics_rollup_minute ON metrics_rollup (minute);
