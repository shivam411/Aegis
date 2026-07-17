# ADR 0007: Immutable Releases

## Status
Accepted

## Context
Many deployment systems treat deployment as "pull latest commit and restart." This model:
* Couples deployment to Git state, making rollback require a reverse commit or branch switch.
* Loses build metadata (what was installed, what tests ran, what environment was used).
* Forces rebuilding on every rollback, wasting time and introducing non-determinism.

## Decision
Introduce the **Release** as a first-class, immutable entity that sits between a Git commit and a Deployment.

### Model

```
Git Commit
    │
    └──→ Build Pipeline
              │
              └──→ Release (immutable)
                        │
                        └──→ Deployment (activates a release)
```

### Release Fields

```rust
pub struct Release {
    pub id: ReleaseId,
    pub project_id: ProjectId,
    pub version: String,           // e.g. "v1.4.2" or auto-generated
    pub commit_sha: String,
    pub commit_message: String,
    pub author: String,
    pub branch: String,
    pub artifacts: Vec<ArtifactId>,
    pub checksums: HashMap<String, String>,
    pub build_metadata: BuildMetadata,
    pub environment: HashMap<String, String>,
    pub created_at: DateTime<Utc>,
}

pub struct BuildMetadata {
    pub runtime: String,           // e.g. "node-20.11.0"
    pub build_command: String,
    pub build_duration_ms: u64,
    pub install_command: String,
    pub test_command: Option<String>,
    pub tests_passed: Option<bool>,
}
```

### Immutability Contract
* Once created, a Release is **never modified**.
* Rollback = create a new Deployment pointing to a previous Release.
* The Release's artifacts are stored in the Artifact Store (ADR 0008).

## Consequences
* **Instant Rollback**: Reactivate a previous release without rebuilding.
* **Auditability**: Every deployment points to an immutable, checksummed release.
* **Reproducibility**: Build metadata captures the exact conditions of the build.
* **Deployment Comparison**: Two releases can be diffed to show what changed.
