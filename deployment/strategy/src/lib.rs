use async_trait::async_trait;
use aegis_types::{ProjectId, ReleaseId};

pub struct DeploymentContext {
    pub project_id: ProjectId,
    pub release_id: ReleaseId,
}

pub enum DeploymentOutcome {
    Success,
    RollbackRequired { reason: String },
}

#[async_trait]
pub trait DeploymentStrategy: Send + Sync {
    fn name(&self) -> &str;
    async fn execute(&self, ctx: &DeploymentContext) -> Result<DeploymentOutcome, anyhow::Error>;
}

pub struct ImmediateStrategy;

#[async_trait]
impl DeploymentStrategy for ImmediateStrategy {
    fn name(&self) -> &str {
        "Immediate"
    }

    async fn execute(&self, _ctx: &DeploymentContext) -> Result<DeploymentOutcome, anyhow::Error> {
        Ok(DeploymentOutcome::Success)
    }
}
