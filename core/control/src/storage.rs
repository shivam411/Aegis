//! Disk use under the data directory, and cleanup of what can be rebuilt or
//! is no longer needed: old builds (release directories outside the rollback
//! window), old logs, old backups and temporary files. The live release,
//! the newest rollback targets, the database and the secrets key are never
//! touched.

use crate::{ControlPlane, ControlResult};
use aegis_types::ProjectId;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// Log files untouched for this long are reclaimable by default.
pub const DEFAULT_LOG_DAYS: u64 = 14;
/// Temporary files older than this are reclaimable.
const TMP_MAX_AGE: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageCategory {
    /// "releases", "old_builds", "logs", "backups", "database" or "temp".
    pub key: String,
    pub label: String,
    pub bytes: u64,
    /// What a cleanup would free now; 0 when the category is never cleaned.
    pub reclaimable_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppStorage {
    pub project_id: String,
    pub name: String,
    pub releases_bytes: u64,
    pub old_builds_bytes: u64,
    pub logs_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageReport {
    pub total_bytes: u64,
    pub reclaimable_bytes: u64,
    pub categories: Vec<StorageCategory>,
    pub apps: Vec<AppStorage>,
}

/// What a cleanup freed, by category.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanupResult {
    pub freed_bytes: u64,
    pub removed: Vec<(String, u64)>,
}

pub const CLEANUP_TARGETS: [&str; 4] = ["old_builds", "logs", "backups", "temp"];

/// Size of everything below `path`, without following symlinks.
pub fn dir_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            total += dir_size(&entry.path());
        }
    }
    total
}

fn older_than(path: &Path, age: Duration) -> bool {
    std::fs::symlink_metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|d| d >= age)
}

/// Files directly in `dir` that are at least `age` old: (path, size).
fn old_files(dir: &Path, age: Duration) -> Vec<(std::path::PathBuf, u64)> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if older_than(&path, age) {
                out.push((path.clone(), dir_size(&path)));
            }
        }
    }
    out
}

impl ControlPlane {
    /// Release versions to keep for a project: the live one plus the newest
    /// rollback targets (same rule as pruning after a deployment).
    fn retained_versions(&self, project_id: ProjectId, max: usize) -> Vec<String> {
        self.inner
            .projection
            .releases_by_recency(&project_id)
            .into_iter()
            .filter(|r| matches!(r.status.as_str(), "Active" | "Inactive"))
            .take(max.max(1))
            .map(|r| r.version)
            .collect()
    }

    /// Release directories outside the rollback window: (version, size).
    fn old_build_dirs(&self, project_id: ProjectId) -> Vec<(std::path::PathBuf, u64)> {
        let keep = self.retained_versions(project_id, self.inner.settings.max_retained_versions);
        let active = self
            .inner
            .projection
            .get_project(&project_id)
            .and_then(|p| p.current_release);
        let releases = self.project_dir(project_id).join("releases");
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&releases) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let path = entry.path();
                if !path.is_dir() || keep.contains(&name) || active.as_deref() == Some(&name) {
                    continue;
                }
                out.push((path.clone(), dir_size(&path)));
            }
        }
        out
    }

    fn log_files(&self, project_id: ProjectId, age: Duration) -> Vec<(std::path::PathBuf, u64)> {
        old_files(&self.project_dir(project_id).join("logs"), age)
    }

    fn old_backups(&self) -> Vec<(std::path::PathBuf, u64)> {
        let keep = self.backup_settings().keep.max(1);
        self.list_backups()
            .unwrap_or_default()
            .into_iter()
            .skip(keep)
            .map(|b| (b.path, b.size_bytes))
            .collect()
    }

    fn temp_files(&self) -> Vec<(std::path::PathBuf, u64)> {
        old_files(&self.data_dir().join("tmp"), TMP_MAX_AGE)
    }

    /// Disk use by category and by app. Reads the disk, so it runs on a
    /// blocking thread.
    pub async fn storage_report(&self, log_days: u64) -> ControlResult<StorageReport> {
        let control = self.clone();
        let report = tokio::task::spawn_blocking(move || control.storage_report_blocking(log_days))
            .await
            .map_err(anyhow::Error::from)?;
        Ok(report)
    }

    fn storage_report_blocking(&self, log_days: u64) -> StorageReport {
        let age = Duration::from_secs(log_days.max(1) * 24 * 3600);
        let mut apps = Vec::new();
        let (mut releases, mut old_builds, mut logs, mut logs_reclaim) = (0u64, 0u64, 0u64, 0u64);
        for view in self.list_projects() {
            let id = view.project.id;
            let dir = self.project_dir(id);
            let old: u64 = self.old_build_dirs(id).iter().map(|(_, s)| s).sum();
            let all_releases = dir_size(&dir.join("releases"));
            let app_logs = dir_size(&dir.join("logs"));
            logs_reclaim += self.log_files(id, age).iter().map(|(_, s)| s).sum::<u64>();
            releases += all_releases.saturating_sub(old);
            old_builds += old;
            logs += app_logs;
            apps.push(AppStorage {
                project_id: id.to_string(),
                name: view.project.name.clone(),
                releases_bytes: all_releases.saturating_sub(old),
                old_builds_bytes: old,
                logs_bytes: app_logs,
            });
        }
        apps.sort_by_key(|a| {
            std::cmp::Reverse(a.releases_bytes + a.old_builds_bytes + a.logs_bytes)
        });

        let data = self.data_dir();
        let db: u64 = ["aegis.db", "aegis.db-wal", "aegis.db-shm"]
            .iter()
            .map(|n| dir_size(&data.join(n)))
            .sum();
        let backups = dir_size(&self.backups_dir());
        let backups_reclaim: u64 = self.old_backups().iter().map(|(_, s)| s).sum();
        let temp = dir_size(&data.join("tmp"));
        let temp_reclaim: u64 = self.temp_files().iter().map(|(_, s)| s).sum();

        let categories = vec![
            StorageCategory {
                key: "releases".into(),
                label: "Live and rollback releases".into(),
                bytes: releases,
                reclaimable_bytes: 0,
            },
            StorageCategory {
                key: "old_builds".into(),
                label: "Old builds".into(),
                bytes: old_builds,
                reclaimable_bytes: old_builds,
            },
            StorageCategory {
                key: "logs".into(),
                label: "Logs".into(),
                bytes: logs,
                reclaimable_bytes: logs_reclaim,
            },
            StorageCategory {
                key: "backups".into(),
                label: "Backups and database snapshots".into(),
                bytes: backups,
                reclaimable_bytes: backups_reclaim,
            },
            StorageCategory {
                key: "database".into(),
                label: "Database".into(),
                bytes: db,
                reclaimable_bytes: 0,
            },
            StorageCategory {
                key: "temp".into(),
                label: "Temporary files".into(),
                bytes: temp,
                reclaimable_bytes: temp_reclaim,
            },
        ];
        let total_bytes = dir_size(data);
        let reclaimable_bytes = categories.iter().map(|c| c.reclaimable_bytes).sum();
        StorageReport {
            total_bytes,
            reclaimable_bytes,
            categories,
            apps,
        }
    }

    /// Removes what the chosen targets (see [`TARGETS`]) say is no longer
    /// needed. Recorded as a `StorageCleaned` event.
    pub async fn storage_cleanup(
        &self,
        targets: &[String],
        log_days: u64,
        trigger: &str,
    ) -> ControlResult<CleanupResult> {
        let control = self.clone();
        let wanted = targets.to_vec();
        let result =
            tokio::task::spawn_blocking(move || control.cleanup_blocking(&wanted, log_days))
                .await
                .map_err(anyhow::Error::from)?;
        if result.freed_bytes > 0 || trigger == "manual" {
            let _ = self
                .record(
                    "StorageCleaned",
                    json!({
                        "trigger": trigger,
                        "targets": targets,
                        "freed_bytes": result.freed_bytes,
                        "removed": result.removed.iter()
                            .map(|(k, b)| json!({"target": k, "bytes": b}))
                            .collect::<Vec<_>>(),
                    }),
                )
                .await;
        }
        Ok(result)
    }

    fn cleanup_blocking(&self, targets: &[String], log_days: u64) -> CleanupResult {
        let age = Duration::from_secs(log_days.max(1) * 24 * 3600);
        let mut result = CleanupResult::default();
        let mut remove = |target: &str, items: Vec<(std::path::PathBuf, u64)>| {
            let mut freed = 0;
            for (path, size) in items {
                let gone = if path.is_dir() {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
                match gone {
                    Ok(()) => freed += size,
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "Cleanup could not remove")
                    }
                }
            }
            result.freed_bytes += freed;
            result.removed.push((target.to_string(), freed));
        };
        for target in targets {
            match target.as_str() {
                "old_builds" => {
                    let items: Vec<_> = self
                        .list_projects()
                        .into_iter()
                        .flat_map(|v| self.old_build_dirs(v.project.id))
                        .collect();
                    remove("old_builds", items);
                }
                "logs" => {
                    let items: Vec<_> = self
                        .list_projects()
                        .into_iter()
                        .flat_map(|v| self.log_files(v.project.id, age))
                        .collect();
                    remove("logs", items);
                }
                "backups" => remove("backups", self.old_backups()),
                "temp" => remove("temp", self.temp_files()),
                _ => {}
            }
        }
        result
    }
}
