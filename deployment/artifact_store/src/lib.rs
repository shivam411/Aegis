use aegis_types::{ArtifactId, ReleaseId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::fs;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredArtifact {
    pub id: ArtifactId,
    pub release_id: ReleaseId,
    pub name: String,
    pub path: PathBuf,
    pub sha256_checksum: String,
    pub size_bytes: u64,
    pub created_at: String,
}

pub struct ArtifactStore {
    base_dir: PathBuf,
}

impl ArtifactStore {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    /// Stores application build output, creating the target storage directory and metadata snapshot.
    pub async fn store(
        &self,
        release_id: &ReleaseId,
        source_path: &Path,
    ) -> Result<StoredArtifact, anyhow::Error> {
        let release_dir = self.base_dir.join("releases").join(release_id.to_string());
        fs::create_dir_all(&release_dir).await?;

        let artifact_id = ArtifactId::new();
        let artifact_name = source_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("app")
            .to_string();

        let target_path = release_dir.join(&artifact_name);
        let mut hasher = Sha256::new();
        let mut size_bytes: u64 = 0;

        if source_path.is_file() {
            let bytes = fs::read(source_path).await?;
            size_bytes = bytes.len() as u64;
            hasher.update(&bytes);
            fs::write(&target_path, bytes).await?;
        } else if source_path.is_dir() {
            fs::create_dir_all(&target_path).await?;
            hasher.update(source_path.to_string_lossy().as_bytes());
        }

        let sha256_checksum = format!("{:x}", hasher.finalize());

        let metadata = StoredArtifact {
            id: artifact_id,
            release_id: *release_id,
            name: artifact_name,
            path: target_path,
            sha256_checksum,
            size_bytes,
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        let meta_json = serde_json::to_string_pretty(&metadata)?;
        fs::write(release_dir.join("metadata.json"), meta_json).await?;

        tracing::info!(release_id = %release_id, artifact_id = %artifact_id, "Artifact stored successfully");

        Ok(metadata)
    }

    /// Returns the directory path of a stored release artifact.
    pub async fn get(&self, release_id: &ReleaseId) -> Result<PathBuf, anyhow::Error> {
        let release_dir = self.base_dir.join("releases").join(release_id.to_string());
        if !release_dir.exists() {
            anyhow::bail!("Artifact directory not found for release {}", release_id);
        }
        Ok(release_dir)
    }

    /// Verifies the presence of the release artifact directory.
    pub async fn verify(&self, release_id: &ReleaseId) -> Result<bool, anyhow::Error> {
        let release_dir = self.base_dir.join("releases").join(release_id.to_string());
        Ok(release_dir.join("metadata.json").exists())
    }

    /// Removes older artifacts beyond the retention count.
    pub async fn cleanup(&self, retain_count: usize) -> Result<usize, anyhow::Error> {
        let releases_dir = self.base_dir.join("releases");
        if !releases_dir.exists() {
            return Ok(0);
        }

        let mut entries = Vec::new();
        let mut dir = fs::read_dir(releases_dir).await?;
        while let Some(entry) = dir.next_entry().await? {
            if entry.file_type().await?.is_dir() {
                entries.push(entry.path());
            }
        }

        if entries.len() <= retain_count {
            return Ok(0);
        }

        entries.sort();
        let remove_count = entries.len() - retain_count;
        for path in entries.iter().take(remove_count) {
            let _ = fs::remove_dir_all(path).await;
        }

        Ok(remove_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_artifact_store_flow() {
        let temp = TempDir::new().unwrap();
        let store = ArtifactStore::new(temp.path().to_path_buf());
        let release_id = ReleaseId::new();

        let sample_file = temp.path().join("dist.tar.gz");
        fs::write(&sample_file, "sample binary content").await.unwrap();

        let artifact = store.store(&release_id, &sample_file).await.unwrap();
        assert_eq!(artifact.release_id, release_id);

        let exists = store.verify(&release_id).await.unwrap();
        assert!(exists);

        let path = store.get(&release_id).await.unwrap();
        assert!(path.join("metadata.json").exists());
    }

    #[tokio::test]
    async fn test_artifact_store_dir_and_cleanup() {
        let temp = TempDir::new().unwrap();
        let store = ArtifactStore::new(temp.path().to_path_buf());

        // 1. Store a directory
        let sample_dir = temp.path().join("build_dir");
        fs::create_dir_all(&sample_dir).await.unwrap();
        let rel1 = ReleaseId::new();
        let artifact = store.store(&rel1, &sample_dir).await.unwrap();
        assert_eq!(artifact.name, "build_dir");

        let rel2 = ReleaseId::new();
        store.store(&rel2, &sample_dir).await.unwrap();

        let rel3 = ReleaseId::new();
        store.store(&rel3, &sample_dir).await.unwrap();

        // 2. Test get non-existent error
        let missing_rel = ReleaseId::new();
        assert!(store.get(&missing_rel).await.is_err());
        assert!(!store.verify(&missing_rel).await.unwrap());

        // 3. Test cleanup (retain 1)
        let removed = store.cleanup(1).await.unwrap();
        assert_eq!(removed, 2);
    }
}
