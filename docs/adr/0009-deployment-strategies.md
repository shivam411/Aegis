# ADR 0009: Deployment Strategies

## Status
Accepted

## Context
A hardcoded `deploy()` function cannot accommodate different deployment models. A hobby project wants immediate restarts; a production API needs rolling updates with health verification; a critical service needs blue/green with traffic switching.

## Decision
Define a `DeploymentStrategy` trait in `deployment/strategy` that abstracts how a release is activated on a target server.

### Interface

```rust
#[async_trait]
pub trait DeploymentStrategy: Send + Sync {
    fn name(&self) -> &str;

    /// Execute the deployment of a release, returning success or rollback signal.
    async fn execute(
        &self,
        ctx: &DeploymentContext,
    ) -> Result<DeploymentOutcome, anyhow::Error>;
}

pub struct DeploymentContext {
    pub project_id: ProjectId,
    pub release: Release,
    pub previous_release: Option<Release>,
    pub health_check: Box<dyn HealthChecker>,
}

pub enum DeploymentOutcome {
    Success,
    RollbackRequired { reason: String },
}
```

### Planned Implementations

| Strategy    | v1  | Description                                                     |
| ----------- | --- | --------------------------------------------------------------- |
| `Immediate` | ✅   | Stop old process, extract artifact, start new process.          |
| `Rolling`   | v2  | Gradually replace instances with health verification per batch. |
| `BlueGreen` | v2  | Deploy to standby environment, switch traffic atomically.       |
| `Canary`    | v3  | Route a percentage of traffic to the new release.               |

Only `Immediate` is implemented in v1. The trait exists now so that adding `Rolling` in v2 does not require architectural changes.

## Consequences
* **Future-Proof**: New strategies are added by implementing a trait, not modifying core code.
* **Testable**: Each strategy can be unit tested with a mock `DeploymentContext`.
* **Configurable**: Projects can specify their preferred strategy in `aegis.toml`.
