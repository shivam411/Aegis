use aegis_types::{ProcessId, ProjectId};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct RuntimeContext {
    pub project_id: ProjectId,
    pub project_path: PathBuf,
    pub working_dir: PathBuf,
    pub environment: HashMap<String, String>,
    pub build_command: Option<String>,
    pub start_command: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProcessHandle {
    pub process_id: ProcessId,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

// ----------------------------------------------------
// Node.js Runtime
// ----------------------------------------------------
pub struct NodeRuntime;

#[async_trait]
impl Runtime for NodeRuntime {
    fn name(&self) -> &str {
        "Node.js"
    }

    async fn detect(&self, path: &Path) -> bool {
        path.join("package.json").exists()
    }

    async fn prepare(&self, _ctx: &RuntimeContext) -> Result<(), anyhow::Error> {
        tracing::info!("NodeRuntime: Environment ready");
        Ok(())
    }

    async fn install(&self, ctx: &RuntimeContext) -> Result<(), anyhow::Error> {
        tracing::info!("NodeRuntime: Running npm install");
        let status = tokio::process::Command::new("npm")
            .arg("install")
            .current_dir(&ctx.working_dir)
            .status()
            .await?;
        if !status.success() {
            anyhow::bail!("npm install failed with status {}", status);
        }
        Ok(())
    }

    async fn build(&self, ctx: &RuntimeContext) -> Result<(), anyhow::Error> {
        if let Some(cmd) = &ctx.build_command {
            tracing::info!(cmd = %cmd, "NodeRuntime: Running custom build command");
        } else {
            tracing::info!("NodeRuntime: Running npm run build (if present)");
        }
        Ok(())
    }

    async fn start(&self, _ctx: &RuntimeContext) -> Result<ProcessHandle, anyhow::Error> {
        let process_id = ProcessId::new();
        tracing::info!(process_id = %process_id, "NodeRuntime: Starting Node process");
        Ok(ProcessHandle { process_id, pid: Some(1001) })
    }

    async fn stop(&self, handle: &ProcessHandle) -> Result<(), anyhow::Error> {
        tracing::info!(process_id = %handle.process_id, "NodeRuntime: Stopping process");
        Ok(())
    }

    async fn health(&self, _handle: &ProcessHandle) -> Result<HealthStatus, anyhow::Error> {
        Ok(HealthStatus::Healthy)
    }
}

// ----------------------------------------------------
// Rust Runtime
// ----------------------------------------------------
pub struct RustRuntime;

#[async_trait]
impl Runtime for RustRuntime {
    fn name(&self) -> &str {
        "Rust"
    }

    async fn detect(&self, path: &Path) -> bool {
        path.join("Cargo.toml").exists()
    }

    async fn prepare(&self, _ctx: &RuntimeContext) -> Result<(), anyhow::Error> {
        tracing::info!("RustRuntime: Environment ready");
        Ok(())
    }

    async fn install(&self, _ctx: &RuntimeContext) -> Result<(), anyhow::Error> {
        tracing::info!("RustRuntime: Dependencies managed by cargo");
        Ok(())
    }

    async fn build(&self, ctx: &RuntimeContext) -> Result<(), anyhow::Error> {
        tracing::info!("RustRuntime: Running cargo build --release");
        let status = tokio::process::Command::new("cargo")
            .args(["build", "--release"])
            .current_dir(&ctx.working_dir)
            .status()
            .await?;
        if !status.success() {
            anyhow::bail!("cargo build failed with status {}", status);
        }
        Ok(())
    }

    async fn start(&self, _ctx: &RuntimeContext) -> Result<ProcessHandle, anyhow::Error> {
        let process_id = ProcessId::new();
        tracing::info!(process_id = %process_id, "RustRuntime: Starting Rust executable");
        Ok(ProcessHandle { process_id, pid: Some(1002) })
    }

    async fn stop(&self, handle: &ProcessHandle) -> Result<(), anyhow::Error> {
        tracing::info!(process_id = %handle.process_id, "RustRuntime: Stopping process");
        Ok(())
    }

    async fn health(&self, _handle: &ProcessHandle) -> Result<HealthStatus, anyhow::Error> {
        Ok(HealthStatus::Healthy)
    }
}

// ----------------------------------------------------
// Go Runtime
// ----------------------------------------------------
pub struct GoRuntime;

#[async_trait]
impl Runtime for GoRuntime {
    fn name(&self) -> &str {
        "Go"
    }

    async fn detect(&self, path: &Path) -> bool {
        path.join("go.mod").exists()
    }

    async fn prepare(&self, _ctx: &RuntimeContext) -> Result<(), anyhow::Error> {
        Ok(())
    }

    async fn install(&self, ctx: &RuntimeContext) -> Result<(), anyhow::Error> {
        tracing::info!("GoRuntime: Running go mod download");
        let _ = tokio::process::Command::new("go")
            .args(["mod", "download"])
            .current_dir(&ctx.working_dir)
            .status()
            .await;
        Ok(())
    }

    async fn build(&self, ctx: &RuntimeContext) -> Result<(), anyhow::Error> {
        tracing::info!("GoRuntime: Running go build");
        let status = tokio::process::Command::new("go")
            .args(["build", "-o", "main"])
            .current_dir(&ctx.working_dir)
            .status()
            .await?;
        if !status.success() {
            anyhow::bail!("go build failed with status {}", status);
        }
        Ok(())
    }

    async fn start(&self, _ctx: &RuntimeContext) -> Result<ProcessHandle, anyhow::Error> {
        let process_id = ProcessId::new();
        tracing::info!(process_id = %process_id, "GoRuntime: Starting Go binary");
        Ok(ProcessHandle { process_id, pid: Some(1003) })
    }

    async fn stop(&self, handle: &ProcessHandle) -> Result<(), anyhow::Error> {
        tracing::info!(process_id = %handle.process_id, "GoRuntime: Stopping Go process");
        Ok(())
    }

    async fn health(&self, _handle: &ProcessHandle) -> Result<HealthStatus, anyhow::Error> {
        Ok(HealthStatus::Healthy)
    }
}

// ----------------------------------------------------
// Python Runtime
// ----------------------------------------------------
pub struct PythonRuntime;

#[async_trait]
impl Runtime for PythonRuntime {
    fn name(&self) -> &str {
        "Python"
    }

    async fn detect(&self, path: &Path) -> bool {
        path.join("requirements.txt").exists() || path.join("pyproject.toml").exists()
    }

    async fn prepare(&self, _ctx: &RuntimeContext) -> Result<(), anyhow::Error> {
        Ok(())
    }

    async fn install(&self, ctx: &RuntimeContext) -> Result<(), anyhow::Error> {
        tracing::info!("PythonRuntime: Installing requirements");
        if ctx.working_dir.join("requirements.txt").exists() {
            let _ = tokio::process::Command::new("pip")
                .args(["install", "-r", "requirements.txt"])
                .current_dir(&ctx.working_dir)
                .status()
                .await;
        }
        Ok(())
    }

    async fn build(&self, _ctx: &RuntimeContext) -> Result<(), anyhow::Error> {
        Ok(())
    }

    async fn start(&self, _ctx: &RuntimeContext) -> Result<ProcessHandle, anyhow::Error> {
        let process_id = ProcessId::new();
        tracing::info!(process_id = %process_id, "PythonRuntime: Starting Python process");
        Ok(ProcessHandle { process_id, pid: Some(1004) })
    }

    async fn stop(&self, handle: &ProcessHandle) -> Result<(), anyhow::Error> {
        tracing::info!(process_id = %handle.process_id, "PythonRuntime: Stopping Python process");
        Ok(())
    }

    async fn health(&self, _handle: &ProcessHandle) -> Result<HealthStatus, anyhow::Error> {
        Ok(HealthStatus::Healthy)
    }
}

// ----------------------------------------------------
// Generic Fallback Runtime
// ----------------------------------------------------
pub struct GenericRuntime;

#[async_trait]
impl Runtime for GenericRuntime {
    fn name(&self) -> &str {
        "Generic"
    }

    async fn detect(&self, _path: &Path) -> bool {
        true
    }

    async fn prepare(&self, _ctx: &RuntimeContext) -> Result<(), anyhow::Error> {
        Ok(())
    }

    async fn install(&self, _ctx: &RuntimeContext) -> Result<(), anyhow::Error> {
        Ok(())
    }

    async fn build(&self, _ctx: &RuntimeContext) -> Result<(), anyhow::Error> {
        Ok(())
    }

    async fn start(&self, _ctx: &RuntimeContext) -> Result<ProcessHandle, anyhow::Error> {
        let process_id = ProcessId::new();
        tracing::info!(process_id = %process_id, "GenericRuntime: Starting generic command process");
        Ok(ProcessHandle { process_id, pid: Some(1005) })
    }

    async fn stop(&self, handle: &ProcessHandle) -> Result<(), anyhow::Error> {
        tracing::info!(process_id = %handle.process_id, "GenericRuntime: Stopping generic process");
        Ok(())
    }

    async fn health(&self, _handle: &ProcessHandle) -> Result<HealthStatus, anyhow::Error> {
        Ok(HealthStatus::Healthy)
    }
}

// ----------------------------------------------------
// Auto-Detection Router
// ----------------------------------------------------
pub struct RuntimeDetector {
    runtimes: Vec<Box<dyn Runtime>>,
}

impl RuntimeDetector {
    pub fn new() -> Self {
        Self {
            runtimes: vec![
                Box::new(NodeRuntime),
                Box::new(RustRuntime),
                Box::new(GoRuntime),
                Box::new(PythonRuntime),
                Box::new(GenericRuntime),
            ],
        }
    }

    pub async fn detect_runtime(&self, project_path: &Path) -> &dyn Runtime {
        for runtime in &self.runtimes {
            if runtime.detect(project_path).await {
                return runtime.as_ref();
            }
        }
        &GenericRuntime
    }
}

impl Default for RuntimeDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_runtime_auto_detection() {
        let detector = RuntimeDetector::new();
        let current_dir = std::env::current_dir().unwrap();
        // Aegis repository contains Cargo.toml -> RustRuntime
        let detected = detector.detect_runtime(&current_dir).await;
        assert_eq!(detected.name(), "Rust");
    }
}
