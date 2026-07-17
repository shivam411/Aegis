# ADR 0003: Domain-Driven Crate Reorganization

## Status
Accepted

## Context
As specified in Phase 1, our architecture separates capabilities into separate library crates. However, a flat directory listing under `crates/` scales poorly. It is difficult to identify dependency boundaries (e.g. which crates belong to process monitoring, which belong to VCS integration, and which are core orchestrators), and circular dependencies are more likely to sneak in.

## Decision
We organize the workspace components into domain directories:
* `core/`: Core application systems (event store, configuration, plugin traits, scheduler, and gRPC interface API definitions).
* `deployment/`: Build pipeline components, release archives, rollbacks, and health checks.
* `runtime/`: Monitored process execution, logs aggregation, and system/process metrics.
* `git/`: VCS repository clones, webhooks, and GitHub actions notifications.
* `ui/`: Visual user interfaces, console CLIs, and TUI dashboards.
* `integrations/`: Integrations with external platforms (Nginx/Caddy proxies, Docker runtimes, Slack alerts).

Run-time executables remain in `apps/` and import these libraries as workspace dependencies.

## Consequences
* **Clear Module Boundaries**: High cohesion within domain subfolders; low coupling between domains.
* **Refactoring Resilience**: The structure separates concern areas, preventing future refactoring loops.
* **Readable Codebase**: It is instantly clear where processes, deployments, or UI logic live.
