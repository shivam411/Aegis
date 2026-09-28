# Architecture Deep Dive

This document explains how Aegis is structured internally — its crate topology, event-sourcing model, deployment pipeline, and runtime architecture.

---

## Design Principles

1. **Zero External Dependencies** — No PostgreSQL, Redis, RabbitMQ, or Docker required. Aegis runs as a single daemon process with embedded SQLite.
2. **Event Sourcing as Source of Truth** — Every state mutation is recorded as an immutable domain event. State is derived by replaying events, never stored directly.
3. **Domain-Driven Crate Topology** — Each bounded context is an independent Rust crate with explicit dependency boundaries.
4. **Atomic Deployments** — New versions are built in isolation and activated via atomic pointer switches. Live traffic is never interrupted during builds.

---

## Crate Topology

Aegis is organized as a Rust workspace with 34 crates across 6 domain directories:

```
Aegis/
├── core/              # Platform foundation
│   ├── daemon/        # Main daemon binary (gRPC server, startup orchestration)
│   ├── config/        # Configuration loading (aegis.toml)
│   ├── event_store/   # SQLite-backed event persistence
│   ├── event_bus/     # tokio::broadcast pub/sub event distribution
│   ├── projection/    # In-memory state projection from event replay
│   ├── control/       # ControlPlane: deployments, rollbacks, process control (used by gRPC and HTTP)
│   ├── web/           # HTTP/JSON API + SSE (loopback-only until authentication lands)
│   ├── plugin/        # Plugin manager trait and lifecycle
│   ├── types/         # Shared domain types (ProjectId, ReleaseId, EventId, etc.)
│   ├── api/           # gRPC service definitions (tonic + prost)
│   ├── state/         # State management primitives
│   ├── scheduler/     # Daily auto-deployment scheduler engine
│   ├── auth/          # Authentication primitives
│   ├── secrets/       # Secrets management
│   └── ssh/           # SSH key management
│
├── deployment/        # Deployment domain
│   ├── release/       # Release model + ReleaseSwitcher (atomic version switching)
│   ├── artifact_store/# Disk-backed artifact storage with SHA-256 integrity
│   ├── builder/       # 7-stage build pipeline
│   ├── health/        # TCP and HTTP health checkers
│   └── analytics/     # Deployment analytics
│
├── runtime/           # Process runtime domain
│   ├── engine/        # Polyglot runtime detection (Node, Rust, Go, Python, Bun, Deno)
│   ├── process/       # Process supervisor (spawn, stop, graceful drain)
│   ├── logs/          # Log aggregation
│   └── metrics/       # Runtime metrics
│
├── git/               # Git integration domain
│   ├── repository/    # Git clone, pull, checkout operations
│   ├── webhook/       # Incoming webhook handler
│   └── action/        # Git-triggered actions
│
├── ui/                # User interface domain
│   ├── cli/           # CLI client (clap + tonic gRPC)
│   └── tui/           # Terminal UI (ratatui + crossterm)
│
└── integrations/      # External service integrations
    ├── slack/          # Slack notification plugin
    ├── github/         # GitHub status API integration
    └── docker/         # Docker container support
```

---

## Event-Sourcing & CQRS Flow

```
┌─────────────────┐       Command       ┌──────────────┐
│  CLI / TUI /    │ ──────────────────► │ Aegis Daemon │
│  Git Webhooks   │                     └──────┬───────┘
└─────────────────┘                            │
                                        (Emits Event)
                                               ▼
                                      ┌─────────────────┐
                                      │    Event Bus    │  (tokio::broadcast)
                                      └────────┬────────┘
                    ┌──────────────────────────┼──────────────────────────┐
                    ▼                          ▼                          ▼
          ┌───────────────────┐      ┌───────────────────┐      ┌───────────────────┐
          │    Event Store    │      │ Projection Engine │      │  Plugin Manager   │
          │    (SQLite DB)    │      │(In-Memory Caches) │      │  (Slack / GitHub) │
          └───────────────────┘      └───────────────────┘      └───────────────────┘
```

### Write Path (Commands)

1. CLI or webhook sends a gRPC request to the daemon.
2. Daemon validates the command and constructs a domain `Event`.
3. Event is persisted to the `EventStore` (SQLite `events` table).
4. Event is published to the `EventBus` (tokio broadcast channel).

### Read Path (Queries)

1. `ProjectionEngine` subscribes to the `EventBus`.
2. Each event is folded into in-memory hash maps (`projects`, `releases`, `deployments`, `processes`, `servers`).
3. CLI `status` and TUI dashboard read directly from projected state — no database queries for reads.

### Replay on Startup

On daemon boot, the `ProjectionEngine` replays all historical events from SQLite to reconstruct full in-memory state. This guarantees consistency after restarts.

---

## 7-Stage Build Pipeline

Every deployment executes a strict, composable pipeline:

```
┌─────────┐   ┌─────────┐   ┌─────────┐   ┌─────────┐   ┌───────────┐   ┌──────────┐   ┌───────────┐
│ 1.Clone │──►│2.Install│──►│ 3.Build │──►│ 4.Test  │──►│ 5.Package │──►│ 6.Verify │──►│ 7.Promote │
└─────────┘   └─────────┘   └─────────┘   └─────────┘   └───────────┘   └──────────┘   └───────────┘
```

| Stage | Description |
| :--- | :--- |
| **Clone** | `git clone` into an isolated build directory (`~/.aegis/builds/{release_id}/`) |
| **Install** | Install dependencies (`npm install`, `cargo fetch`, `go mod download`, `pip install`) |
| **Build** | Compile (`npm run build`, `cargo build --release`, `go build`) |
| **Test** | Run test suite (`npm test`, `cargo test`, `go test`) |
| **Package** | Archive artifacts with SHA-256 checksums into `~/.aegis/artifacts/` |
| **Verify** | Start new instance, run TCP/HTTP health check |
| **Promote** | Atomically update `current.json` version pointer, gracefully drain old process |

If any stage fails, the pipeline aborts. The live version is never touched.

---

## Zero-Downtime Deployment (GracefulSwitch Strategy)

```
                     Build in Isolation
                     (~/.aegis/builds/)
                            │
                            ▼
                    Health Check New Instance
                            │
                     ┌──────┴──────┐
                     │             │
                  PASS           FAIL
                     │             │
                     ▼             ▼
          Atomically Switch    Abort Switch
          current.json         (Live version
                │               untouched)
                ▼
         Graceful Drain
         Old Process
         (SIGTERM → wait → SIGKILL)
                │
                ▼
         Auto-Prune Old Versions
         (max_retained_versions)
```

---

## Deployment Strategies

| Strategy | Behavior |
| :--- | :--- |
| **Immediate** | Direct activation with post-deployment health check |
| **Rolling** | Batch-based incremental rollout |
| **BlueGreen** | Parallel environment switching |
| **GracefulSwitch** | Isolated build → health check → atomic switch → graceful drain → auto-prune |

---

## Scheduler Engine

The `SchedulerEngine` runs a background loop evaluating project schedules every 30 seconds:

1. Compare current UTC hour/minute against registered `ScheduledTask` entries.
2. If matched and not already triggered today, emit a `DeploymentQueued` event to the `EventBus`.
3. The deployment pipeline picks up the event and executes a full zero-downtime build.

---

## Domain Type System

All domain identifiers use strongly-typed `UUIDv7` wrappers for type safety and time-sortability:

```rust
ProjectId     // Identifies a project
ReleaseId     // Identifies a release
DeploymentId  // Identifies a deployment
ProcessId     // Identifies a running process
EventId       // Identifies an event
ArtifactId    // Identifies a stored artifact
ServerId      // Identifies a server node
```

This prevents accidental ID misuse across domain boundaries (e.g., passing a `ProjectId` where a `ReleaseId` is expected).
