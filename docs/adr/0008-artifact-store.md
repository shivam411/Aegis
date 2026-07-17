# ADR 0008: Artifact Store

## Status
Accepted

## Context
Without an artifact store, every deployment and rollback requires a full rebuild from source. This is slow, non-deterministic (dependency versions may change), and wastes compute resources.

## Decision
Introduce an **Artifact Store** (`deployment/artifact_store`) that manages immutable build outputs on disk.

### Architecture

```
Build Pipeline
    │
    └──→ Package Stage
              │
              └──→ Artifact Store
                        │
                        ├── /artifacts/{release_id}/app.tar.gz
                        ├── /artifacts/{release_id}/checksums.sha256
                        └── /artifacts/{release_id}/metadata.json
```

### Interface

```rust
pub struct ArtifactStore { ... }

impl ArtifactStore {
    /// Store a build output, returning its artifact ID and checksum.
    pub async fn store(
        &self,
        release_id: &ReleaseId,
        source_path: &Path,
    ) -> Result<Artifact, anyhow::Error>;

    /// Retrieve the path to a stored artifact.
    pub async fn get(
        &self,
        release_id: &ReleaseId,
    ) -> Result<PathBuf, anyhow::Error>;

    /// Verify artifact integrity against stored checksums.
    pub async fn verify(
        &self,
        release_id: &ReleaseId,
    ) -> Result<bool, anyhow::Error>;

    /// Remove artifacts older than the retention policy.
    pub async fn cleanup(
        &self,
        retain_count: usize,
    ) -> Result<usize, anyhow::Error>;
}
```

### Storage Layout

```
~/.aegis/artifacts/
    └── {release_id}/
            ├── app.tar.gz          # Packaged application
            ├── checksums.sha256    # SHA-256 checksums
            └── metadata.json       # Build metadata snapshot
```

## Consequences
* **Instant Rollback**: Activate a previous release by extracting its stored artifact. No rebuild.
* **Integrity Verification**: Checksums detect corrupted or tampered artifacts.
* **Disk Management**: Configurable retention policy prevents unbounded growth.
* **Deployment Speed**: Subsequent deployments of the same release skip the build entirely.
