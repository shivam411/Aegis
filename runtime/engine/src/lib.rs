use async_trait::async_trait;
use std::path::Path;

pub struct RuntimeContext {
    pub project_path: std::path::PathBuf,
}

pub struct ProcessHandle {
    pub pid: u32,
}

pub enum HealthStatus {
    Healthy,
    Unhealthy { reason: String },
}

#[async_trait]
pub trait Runtime: Send + Sync {
    fn name(&self) -> &str;
    async fn detect(&self, path: &Path) -> bool;
    async fn prepare(&self, ctx: &RuntimeContext) -> Result<(), anyhow::Error>;
    async fn install(&self, ctx: &RuntimeContext) -> Result<(), anyhow::Error>;
    async fn build(&self, ctx: &RuntimeContext) -> Result<(), anyhow::Error>;
    async fn start(&self, ctx: &RuntimeContext) -> Result<ProcessHandle, anyhow::Error>;
    async fn stop(&self, handle: &ProcessHandle) -> Result<(), anyhow::Error>;
    async fn health(&self, handle: &ProcessHandle) -> Result<HealthStatus, anyhow::Error>;
}
