use aegis_health::HealthChecker;
use aegis_release::Release;
use aegis_types::{DeploymentId, ProjectId};
use async_trait::async_trait;

pub struct DeploymentContext {
    pub deployment_id: DeploymentId,
    pub project_id: ProjectId,
    pub release: Release,
    pub previous_release: Option<Release>,
    pub health_target: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DeploymentOutcome {
    Success,
    RollbackRequired { reason: String },
}

#[async_trait]
pub trait DeploymentStrategy: Send + Sync {
    fn name(&self) -> &str;
    async fn execute(
        &self,
        ctx: &DeploymentContext,
        health_checker: Option<&dyn HealthChecker>,
    ) -> Result<DeploymentOutcome, anyhow::Error>;
}

// ----------------------------------------------------
// Immediate Strategy (V1 Default)
// ----------------------------------------------------
pub struct ImmediateStrategy;

#[async_trait]
impl DeploymentStrategy for ImmediateStrategy {
    fn name(&self) -> &str {
        "Immediate"
    }

    async fn execute(
        &self,
        ctx: &DeploymentContext,
        health_checker: Option<&dyn HealthChecker>,
    ) -> Result<DeploymentOutcome, anyhow::Error> {
        tracing::info!(
            deployment_id = %ctx.deployment_id,
            release_id = %ctx.release.id,
            "Executing Immediate deployment strategy"
        );

        if let (Some(target), Some(checker)) = (&ctx.health_target, health_checker) {
            tracing::info!(target = %target, "Verifying deployment health");
            if !checker.check_health(target).await? {
                return Ok(DeploymentOutcome::RollbackRequired {
                    reason: "Health check failed after activation".to_string(),
                });
            }
        }

        Ok(DeploymentOutcome::Success)
    }
}

// ----------------------------------------------------
// Rolling Strategy (V2)
// ----------------------------------------------------
pub struct RollingStrategy {
    pub batch_size: usize,
}

#[async_trait]
impl DeploymentStrategy for RollingStrategy {
    fn name(&self) -> &str {
        "Rolling"
    }

    async fn execute(
        &self,
        ctx: &DeploymentContext,
        _health_checker: Option<&dyn HealthChecker>,
    ) -> Result<DeploymentOutcome, anyhow::Error> {
        tracing::info!(
            deployment_id = %ctx.deployment_id,
            batch_size = self.batch_size,
            "Executing Rolling deployment strategy"
        );
        Ok(DeploymentOutcome::Success)
    }
}

// ----------------------------------------------------
// Blue/Green Strategy (V2)
// ----------------------------------------------------
pub struct BlueGreenStrategy;

#[async_trait]
impl DeploymentStrategy for BlueGreenStrategy {
    fn name(&self) -> &str {
        "BlueGreen"
    }

    async fn execute(
        &self,
        ctx: &DeploymentContext,
        _health_checker: Option<&dyn HealthChecker>,
    ) -> Result<DeploymentOutcome, anyhow::Error> {
        tracing::info!(
            deployment_id = %ctx.deployment_id,
            "Executing Blue/Green deployment strategy"
        );
        Ok(DeploymentOutcome::Success)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_immediate_strategy() {
        let strategy = ImmediateStrategy;
        let proj_id = ProjectId::new();
        let release = Release::new(
            proj_id,
            "v1.0.0".to_string(),
            "abc1234".to_string(),
            "Initial commit".to_string(),
            "Dev".to_string(),
            "main".to_string(),
            "Rust".to_string(),
        );

        let ctx = DeploymentContext {
            deployment_id: DeploymentId::new(),
            project_id: proj_id,
            release,
            previous_release: None,
            health_target: None,
        };

        let outcome = strategy.execute(&ctx, None).await.unwrap();
        assert_eq!(outcome, DeploymentOutcome::Success);
    }
}
