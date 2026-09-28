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

/// Content fingerprint of a file or directory tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    pub sha256: String,
    pub size_bytes: u64,
    pub file_count: u64,
}

/// Hashes relative paths and file contents in a stable (sorted) order, so two
/// trees with identical contents always produce the same checksum.
/// Symlinks are hashed by their target path rather than followed.
pub fn fingerprint_path(path: &Path) -> Result<Fingerprint, anyhow::Error> {
    use std::io::Read;

    let mut hasher = Sha256::new();
    let mut size_bytes = 0u64;
    let mut file_count = 0u64;
    let mut stack = vec![PathBuf::new()];
    let root_meta = std::fs::symlink_metadata(path)?;
    if root_meta.is_file() {
        stack.clear();
        let mut file = std::fs::File::open(path)?;
        size_bytes = std::io::copy(&mut file, &mut HashWriter(&mut hasher))?;
        file_count = 1;
    }

    while let Some(rel) = stack.pop() {
        let mut entries: Vec<_> =
            std::fs::read_dir(path.join(&rel))?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        // Reverse so the stack pops directories in sorted order.
        for entry in entries.into_iter().rev() {
            let rel_path = rel.join(entry.file_name());
            let file_type = entry.file_type()?;
            let rel_str = rel_path.to_string_lossy().replace('\\', "/");
            if file_type.is_symlink() {
                let target = std::fs::read_link(entry.path())?;
                hasher.update(b"L");
                hasher.update(rel_str.as_bytes());
                hasher.update([0]);
                hasher.update(target.to_string_lossy().as_bytes());
                hasher.update([0]);
            } else if file_type.is_dir() {
                hasher.update(b"D");
                hasher.update(rel_str.as_bytes());
                hasher.update([0]);
                stack.push(rel_path);
            } else if file_type.is_file() {
                hasher.update(b"F");
                hasher.update(rel_str.as_bytes());
                hasher.update([0]);
                let mut file = std::fs::File::open(entry.path())?;
                let mut buf = [0u8; 64 * 1024];
                loop {
                    let n = file.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    hasher.update(&buf[..n]);
                    size_bytes += n as u64;
                }
                hasher.update([0]);
                file_count += 1;
            }
        }
    }

    Ok(Fingerprint {
        sha256: format!("{:x}", hasher.finalize()),
        size_bytes,
        file_count,
    })
}

struct HashWriter<'a>(&'a mut Sha256);

impl std::io::Write for HashWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if file_type.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(entry.path())?, &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

impl ArtifactStore {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    fn release_dir(&self, release_id: &ReleaseId) -> PathBuf {
        self.base_dir.join("releases").join(release_id.to_string())
    }

    async fn write_metadata(
        &self,
        release_id: &ReleaseId,
        metadata: &StoredArtifact,
    ) -> Result<(), anyhow::Error> {
        let release_dir = self.release_dir(release_id);
        fs::create_dir_all(&release_dir).await?;
        fs::write(
            release_dir.join("metadata.json"),
            serde_json::to_string_pretty(metadata)?,
        )
        .await?;
        Ok(())
    }

    /// Copies build output (a file or a directory tree) into the store and
    /// records its content checksum.
    pub async fn store(
        &self,
        release_id: &ReleaseId,
        source_path: &Path,
    ) -> Result<StoredArtifact, anyhow::Error> {
        let artifact_name = source_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("app")
            .to_string();
        let target_path = self.release_dir(release_id).join(&artifact_name);
        fs::create_dir_all(self.release_dir(release_id)).await?;

        let (src, dst) = (source_path.to_path_buf(), target_path.clone());
        let fingerprint = tokio::task::spawn_blocking(move || -> Result<_, anyhow::Error> {
            if src.is_dir() {
                copy_tree(&src, &dst)?;
            } else {
                std::fs::copy(&src, &dst)?;
            }
            fingerprint_path(&dst)
        })
        .await??;

        let metadata = StoredArtifact {
            id: ArtifactId::new(),
            release_id: *release_id,
            name: artifact_name,
            path: target_path,
            sha256_checksum: fingerprint.sha256,
            size_bytes: fingerprint.size_bytes,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        self.write_metadata(release_id, &metadata).await?;

        tracing::info!(release_id = %release_id, artifact_id = %metadata.id, "Artifact stored successfully");
        Ok(metadata)
    }

    /// Records an artifact that already lives at its final location (such as
    /// a release directory the app runs from) without copying it.
    pub async fn register(
        &self,
        release_id: &ReleaseId,
        path: &Path,
    ) -> Result<StoredArtifact, anyhow::Error> {
        let owned = path.to_path_buf();
        let fingerprint = tokio::task::spawn_blocking(move || fingerprint_path(&owned)).await??;
        let metadata = StoredArtifact {
            id: ArtifactId::new(),
            release_id: *release_id,
            name: path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("app")
                .to_string(),
            path: path.to_path_buf(),
            sha256_checksum: fingerprint.sha256,
            size_bytes: fingerprint.size_bytes,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        self.write_metadata(release_id, &metadata).await?;
        Ok(metadata)
    }

    /// Reads the recorded metadata for a release.
    pub async fn metadata(&self, release_id: &ReleaseId) -> Result<StoredArtifact, anyhow::Error> {
        let content =
            fs::read_to_string(self.release_dir(release_id).join("metadata.json")).await?;
        Ok(serde_json::from_str(&content)?)
    }

    /// Returns the directory path of a stored release artifact.
    pub async fn get(&self, release_id: &ReleaseId) -> Result<PathBuf, anyhow::Error> {
        let release_dir = self.base_dir.join("releases").join(release_id.to_string());
        if !release_dir.exists() {
            anyhow::bail!("Artifact directory not found for release {}", release_id);
        }
        Ok(release_dir)
    }

    /// Verifies that the artifact is recorded and still present on disk.
    pub async fn verify(&self, release_id: &ReleaseId) -> Result<bool, anyhow::Error> {
        match self.metadata(release_id).await {
            Ok(meta) => Ok(meta.path.exists()),
            Err(_) => Ok(false),
        }
    }

    /// Recomputes the checksum and compares it with the recorded one.
    pub async fn verify_checksum(&self, release_id: &ReleaseId) -> Result<bool, anyhow::Error> {
        let meta = match self.metadata(release_id).await {
            Ok(meta) => meta,
            Err(_) => return Ok(false),
        };
        if !meta.path.exists() {
            return Ok(false);
        }
        let path = meta.path.clone();
        let fingerprint = tokio::task::spawn_blocking(move || fingerprint_path(&path)).await??;
        Ok(fingerprint.sha256 == meta.sha256_checksum)
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
        fs::write(&sample_file, "sample binary content")
            .await
            .unwrap();

        let artifact = store.store(&release_id, &sample_file).await.unwrap();
        assert_eq!(artifact.release_id, release_id);
        assert_eq!(artifact.size_bytes, 21);
        assert!(store.verify_checksum(&release_id).await.unwrap());

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

        // Directories are copied, and the checksum covers their contents.
        let tree = temp.path().join("tree");
        fs::create_dir_all(tree.join("sub")).await.unwrap();
        fs::write(tree.join("a.txt"), "a").await.unwrap();
        fs::write(tree.join("sub/b.txt"), "b").await.unwrap();
        let rel_tree = ReleaseId::new();
        let stored = store.store(&rel_tree, &tree).await.unwrap();
        assert!(stored.path.join("sub/b.txt").exists());
        assert_eq!(stored.size_bytes, 2);
        assert_eq!(
            stored.sha256_checksum,
            fingerprint_path(&tree).unwrap().sha256
        );
        fs::write(stored.path.join("sub/b.txt"), "tampered")
            .await
            .unwrap();
        assert!(!store.verify_checksum(&rel_tree).await.unwrap());
        fs::remove_dir_all(store.get(&rel_tree).await.unwrap())
            .await
            .unwrap();

        // 2. Test get non-existent error
        let missing_rel = ReleaseId::new();
        assert!(store.get(&missing_rel).await.is_err());
        assert!(!store.verify(&missing_rel).await.unwrap());

        // 3. Test cleanup (retain 1)
        let removed = store.cleanup(1).await.unwrap();
        assert_eq!(removed, 2);
    }

    #[tokio::test]
    async fn test_register_in_place_and_fingerprint_stability() {
        let temp = TempDir::new().unwrap();
        let store = ArtifactStore::new(temp.path().join("store"));
        let app = temp.path().join("app");
        fs::create_dir_all(&app).await.unwrap();
        fs::write(app.join("index.js"), "console.log(1)")
            .await
            .unwrap();

        let rel = ReleaseId::new();
        let meta = store.register(&rel, &app).await.unwrap();
        assert_eq!(meta.path, app);
        assert_eq!(meta.size_bytes, 14);
        assert!(store.verify(&rel).await.unwrap());
        assert_eq!(fingerprint_path(&app).unwrap().file_count, 1);

        fs::write(app.join("index.js"), "console.log(2)")
            .await
            .unwrap();
        assert_ne!(meta.sha256_checksum, fingerprint_path(&app).unwrap().sha256);
    }
}
