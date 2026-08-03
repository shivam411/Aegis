# Aegis Typed Error Model Specification

This specification defines `AegisError`, the standard error taxonomy for all Aegis components, replacing un-typed `anyhow::Error` in public interfaces.

---

## 1. Domain Error Taxonomy (`AegisError`)

```rust
#[derive(Debug, thiserror::Error)]
pub enum AegisError {
    #[error("Configuration Error: {reason}")]
    Config { reason: String },

    #[error("Project Error [{id}]: {message}")]
    Project { id: String, message: String },

    #[error("Deployment Error [{id}]: {stage} failed - {reason}")]
    Deployment { id: String, stage: String, reason: String },

    #[error("Runtime Error [{runtime}]: {message}")]
    Runtime { runtime: String, message: String },

    #[error("Build Pipeline Error: stage {stage} failed with code {exit_code:?}")]
    Build { stage: String, exit_code: Option<i32> },

    #[error("EventStore Persistence Error: {0}")]
    EventStore(String),

    #[error("Artifact Store Error: {0}")]
    ArtifactStore(String),

    #[error("Health Check Failed for target '{target}': {reason}")]
    HealthCheck { target: String, reason: String },

    #[error("Plugin Error [{plugin_name}]: {message}")]
    Plugin { plugin_name: String, message: String },

    #[error("gRPC Network Error: {0}")]
    Network(String),

    #[error("Validation Error: {0}")]
    Validation(String),

    #[error("Internal System Error: {0}")]
    Internal(String),
}
```

---

## 2. Error Code & gRPC Mapping Matrix

| `AegisError` Variant | Error Code | gRPC Status Code | Description |
| :--- | :--- | :--- | :--- |
| `Validation` | `AEGIS_1001` | `INVALID_ARGUMENT` | Invalid input payload or CLI flag. |
| `Config` | `AEGIS_1002` | `FAILED_PRECONDITION` | Malformed or missing `aegis.toml`. |
| `Project` | `AEGIS_2001` | `NOT_FOUND` | Project ID or repository path missing. |
| `Deployment` | `AEGIS_3001` | `ABORTED` | Deployment failed or rolled back. |
| `Build` | `AEGIS_3002` | `INTERNAL` | Non-zero exit code during build stage. |
| `Runtime` | `AEGIS_4001` | `UNAVAILABLE` | Runtime executable or PID missing. |
| `ArtifactStore` | `AEGIS_5001` | `NOT_FOUND` | Missing build package or invalid SHA256 checksum. |
| `HealthCheck` | `AEGIS_6001` | `DEADLINE_EXCEEDED` | Target HTTP/TCP endpoint failed probes. |
| `Plugin` | `AEGIS_7001` | `UNKNOWN` | Plugin initialization or event handling error. |
| `Network` | `AEGIS_8001` | `UNAVAILABLE` | Daemon unreachable or gRPC stream closed. |

---

## 3. UI & Logging Principles
* **Daemon Logs**: Formatted with `tracing` key-value pairs including `error_code`, `aggregate_id`, and root cause backtrace.
* **CLI Output**: Printed in human-readable red/yellow stderr banners with clear fix recommendations (e.g. `aegis doctor fix`).
* **TUI Banners**: Visual alert notifications displaying short error summary and detailed log drill-down shortcut.
