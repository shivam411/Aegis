# ADR 0006: Domain Type IDs (UUIDv7)

## Status
Accepted

## Context
Using raw `String` or generic `Uuid` types for entity identifiers leads to:
* Accidental misuse (passing a `ProjectId` where a `DeploymentId` is expected).
* No compile-time safety on foreign key relationships.
* Ambiguous API signatures like `fn get(id: String)`.

## Decision
Introduce strongly-typed ID wrappers in `core/types` using **UUIDv7**.

### Why UUIDv7
* **Time-sortable**: Embeds a Unix timestamp in the high bits. Sorting by ID is equivalent to sorting by creation time.
* **Globally unique**: No coordination required between distributed agents.
* **Database-friendly**: Can be stored as TEXT in SQLite, indexed efficiently due to monotonic ordering.

### Types

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProjectId(Uuid);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReleaseId(Uuid);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeploymentId(Uuid);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProcessId(Uuid);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ServerId(Uuid);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventId(Uuid);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArtifactId(Uuid);
```

Each type implements `new()` (generates UUIDv7), `Display`, `FromStr`, and serde traits via a shared macro.

## Consequences
* **Compile-Time Safety**: `fn deploy(project: ProjectId, release: ReleaseId)` cannot be called with swapped arguments.
* **Sortable History**: Listing events or deployments by ID produces chronological order.
* **Zero Runtime Cost**: Newtype wrappers are optimized away by the compiler.
