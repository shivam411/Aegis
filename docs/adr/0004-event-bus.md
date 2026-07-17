# ADR 0004: Event Bus Architecture

## Status
Accepted

## Context
The Event Store (ADR 0001) provides durable persistence of events. However, persistence alone does not solve real-time communication between subsystems. Without a dedicated messaging layer, modules resort to direct function calls, creating tight coupling.

Consider a `DeploymentStarted` event. It must:
* Update the TUI dashboard in real-time.
* Trigger metrics collection.
* Send a Slack notification.
* Write to the event log.
* Update the state projection.

If each module calls the next directly, the deployment module must know about TUI, metrics, Slack, logging, and projections. This violates separation of concerns and makes the system fragile.

## Decision
Introduce a dedicated **Event Bus** (`core/event_bus`) as the reactive nervous system of the platform.

### Architecture

```
Any Module
    │
    ├── publish(event) ──→ Event Bus ──→ Subscriber: Event Store (persists)
    │                          │
    │                          ├──→ Subscriber: Projection Engine (updates state)
    │                          │
    │                          ├──→ Subscriber: TUI (renders update)
    │                          │
    │                          ├──→ Subscriber: Notification Plugin (alerts)
    │                          │
    │                          └──→ Subscriber: Metrics Collector (records)
```

### Interface

```rust
pub struct EventBus { ... }

impl EventBus {
    pub fn publish(&self, event: DomainEvent);
    pub fn subscribe(&self) -> broadcast::Receiver<DomainEvent>;
}
```

### Relationship to Event Store
The Event Store becomes a **subscriber** of the Event Bus, not the source of truth for distribution. The bus distributes; the store persists.

```
publish() → Event Bus → [Event Store, Projections, TUI, Notifications, ...]
```

## Consequences
* **Decoupled Communication**: No module needs to know about any other module.
* **Extensibility**: Adding a new subscriber (e.g., a Telegram notifier) requires zero changes to existing code.
* **Testability**: Modules can be tested in isolation by injecting a mock bus.
* **Ordering**: Events are ordered within a single publisher. Cross-publisher ordering is eventually consistent, which is acceptable for our use cases.
