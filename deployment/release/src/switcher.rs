use aegis_types::ProjectId;
use std::path::PathBuf;
use tokio::fs;

pub struct ReleaseSwitcher {
    pub base_dir: PathBuf,
    pub project_id: ProjectId,
}

impl ReleaseSwitcher {
    pub fn new(base_dir: PathBuf, project_id: ProjectId) -> Self {
        let app_dir = base_dir.join(project_id.to_string());
        Self {
            base_dir: app_dir,
            project_id,
        }
    }

    /// Returns the directory path for a specific release version.
    pub fn get_version_dir(&self, version: &str) -> PathBuf {
        self.base_dir.join("releases").join(version)
    }

    /// Returns the active pointer file/path.
    pub fn get_current_pointer_path(&self) -> PathBuf {
        self.base_dir.join("current.json")
    }

    /// Rejects version names that could escape the releases directory.
    pub fn validate_version(version: &str) -> Result<(), anyhow::Error> {
        let valid = !version.is_empty()
            && version.len() <= 128
            && !version.starts_with('.')
            && version
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'));
        if !valid {
            anyhow::bail!(
                "Invalid release version '{}': use letters, digits, '.', '_', '-' or '+'",
                version
            );
        }
        Ok(())
    }

    /// Creates the directory structure for a new release version without affecting active deployments.
    pub async fn prepare_version_dir(&self, version: &str) -> Result<PathBuf, anyhow::Error> {
        Self::validate_version(version)?;
        let version_dir = self.get_version_dir(version);
        fs::create_dir_all(&version_dir).await?;
        tracing::info!(
            project_id = %self.project_id,
            version = %version,
            path = %version_dir.display(),
            "Prepared isolated release version directory"
        );
        Ok(version_dir)
    }

    /// Atomically switches the active release version pointer to the new target version.
    pub async fn switch_to_version(&self, version: &str) -> Result<PathBuf, anyhow::Error> {
        Self::validate_version(version)?;
        let version_dir = self.get_version_dir(version);
        if !version_dir.exists() {
            anyhow::bail!(
                "Cannot switch: Release version directory {} does not exist",
                version_dir.display()
            );
        }

        let pointer_path = self.get_current_pointer_path();
        let payload = serde_json::json!({
            "project_id": self.project_id.to_string(),
            "active_version": version,
            "switched_at": chrono::Utc::now().to_rfc3339(),
            "version_path": version_dir.to_str().unwrap_or_default(),
        });

        // Atomic write via temp file rename
        let temp_pointer = self.base_dir.join(format!("current_tmp_{}.json", version));
        fs::write(&temp_pointer, serde_json::to_string_pretty(&payload)?).await?;
        fs::rename(&temp_pointer, &pointer_path).await?;

        tracing::info!(
            project_id = %self.project_id,
            active_version = %version,
            "Atomically switched current active release version"
        );

        Ok(version_dir)
    }

    /// Retrieves the currently active release version string.
    pub async fn get_active_version(&self) -> Result<Option<String>, anyhow::Error> {
        let pointer_path = self.get_current_pointer_path();
        if !pointer_path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(pointer_path).await?;
        let json: serde_json::Value = serde_json::from_str(&content)?;
        Ok(json
            .get("active_version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()))
    }

    /// Deletes every release directory except the ones in `keep` and the
    /// active one. Returns the versions that were removed.
    pub async fn prune_versions(&self, keep: &[String]) -> Result<Vec<String>, anyhow::Error> {
        let releases_dir = self.base_dir.join("releases");
        if !releases_dir.exists() {
            return Ok(Vec::new());
        }
        let active_version = self.get_active_version().await?.unwrap_or_default();
        let mut removed = Vec::new();
        let mut dir = fs::read_dir(&releases_dir).await?;
        while let Some(entry) = dir.next_entry().await? {
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name == active_version || keep.contains(&name) {
                continue;
            }
            match fs::remove_dir_all(entry.path()).await {
                Ok(()) => {
                    tracing::info!(project_id = %self.project_id, version = %name, "Pruned old release");
                    removed.push(name);
                }
                Err(e) => {
                    tracing::warn!(version = %name, error = %e, "Failed to prune release directory")
                }
            }
        }
        removed.sort();
        Ok(removed)
    }

    /// Automatically prunes old release version directories exceeding the retention limit (default: 2).
    /// The currently active release version is guaranteed to never be purged.
    pub async fn cleanup_old_versions(&self, max_retained: usize) -> Result<usize, anyhow::Error> {
        let releases_dir = self.base_dir.join("releases");
        if !releases_dir.exists() {
            return Ok(0);
        }

        let active_version = self.get_active_version().await?.unwrap_or_default();
        let mut entries = Vec::new();
        let mut dir = fs::read_dir(&releases_dir).await?;

        while let Some(entry) = dir.next_entry().await? {
            if entry.file_type().await?.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                let meta = entry.metadata().await?;
                let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                entries.push((name, entry.path(), modified));
            }
        }

        if entries.len() <= max_retained {
            return Ok(0);
        }

        // Sort chronologically (oldest first)
        entries.sort_by_key(|(_, _, modified)| *modified);

        let mut purged = 0;
        let remove_count = entries.len().saturating_sub(max_retained);

        for (name, path, _) in entries.into_iter() {
            if purged >= remove_count {
                break;
            }
            if name == active_version {
                tracing::info!(version = %name, "Skipping purge of active version");
                continue;
            }

            tracing::info!(
                project_id = %self.project_id,
                version = %name,
                path = %path.display(),
                "Auto-deleting old release version exceeding max retention"
            );
            if fs::remove_dir_all(&path).await.is_ok() {
                purged += 1;
            }
        }

        Ok(purged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_release_switcher_atomic_handover() {
        let temp = TempDir::new().unwrap();
        let proj_id = ProjectId::new();
        let switcher = ReleaseSwitcher::new(temp.path().to_path_buf(), proj_id);

        // Prepare v1.0.0 and v2.0.0
        let v1_dir = switcher.prepare_version_dir("v1.0.0").await.unwrap();
        let v2_dir = switcher.prepare_version_dir("v2.0.0").await.unwrap();
        assert!(v1_dir.exists());
        assert!(v2_dir.exists());

        // Switch to v1.0.0
        switcher.switch_to_version("v1.0.0").await.unwrap();
        assert_eq!(
            switcher.get_active_version().await.unwrap(),
            Some("v1.0.0".to_string())
        );

        // Gracefully switch to v2.0.0
        switcher.switch_to_version("v2.0.0").await.unwrap();
        assert_eq!(
            switcher.get_active_version().await.unwrap(),
            Some("v2.0.0".to_string())
        );
    }

    #[tokio::test]
    async fn test_cleanup_old_versions_auto_prune() {
        let temp = TempDir::new().unwrap();
        let proj_id = ProjectId::new();
        let switcher = ReleaseSwitcher::new(temp.path().to_path_buf(), proj_id);

        let v1 = switcher.prepare_version_dir("v1.0.0").await.unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        let v2 = switcher.prepare_version_dir("v2.0.0").await.unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        let v3 = switcher.prepare_version_dir("v3.0.0").await.unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        let v4 = switcher.prepare_version_dir("v4.0.0").await.unwrap();

        // Switch to v4.0.0
        switcher.switch_to_version("v4.0.0").await.unwrap();

        // Cleanup with max_retained = 2 (retains v3.0.0 and v4.0.0, auto-deletes v1.0.0 and v2.0.0)
        let purged = switcher.cleanup_old_versions(2).await.unwrap();
        assert_eq!(purged, 2);

        assert!(!v1.exists());
        assert!(!v2.exists());
        assert!(v3.exists());
        assert!(v4.exists());
    }

    #[tokio::test]
    async fn test_prune_versions_keeps_listed_and_active() {
        let temp = TempDir::new().unwrap();
        let switcher = ReleaseSwitcher::new(temp.path().to_path_buf(), ProjectId::new());
        for v in ["r1", "r2", "r3", "r4"] {
            switcher.prepare_version_dir(v).await.unwrap();
        }
        switcher.switch_to_version("r1").await.unwrap();
        let removed = switcher
            .prune_versions(&["r3".to_string(), "r4".to_string()])
            .await
            .unwrap();
        assert_eq!(removed, vec!["r2".to_string()]);
        assert!(switcher.get_version_dir("r1").exists());
        assert!(switcher.get_version_dir("r3").exists());
    }

    #[tokio::test]
    async fn test_version_names_cannot_escape() {
        let temp = TempDir::new().unwrap();
        let switcher = ReleaseSwitcher::new(temp.path().to_path_buf(), ProjectId::new());
        for bad in ["", "..", "../x", "a/b", ".hidden", "v 1"] {
            assert!(switcher.prepare_version_dir(bad).await.is_err(), "{bad}");
        }
        assert!(ReleaseSwitcher::validate_version("20260928-051203-ab12cd3").is_ok());
        assert!(ReleaseSwitcher::validate_version("v1.2.3+build.5").is_ok());
    }
}
