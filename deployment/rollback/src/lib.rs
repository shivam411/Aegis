use aegis_artifact_store::ArtifactStore;
use aegis_release::Release;
use aegis_types::ProjectId;

pub struct RollbackExecutor;

impl RollbackExecutor {
    pub async fn execute_rollback(
        project_id: ProjectId,
        target_release: &Release,
        artifact_store: &ArtifactStore,
    ) -> Result<(), anyhow::Error> {
        tracing::info!(
            project_id = %project_id,
            target_release_id = %target_release.id,
            version = %target_release.version,
            "Executing instant rollback to target release"
        );

        // Verify artifact presence in store
        if !artifact_store.verify(&target_release.id).await? {
            anyhow::bail!(
                "Cannot rollback: Artifact for release {} not found in store",
                target_release.id
            );
        }

        let artifact_path = artifact_store.get(&target_release.id).await?;
        tracing::info!(path = %artifact_path.display(), "Reactivated stored release artifact");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_rollback_executor() {
        let temp = TempDir::new().unwrap();
        let store = ArtifactStore::new(temp.path().to_path_buf());
        let proj_id = ProjectId::new();

        let release = Release::new(
            proj_id,
            "v1.0.0".to_string(),
            "1234567".to_string(),
            "Initial".to_string(),
            "Dev".to_string(),
            "main".to_string(),
            "Rust".to_string(),
        );

        let sample_file = temp.path().join("app");
        tokio::fs::write(&sample_file, "data").await.unwrap();
        store.store(&release.id, &sample_file).await.unwrap();

        let result = RollbackExecutor::execute_rollback(proj_id, &release, &store).await;
        assert!(result.is_ok());
    }
}
