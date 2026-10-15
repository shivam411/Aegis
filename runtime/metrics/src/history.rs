//! Metric history for charts: the last hour of samples in memory, and
//! per-minute rollups (average and peak) in SQLite for longer ranges.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One measurement of an app (or the host).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// Unix milliseconds.
    pub at: i64,
    /// CPU used since the previous sample, in cores (1.0 = one full core).
    pub cpu_cores: f64,
    pub memory_bytes: u64,
    pub cpu_limit_cores: Option<f64>,
    pub memory_limit_bytes: Option<u64>,
    /// Share of CPU periods in which the app hit its CPU limit, 0-1.
    pub throttled_ratio: f64,
    pub threads: u64,
    pub fds: u64,
    pub procs: u64,
    pub restarts: u32,
    /// Out-of-memory kills in the app's current cgroup.
    pub oom_kills: u64,
}

/// A point on a chart: the average and peak over its time bucket.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Point {
    /// Unix milliseconds, start of the bucket.
    pub at: i64,
    pub cpu_cores: f64,
    pub cpu_cores_max: f64,
    pub memory_bytes: u64,
    pub memory_bytes_max: u64,
    pub cpu_limit_cores: Option<f64>,
    pub memory_limit_bytes: Option<u64>,
    pub throttled_ratio: f64,
}

#[derive(Debug, Clone, Default)]
struct Bucket {
    start: i64,
    n: u64,
    cpu_sum: f64,
    cpu_max: f64,
    mem_sum: u128,
    mem_max: u64,
    thr_sum: f64,
    cpu_limit: Option<f64>,
    mem_limit: Option<u64>,
}

impl Bucket {
    fn new(start: i64) -> Self {
        Self {
            start,
            ..Default::default()
        }
    }

    /// Adds `n` samples summarised by these values.
    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        n: u64,
        cpu_avg: f64,
        cpu_max: f64,
        mem_avg: u64,
        mem_max: u64,
        thr_avg: f64,
        cpu_limit: Option<f64>,
        mem_limit: Option<u64>,
    ) {
        self.n += n;
        self.cpu_sum += cpu_avg * n as f64;
        self.cpu_max = self.cpu_max.max(cpu_max);
        self.mem_sum += mem_avg as u128 * n as u128;
        self.mem_max = self.mem_max.max(mem_max);
        self.thr_sum += thr_avg * n as f64;
        // The latest limit in the bucket wins.
        self.cpu_limit = cpu_limit;
        self.mem_limit = mem_limit;
    }

    fn add_sample(&mut self, s: &Sample) {
        self.add(
            1,
            s.cpu_cores,
            s.cpu_cores,
            s.memory_bytes,
            s.memory_bytes,
            s.throttled_ratio,
            s.cpu_limit_cores,
            s.memory_limit_bytes,
        );
    }

    fn point(&self) -> Point {
        let n = self.n.max(1);
        Point {
            at: self.start,
            cpu_cores: self.cpu_sum / n as f64,
            cpu_cores_max: self.cpu_max,
            memory_bytes: (self.mem_sum / n as u128) as u64,
            memory_bytes_max: self.mem_max,
            cpu_limit_cores: self.cpu_limit,
            memory_limit_bytes: self.mem_limit,
            throttled_ratio: self.thr_sum / n as f64,
        }
    }
}

#[derive(Default)]
struct Series {
    ring: VecDeque<Sample>,
    minute: Option<Bucket>,
}

struct Inner {
    pool: Option<SqlitePool>,
    ring_ms: i64,
    retention_secs: i64,
    series: Mutex<HashMap<String, Series>>,
}

#[derive(Clone)]
pub struct History {
    inner: Arc<Inner>,
}

const MINUTE_MS: i64 = 60_000;

impl History {
    /// `pool` must have the `metrics_rollup` table (see the event store's
    /// migrations); without one, only the in-memory hour is kept.
    pub fn new(pool: Option<SqlitePool>, ring: Duration, retention: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                pool,
                ring_ms: ring.as_millis() as i64,
                retention_secs: retention.as_secs() as i64,
                series: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Adds a sample. When it starts a new minute, the previous minute's
    /// rollup is written to the database.
    pub async fn record(&self, series: &str, sample: Sample) {
        let finished = {
            let mut all = self.inner.series.lock().unwrap();
            let s = all.entry(series.to_string()).or_default();
            let cutoff = sample.at - self.inner.ring_ms;
            while s.ring.front().is_some_and(|x| x.at < cutoff) {
                s.ring.pop_front();
            }
            let minute = sample.at - sample.at.rem_euclid(MINUTE_MS);
            let finished = match &s.minute {
                Some(b) if b.start != minute => s.minute.take(),
                _ => None,
            };
            s.minute
                .get_or_insert_with(|| Bucket::new(minute))
                .add_sample(&sample);
            s.ring.push_back(sample);
            finished
        };
        if let Some(bucket) = finished {
            self.write(series, &bucket).await;
        }
    }

    async fn write(&self, series: &str, b: &Bucket) {
        let Some(pool) = &self.inner.pool else { return };
        let p = b.point();
        let result = sqlx::query(
            "INSERT OR REPLACE INTO metrics_rollup \
             (series, minute, samples, cpu_avg, cpu_max, memory_avg, memory_max, cpu_limit, memory_limit, throttled_avg) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(series)
        .bind(b.start / 1000)
        .bind(b.n as i64)
        .bind(p.cpu_cores)
        .bind(p.cpu_cores_max)
        .bind(p.memory_bytes as i64)
        .bind(p.memory_bytes_max as i64)
        .bind(p.cpu_limit_cores)
        .bind(p.memory_limit_bytes.map(|m| m as i64))
        .bind(p.throttled_ratio)
        .execute(pool)
        .await;
        if let Err(e) = result {
            tracing::warn!(error = %e, series, "Could not store metrics rollup");
        }
    }

    /// Writes the current (incomplete) minute of every series, e.g. before
    /// shutting down.
    pub async fn flush(&self) {
        let buckets: Vec<(String, Bucket)> = {
            let all = self.inner.series.lock().unwrap();
            all.iter()
                .filter_map(|(k, s)| s.minute.clone().map(|b| (k.clone(), b)))
                .collect()
        };
        for (series, bucket) in buckets {
            self.write(&series, &bucket).await;
        }
    }

    pub fn latest(&self, series: &str) -> Option<Sample> {
        self.inner
            .series
            .lock()
            .unwrap()
            .get(series)
            .and_then(|s| s.ring.back().cloned())
    }

    /// Chart points covering the last `range`, at most `max_points` of them.
    /// Ranges within the in-memory hour use the raw samples; longer ones use
    /// the per-minute rollups.
    pub async fn range(
        &self,
        series: &str,
        now_ms: i64,
        range: Duration,
        max_points: usize,
    ) -> Result<Vec<Point>, sqlx::Error> {
        let range_ms = range.as_millis() as i64;
        let start = now_ms - range_ms;
        let max_points = max_points.max(1) as i64;
        if range_ms <= self.inner.ring_ms || self.inner.pool.is_none() {
            let width = (range_ms / max_points).max(1);
            let samples: Vec<Sample> = self
                .inner
                .series
                .lock()
                .unwrap()
                .get(series)
                .map(|s| s.ring.iter().filter(|x| x.at >= start).cloned().collect())
                .unwrap_or_default();
            let mut buckets: Vec<Bucket> = Vec::new();
            for s in &samples {
                let b_start = start + (s.at - start) / width * width;
                if buckets.last().is_none_or(|b| b.start != b_start) {
                    buckets.push(Bucket::new(b_start));
                }
                buckets.last_mut().unwrap().add_sample(s);
            }
            return Ok(buckets.iter().map(Bucket::point).collect());
        }

        let pool = self.inner.pool.as_ref().unwrap();
        type Row = (i64, i64, f64, f64, i64, i64, Option<f64>, Option<i64>, f64);
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT minute, samples, cpu_avg, cpu_max, memory_avg, memory_max, cpu_limit, memory_limit, throttled_avg \
             FROM metrics_rollup WHERE series = ? AND minute >= ? ORDER BY minute",
        )
        .bind(series)
        .bind(start / 1000)
        .fetch_all(pool)
        .await?;
        let width = (range_ms / max_points).max(MINUTE_MS);
        let mut buckets: Vec<Bucket> = Vec::new();
        let mut add = |at: i64, f: &dyn Fn(&mut Bucket)| {
            let b_start = start + (at - start).max(0) / width * width;
            if buckets.last().is_none_or(|b| b.start != b_start) {
                buckets.push(Bucket::new(b_start));
            }
            f(buckets.last_mut().unwrap());
        };
        let mut last_minute = i64::MIN;
        for (minute, n, cpu, cpu_max, mem, mem_max, cpu_lim, mem_lim, thr) in rows {
            last_minute = minute * 1000;
            add(minute * 1000, &|b| {
                b.add(
                    n.max(0) as u64,
                    cpu,
                    cpu_max,
                    mem.max(0) as u64,
                    mem_max.max(0) as u64,
                    thr,
                    cpu_lim,
                    mem_lim.map(|m| m.max(0) as u64),
                )
            });
        }
        // Include the minute in progress.
        let current = self
            .inner
            .series
            .lock()
            .unwrap()
            .get(series)
            .and_then(|s| s.minute.clone());
        if let Some(c) = current.filter(|c| c.start > last_minute && c.n > 0) {
            let p = c.point();
            add(c.start, &|b| {
                b.add(
                    c.n,
                    p.cpu_cores,
                    p.cpu_cores_max,
                    p.memory_bytes,
                    p.memory_bytes_max,
                    p.throttled_ratio,
                    p.cpu_limit_cores,
                    p.memory_limit_bytes,
                )
            });
        }
        Ok(buckets.iter().map(Bucket::point).collect())
    }

    /// Deletes rollups older than the retention period.
    pub async fn prune(&self, now_ms: i64) -> Result<u64, sqlx::Error> {
        let Some(pool) = &self.inner.pool else {
            return Ok(0);
        };
        let cutoff = now_ms / 1000 - self.inner.retention_secs;
        let res = sqlx::query("DELETE FROM metrics_rollup WHERE minute < ?")
            .bind(cutoff)
            .execute(pool)
            .await?;
        Ok(res.rows_affected())
    }

    /// Forgets a series held in memory (its rollups stay until pruned).
    pub fn forget(&self, series: &str) {
        self.inner.series.lock().unwrap().remove(series);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(include_str!(
            "../../../core/event_store/migrations/20261001000000_metrics.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    fn sample(at: i64, cpu: f64, mem: u64) -> Sample {
        Sample {
            at,
            cpu_cores: cpu,
            memory_bytes: mem,
            memory_limit_bytes: Some(1000),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_ring_keeps_the_last_hour_and_downsamples() {
        let h = History::new(None, Duration::from_secs(3600), Duration::from_secs(86400));
        let t0 = 1_700_000_000_000;
        // Two hours of samples every 2 s.
        for i in 0..3600 {
            h.record("app", sample(t0 + i * 2000, (i % 2) as f64, 100 + i as u64))
                .await;
        }
        let now = t0 + 3599 * 2000;
        assert_eq!(h.latest("app").unwrap().at, now);
        let points = h
            .range("app", now, Duration::from_secs(60), 1000)
            .await
            .unwrap();
        assert!((30..=31).contains(&points.len()), "{}", points.len());
        let points = h
            .range("app", now, Duration::from_secs(3600), 60)
            .await
            .unwrap();
        assert!(points.len() <= 61, "{}", points.len());
        // Averages and peaks per bucket.
        let p = &points[points.len() / 2];
        assert!((p.cpu_cores - 0.5).abs() < 0.05, "{p:?}");
        assert_eq!(p.cpu_cores_max, 1.0);
        assert!(p.memory_bytes_max > p.memory_bytes);
        assert_eq!(p.memory_limit_bytes, Some(1000));
        // Nothing older than an hour is kept in memory.
        let all = h
            .range("app", now, Duration::from_secs(7200), 7200)
            .await
            .unwrap();
        assert!(all.first().unwrap().at >= now - 3_600_000);
        assert!(h
            .range("other", now, Duration::from_secs(60), 10)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn test_minute_rollups_are_stored_queried_and_pruned() {
        let h = History::new(
            Some(pool().await),
            Duration::from_secs(600),
            Duration::from_secs(3 * 3600),
        );
        let t0 = 1_700_000_000_000 - 1_700_000_000_000 % MINUTE_MS;
        // Five hours, one sample every 10 s, CPU = hour number.
        for i in 0..(5 * 360) {
            let at = t0 + i * 10_000;
            h.record("app", sample(at, (i / 360) as f64, 500)).await;
        }
        let now = t0 + (5 * 360 - 1) * 10_000;
        let points = h
            .range("app", now, Duration::from_secs(5 * 3600), 5)
            .await
            .unwrap();
        assert_eq!(points.len(), 5, "{points:?}");
        for (hour, p) in points.iter().enumerate() {
            assert!((p.cpu_cores - hour as f64).abs() < 0.2, "{hour}: {p:?}");
            assert_eq!(p.memory_bytes, 500);
        }
        // The minute in progress is included.
        let recent = h
            .range("app", now, Duration::from_secs(2 * 3600), 1000)
            .await
            .unwrap();
        assert!(recent.last().unwrap().at >= now - MINUTE_MS);

        let removed = h.prune(now).await.unwrap();
        assert!(removed >= 110, "{removed}");
        let left = h
            .range("app", now, Duration::from_secs(5 * 3600), 1000)
            .await
            .unwrap();
        assert!(left.first().unwrap().at >= now - 3 * 3_600_000 - MINUTE_MS);
    }
}
