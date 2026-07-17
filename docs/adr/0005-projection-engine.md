# ADR 0005: Projection Engine (CQRS-Lite)

## Status
Accepted

## Context
With event sourcing (ADR 0001), the canonical state of every entity is the sequence of events that produced it. Reconstructing current state by replaying all events on every query is prohibitively expensive, especially for TUI rendering at 60fps.

## Decision
Introduce a **Projection Engine** (`core/projection`) that subscribes to the Event Bus and maintains in-memory state caches for each domain entity.

### Architecture

```
Event Bus
    │
    └──→ Projection Engine
              │
              ├── ProjectProjection     (HashMap<ProjectId, ProjectState>)
              ├── DeploymentProjection  (HashMap<DeploymentId, DeploymentState>)
              ├── ReleaseProjection     (HashMap<ReleaseId, ReleaseState>)
              ├── ProcessProjection     (HashMap<ProcessId, ProcessState>)
              └── ServerProjection      (HashMap<ServerId, ServerState>)
```

### Interface

```rust
pub trait Projection: Send + Sync {
    type Event;
    fn apply(&mut self, event: &Self::Event);
}
```

Each projection implements `apply()` to fold an event into its current state. Projections are rebuilt on daemon startup by replaying events from the store, then kept live via bus subscription.

### Read Path (CQRS-Lite)

```
CLI / TUI
    │
    └──→ Query Projection (read current state)

Never:
    CLI / TUI → Query Event Store → Reconstruct state
```

### Write Path

```
CLI / TUI
    │
    └──→ Command → Daemon → publish(event) → Event Bus
```

## Consequences
* **Fast Reads**: TUI reads pre-computed state, not event streams.
* **Separation of Concerns**: Write path (events) and read path (projections) are independent.
* **Rebuild Safety**: Projections can be rebuilt from the event store at any time if they become corrupted or schema changes.
* **Memory Overhead**: Acceptable for the expected scale (hundreds of projects, not millions).
