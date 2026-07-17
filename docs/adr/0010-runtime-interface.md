# ADR 0010: Runtime Interface

## Status
Accepted

## Context
Aegis must support Node.js, Python, Go, Rust, Java, Bun, and Deno without hardcoding language-specific logic into the core. PM2 solves this by being Node-centric with generic process spawning. We can do better by defining a structured lifecycle that each runtime implements.

## Decision
Define a `Runtime` trait in `runtime/engine` that standardizes the process lifecycle across all supported languages.

### Interface

```rust
#[async_trait]
pub trait Runtime: Send + Sync {
    fn name(&self) -> &str;

    /// Detect if this runtime can handle the given project directory.
    async fn detect(&self, project_path: &Path) -> bool;

    /// Set up the runtime environment (e.g. install Node version via nvm).
    async fn prepare(&self, ctx: &RuntimeContext) -> Result<(), anyhow::Error>;

    /// Install dependencies (e.g. npm install, pip install, go mod download).
    async fn install(&self, ctx: &RuntimeContext) -> Result<(), anyhow::Error>;

    /// Compile or bundle the application (e.g. npm run build, cargo build --release).
    async fn build(&self, ctx: &RuntimeContext) -> Result<BuildOutput, anyhow::Error>;

    /// Start the application process.
    async fn start(&self, ctx: &RuntimeContext) -> Result<ProcessHandle, anyhow::Error>;

    /// Gracefully stop the application process.
    async fn stop(&self, handle: &ProcessHandle) -> Result<(), anyhow::Error>;

    /// Check if the process is healthy and responsive.
    async fn health(&self, handle: &ProcessHandle) -> Result<HealthStatus, anyhow::Error>;
}

pub struct RuntimeContext {
    pub project_id: ProjectId,
    pub project_path: PathBuf,
    pub working_dir: PathBuf,
    pub environment: HashMap<String, String>,
    pub build_command: Option<String>,
    pub start_command: String,
}
```

### Auto-Detection Order
When `aegis init` is run, runtimes are queried in order:
1. Check for `package.json` → Node / Bun
2. Check for `Cargo.toml` → Rust
3. Check for `go.mod` → Go
4. Check for `requirements.txt` / `pyproject.toml` → Python
5. Check for `pom.xml` / `build.gradle` → Java
6. Fallback → Generic process (just a start command)

### Relationship to Build Pipeline
The Runtime's `prepare()`, `install()`, and `build()` methods map directly to Build Pipeline stages (see domain model). The pipeline orchestrator calls them in sequence, emitting stage events through the Event Bus.

## Consequences
* **Language Agnostic**: Adding Deno support means implementing one trait, not modifying the deployment engine.
* **Composable**: A Docker runtime can wrap any other runtime, using `build()` to create an image.
* **Testable**: Each runtime can be tested independently against fixture project directories.
* **Convention over Configuration**: Auto-detection eliminates the need for users to specify their runtime manually.
