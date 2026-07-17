# Plugin API Contract

This document freezes the plugin interfaces to ensure backward compatibility and extensibility.

---

## 1. Dynamic Interface Traits

Plugins interact with the system by implementing the async-safe `Plugin` trait. The registry is located in `core/plugin`.

```rust
use async_trait::async_trait;

#[async_trait]
pub trait Plugin: Send + Sync {
    /// Unique identifier for the plugin (e.g. "aegis-github-trigger").
    fn name(&self) -> &str;

    /// Lifecycle hook triggered when the daemon finishes booting.
    async fn on_init(&self, ctx: &PluginContext) -> Result<(), anyhow::Error>;

    /// Event hook triggered whenever an event is published to the Event Bus.
    async fn on_event(&self, event: &Event, ctx: &PluginContext) -> Result<(), anyhow::Error>;
}
```

### Context Structs
The `PluginContext` provides read-only access to config and allows publishing back to the Event Bus:

```rust
pub struct PluginContext {
    pub daemon_version: String,
    pub config: aegis_config::Config,
    pub event_bus_tx: tokio::sync::broadcast::Sender<Event>,
}
```

---

## 2. Core Extension Points

Subsystems defer execution mechanics to plugins implementing these specific traits.

### 2.1 Runtime Engine Lifecycle
Standardizes how applications are detected, prepared, built, and executed:

```rust
#[async_trait]
pub trait Runtime: Send + Sync {
    /// Name of the runtime engine (e.g., "node", "rust", "docker")
    fn name(&self) -> &str;

    /// Returns true if the project root matches this runtime's convention
    async fn detect(&self, path: &Path) -> bool;

    /// Configures execution environment variables and settings
    async fn prepare(&self, ctx: &RuntimeContext) -> Result<(), anyhow::Error>;

    /// Fetches package dependencies
    async fn install(&self, ctx: &RuntimeContext) -> Result<(), anyhow::Error>;

    /// Compiles code and generates files
    async fn build(&self, ctx: &RuntimeContext) -> Result<BuildOutput, anyhow::Error>;

    /// Spawns the process and monitors its process ID (PID)
    async fn start(&self, ctx: &RuntimeContext) -> Result<ProcessHandle, anyhow::Error>;

    /// Gracefully signals the process to stop
    async fn stop(&self, handle: &ProcessHandle) -> Result<(), anyhow::Error>;

    /// Queries runtime metrics or health endpoints
    async fn health(&self, handle: &ProcessHandle) -> Result<HealthStatus, anyhow::Error>;
}
```

### 2.2 Deployment Strategy
Orchestrates how releases promote and verify on hosts:

```rust
#[async_trait]
pub trait DeploymentStrategy: Send + Sync {
    /// Strategy identifier (e.g., "Immediate", "Rolling", "BlueGreen")
    fn name(&self) -> &str;

    /// Executes release promotion
    async fn execute(
        &self,
        ctx: &DeploymentContext,
    ) -> Result<DeploymentOutcome, anyhow::Error>;
}

pub struct DeploymentContext {
    pub project_id: ProjectId,
    pub target_release: Release,
    pub previous_release: Option<Release>,
    pub runtime: Box<dyn Runtime>,
}

pub enum DeploymentOutcome {
    Success,
    RollbackRequired { reason: String },
}
```

### 2.3 Build Stage
Allows plugins to insert custom middleware stages into the deployment pipeline:

```rust
#[async_trait]
pub trait BuildStage: Send + Sync {
    /// The name of the custom build pipeline step
    fn name(&self) -> &str;

    /// Executed in sequence during the build pipeline
    async fn run(&self, ctx: &BuildContext) -> Result<(), anyhow::Error>;
}
```
