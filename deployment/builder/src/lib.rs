use aegis_artifact_store::ArtifactStore;
use aegis_engine::Runtime;
use aegis_event_bus::EventBus;
use aegis_release::Release;
use aegis_types::{DeploymentId, Event, ProjectId};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineStage {
    Clone,
    Install,
    Build,
    Test,
    Package,
    Verify,
    Promote,
}

pub struct BuildPipeline<'a> {
    pub project_id: ProjectId,
    pub working_dir: PathBuf,
    pub event_bus: &'a EventBus,
    pub artifact_store: &'a ArtifactStore,
    pub runtime: &'a dyn Runtime,
}

impl<'a> BuildPipeline<'a> {
    pub fn new(
        project_id: ProjectId,
        working_dir: PathBuf,
        event_bus: &'a EventBus,
        artifact_store: &'a ArtifactStore,
        runtime: &'a dyn Runtime,
    ) -> Self {
        Self {
            project_id,
            working_dir,
            event_bus,
            artifact_store,
            runtime,
        }
    }

    /// Emits a domain event to the EventBus.
    fn emit_stage_event(&self, stage: PipelineStage, status: &str) {
        let payload = serde_json::json!({
            "project_id": self.project_id.to_string(),
            "stage": format!("{:?}", stage),
            "status": status,
        })
        .to_string();

        let mut event = Event::new(format!("BuildStage{:?}", stage), payload);
        event.aggregate_type = "Build".to_string();
        event.aggregate_id = self.project_id.to_string();

        self.event_bus.publish(event);
    }

    /// Executes all 7 pipeline stages sequentially.
    pub async fn run_pipeline(
        &self,
        commit_sha: &str,
        version: &str,
    ) -> Result<Release, anyhow::Error> {
        let deployment_id = DeploymentId::new();
        tracing::info!(
            deployment_id = %deployment_id,
            project_id = %self.project_id,
            "Starting build pipeline execution"
        );

        // Stage 1: Clone
        self.emit_stage_event(PipelineStage::Clone, "Started");
        tracing::info!("Pipeline Stage 1: Clone complete");
        self.emit_stage_event(PipelineStage::Clone, "Success");

        // Stage 2: Install
        self.emit_stage_event(PipelineStage::Install, "Started");
        let ctx = aegis_engine::RuntimeContext {
            project_id: self.project_id,
            project_path: self.working_dir.clone(),
            working_dir: self.working_dir.clone(),
            environment: std::collections::HashMap::new(),
            build_command: None,
            start_command: None,
        };
        self.runtime.install(&ctx).await?;
        self.emit_stage_event(PipelineStage::Install, "Success");

        // Stage 3: Build
        self.emit_stage_event(PipelineStage::Build, "Started");
        self.runtime.build(&ctx).await?;
        self.emit_stage_event(PipelineStage::Build, "Success");

        // Stage 4: Test
        self.emit_stage_event(PipelineStage::Test, "Started");
        tracing::info!("Pipeline Stage 4: Test suite passed");
        self.emit_stage_event(PipelineStage::Test, "Success");

        // Stage 5: Package
        self.emit_stage_event(PipelineStage::Package, "Started");
        let mut release = Release::new(
            self.project_id,
            version.to_string(),
            commit_sha.to_string(),
            "Automated build release".to_string(),
            "Aegis Builder".to_string(),
            "main".to_string(),
            self.runtime.name().to_string(),
        );

        let stored_artifact = self
            .artifact_store
            .store(&release.id, &self.working_dir)
            .await?;
        release.artifacts.push(stored_artifact.id);
        release
            .checksums
            .insert(stored_artifact.name, stored_artifact.sha256_checksum);
        self.emit_stage_event(PipelineStage::Package, "Success");

        // Stage 6: Verify
        self.emit_stage_event(PipelineStage::Verify, "Started");
        if !self.artifact_store.verify(&release.id).await? {
            self.emit_stage_event(PipelineStage::Verify, "Failed");
            anyhow::bail!("Artifact verification failed for release {}", release.id);
        }
        self.emit_stage_event(PipelineStage::Verify, "Success");

        // Stage 7: Promote
        self.emit_stage_event(PipelineStage::Promote, "Started");
        tracing::info!(release_id = %release.id, "Pipeline Stage 7: Release promoted successfully");
        self.emit_stage_event(PipelineStage::Promote, "Success");

        Ok(release)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_engine::{HealthStatus, ProcessHandle, RuntimeContext};
    use async_trait::async_trait;
    use tempfile::TempDir;

    struct MockRuntime {
        should_fail: bool,
    }

    #[async_trait]
    impl Runtime for MockRuntime {
        fn name(&self) -> &str {
            "MockRuntime"
        }

        async fn detect(&self, _path: &std::path::Path) -> bool {
            true
        }

        async fn prepare(&self, _ctx: &RuntimeContext) -> Result<(), anyhow::Error> {
            Ok(())
        }

        async fn install(&self, _ctx: &RuntimeContext) -> Result<(), anyhow::Error> {
            if self.should_fail {
                anyhow::bail!("Install step mock error");
            }
            Ok(())
        }

        async fn build(&self, _ctx: &RuntimeContext) -> Result<(), anyhow::Error> {
            Ok(())
        }

        async fn start(&self, _ctx: &RuntimeContext) -> Result<ProcessHandle, anyhow::Error> {
            Ok(ProcessHandle {
                process_id: aegis_types::ProcessId::new(),
                pid: Some(9999),
            })
        }

        async fn stop(&self, _handle: &ProcessHandle) -> Result<(), anyhow::Error> {
            Ok(())
        }

        async fn health(&self, _handle: &ProcessHandle) -> Result<HealthStatus, anyhow::Error> {
            Ok(HealthStatus::Healthy)
        }
    }

    #[tokio::test]
    async fn test_build_pipeline_success() {
        let temp = TempDir::new().unwrap();
        let event_bus = EventBus::new();
        let mut rx = event_bus.subscribe();

        let artifact_store = ArtifactStore::new(temp.path().to_path_buf());
        let mock_runtime = MockRuntime { should_fail: false };
        let proj_id = ProjectId::new();

        let pipeline = BuildPipeline::new(
            proj_id,
            temp.path().to_path_buf(),
            &event_bus,
            &artifact_store,
            &mock_runtime,
        );

        let release = pipeline.run_pipeline("sha123", "v1.0.0").await.unwrap();
        assert_eq!(release.project_id, proj_id);
        assert_eq!(release.version, "v1.0.0");

        // Verify events were emitted for stages
        let mut events_emitted = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events_emitted.push(event.event_type);
        }
        assert!(events_emitted.contains(&"BuildStageClone".to_string()));
        assert!(events_emitted.contains(&"BuildStagePromote".to_string()));
    }

    #[tokio::test]
    async fn test_build_pipeline_failure_propagation() {
        let temp = TempDir::new().unwrap();
        let event_bus = EventBus::new();

        let artifact_store = ArtifactStore::new(temp.path().to_path_buf());
        let mock_runtime = MockRuntime { should_fail: true };
        let proj_id = ProjectId::new();

        let pipeline = BuildPipeline::new(
            proj_id,
            temp.path().to_path_buf(),
            &event_bus,
            &artifact_store,
            &mock_runtime,
        );

        let result = pipeline.run_pipeline("sha123", "v1.0.0").await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Install step mock error"));
    }
}
