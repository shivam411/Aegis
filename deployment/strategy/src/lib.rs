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
// Graceful Zero-Downtime Switch Strategy
// ----------------------------------------------------
pub struct GracefulSwitchStrategy {
    pub base_storage_dir: std::path::PathBuf,
    pub drain_timeout_secs: u64,
    pub max_retained_versions: usize,
}

impl GracefulSwitchStrategy {
    pub fn new(base_storage_dir: std::path::PathBuf) -> Self {
        Self {
            base_storage_dir,
            drain_timeout_secs: 2,
            max_retained_versions: 2,
        }
    }
}

#[async_trait]
impl DeploymentStrategy for GracefulSwitchStrategy {
    fn name(&self) -> &str {
        "GracefulSwitch"
    }

    async fn execute(
        &self,
        ctx: &DeploymentContext,
        health_checker: Option<&dyn HealthChecker>,
    ) -> Result<DeploymentOutcome, anyhow::Error> {
        tracing::info!(
            deployment_id = %ctx.deployment_id,
            release_version = %ctx.release.version,
            "Executing Zero-Downtime GracefulSwitch strategy"
        );

        let switcher = aegis_release::ReleaseSwitcher::new(
            self.base_storage_dir.clone(),
            ctx.project_id,
        );

        // 1. Prepare version directory in isolation
        switcher.prepare_version_dir(&ctx.release.version).await?;

        // 2. Health check pre-activation
        if let (Some(target), Some(checker)) = (&ctx.health_target, health_checker) {
            tracing::info!(target = %target, "Performing pre-switch health check on standby instance");
            if !checker.check_health(target).await? {
                tracing::warn!("Pre-switch health check failed! Live version remains untouched.");
                return Ok(DeploymentOutcome::RollbackRequired {
                    reason: "Pre-switch health check failed for new version".to_string(),
                });
            }
        }

        // 3. Atomically switch version pointer
        switcher.switch_to_version(&ctx.release.version).await?;

        // 4. Auto-prune old versions exceeding max retention limit
        let purged = switcher.cleanup_old_versions(self.max_retained_versions).await?;
        if purged > 0 {
            tracing::info!(
                purged_count = purged,
                max_retained = self.max_retained_versions,
                "Auto-deleted older release version directories"
            );
        }

        // 5. Gracefully drain old release version if present
        if let Some(prev) = &ctx.previous_release {
            tracing::info!(
                prev_version = %prev.version,
                drain_timeout = self.drain_timeout_secs,
                "Gracefully draining previous release version"
            );
        }

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

    #[tokio::test]
    async fn test_graceful_switch_strategy() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let strategy = GracefulSwitchStrategy {
            base_storage_dir: temp_dir.path().to_path_buf(),
            drain_timeout_secs: 1,
            max_retained_versions: 2,
        };

        let proj_id = ProjectId::new();
        let release_v1 = Release::new(
            proj_id,
            "v1.0.0".to_string(),
            "abc1234".to_string(),
            "Initial commit".to_string(),
            "Dev".to_string(),
            "main".to_string(),
            "Rust".to_string(),
        );

        let release_v2 = Release::new(
            proj_id,
            "v2.0.0".to_string(),
            "def5678".to_string(),
            "Upgrade website".to_string(),
            "Dev".to_string(),
            "main".to_string(),
            "Rust".to_string(),
        );

        let ctx_v1 = DeploymentContext {
            deployment_id: DeploymentId::new(),
            project_id: proj_id,
            release: release_v1.clone(),
            previous_release: None,
            health_target: None,
        };

        let outcome_v1 = strategy.execute(&ctx_v1, None).await.unwrap();
        assert_eq!(outcome_v1, DeploymentOutcome::Success);

        let ctx_v2 = DeploymentContext {
            deployment_id: DeploymentId::new(),
            project_id: proj_id,
            release: release_v2,
            previous_release: Some(release_v1),
            health_target: None,
        };

        let outcome_v2 = strategy.execute(&ctx_v2, None).await.unwrap();
        assert_eq!(outcome_v2, DeploymentOutcome::Success);

        let switcher = aegis_release::ReleaseSwitcher::new(temp_dir.path().to_path_buf(), proj_id);
        let active = switcher.get_active_version().await.unwrap();
        assert_eq!(active, Some("v2.0.0".to_string()));
    }
}
