# ADR 0001: Event Sourcing as Source of Truth

## Status
Accepted

## Context
Traditional deployment and process control systems (such as PM2 or Kubernetes) manage state as mutable rows in database tables or dynamic in-memory configurations. This has several drawbacks:
* Loss of historic state transitions (e.g. why did an app fail and roll back?).
* Difficult troubleshooting and lack of unified audit logs.
* Complex recovery logic, as state updates can fail mid-transaction.
* Hard to generate analytics (e.g., build durations, success rates) without separate tracking.

## Decision
All state changes in Aegis must be modeled as immutable, sequential events appended to a central event store. 
* Mutating domain state directly is prohibited.
* Current system state is derived/projected by reading and applying the chronological history of events.
* Memory projections may cache current state for performance, but the source of truth is always the event log.

### Proposed Event Types
1. **Core Domain Events**: `ProjectCreated`, `ProjectUpdated`, `ProjectDeleted`.
2. **Deployment Pipeline Events**: `DeploymentQueued`, `BuildStarted`, `BuildFinished`, `TestsPassed`, `ReleaseCreated`, `ReleaseActivated`, `RollbackTriggered`, `RollbackCompleted`.
3. **Runtime Monitoring Events**: `ProcessStarted`, `ProcessStopped`, `ProcessRestarted`, `HealthCheckPassed`, `HealthCheckFailed`.

## Consequences
* **Perfect Audit Trail**: Full historical visibility of every action.
* **Instant Rollbacks**: Rollbacks are triggered by appending a `RollbackTriggered` event and activating the previous healthy release event.
* **Analytics Ready**: Projections can process event timestamps to plot average build durations and success rates.
* **Telemetry & Logging**: Integrates cleanly with TUI widgets, tracing, and webhooks by broadcasting event streams.
* **Complexity**: Event schemas must remain backward-compatible to ensure legacy events can be projected correctly.
