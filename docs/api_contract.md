# Aegis Public API Contract Specification

This document establishes the public API freeze and classification system for all Aegis workspace crates. Every public module, struct, trait, and function is classified into one of three stability tiers:

* **Stable (`[STABLE]`)**: Guaranteed backwards-compatible public API. Breaking changes require a major version bump.
* **Experimental (`[EXPERIMENTAL]`)**: Publicly accessible interface undergoing active iteration. May change in minor releases.
* **Internal (`[INTERNAL]`)**: Private or crate-internal implementation detail. Must not be consumed outside the declaring crate.

---

## 1. Domain Types (`aegis-types`)

| Target Symbol | Stability Tier | Description |
| :--- | :--- | :--- |
| `ProjectId`, `ReleaseId`, `DeploymentId`, `ProcessId`, `ServerId`, `EventId`, `ArtifactId` | `[STABLE]` | Type-safe UUIDv7 entity wrappers. |
| `Event` | `[STABLE]` | Immutable domain event header and payload container. |
| `AegisError` | `[STABLE]` | Typed domain error enumeration. |

---

## 2. Platform Kernel (`core/*`)

### `aegis-event-bus`
* **`[STABLE]`** `EventBus::new()`
* **`[STABLE]`** `EventBus::publish(event: Event)`
* **`[STABLE]`** `EventBus::subscribe() -> Receiver<Event>`

### `aegis-event-store`
* **`[STABLE]`** `EventStore::new(pool: SqlitePool) -> Self`
* **`[STABLE]`** `EventStore::append_event(type: &str, payload: Value) -> Result<Event, AegisError>`
* **`[STABLE]`** `EventStore::get_events() -> Result<Vec<Event>, AegisError>` (insertion order)
* **`[EXPERIMENTAL]`** `EventStore::add_sync_listener(...)`: runs a callback for each event, in sequence order, before it is broadcast
* **`[INTERNAL]`** `EventStore::initialize_db(pool: &SqlitePool)`

### `aegis-control`
* **`[EXPERIMENTAL]`** `ControlPlane`: the command and query API used by every transport (gRPC, HTTP). Covers `register_project`, `queue_deployment`, `rollback`, `process_action`, `configure_schedule`, `recover`, `shutdown`, and the `list_*` and `resolve_*` queries.
* **`[EXPERIMENTAL]`** `ControlError` (`NotFound`, `InvalidArgument`, `FailedPrecondition`, `Internal`)

### `aegis-projection`
* **`[STABLE]`** `ProjectionEngine::new() -> Self`
* **`[STABLE]`** `ProjectionEngine::apply_event(event: &Event)`
* **`[STABLE]`** `ProjectionEngine::replay_from_store(events: &[Event])`
* **`[STABLE]`** `ProjectionEngine::get_projects()`, `get_releases()`, `get_deployments()`, `get_processes()`
* **`[INTERNAL]`** In-memory `RwLock<HashMap<...>>` storage models.

### `aegis-api`
* **`[STABLE]`** Protocol Buffer service definition (`aegis.proto`).
* **`[STABLE]`** `AegisDaemonClient` and `AegisDaemonServer` gRPC bindings.

### `aegis-web`
* **`[EXPERIMENTAL]`** HTTP/JSON API under `/api/v1`, described in [`openapi.json`](openapi.json) and [`http_api.md`](http_api.md).

### `aegis-scheduler`
* **`[STABLE]`** `SchedulerEngine::schedule_daily_deploy(...)`
* **`[STABLE]`** `SchedulerEngine::evaluate_and_trigger(...)`
* **`[EXPERIMENTAL]`** Generic background job engine hooks.

---

## 3. Deployment Engine (`deployment/*`)

### `aegis-release`
* **`[STABLE]`** `Release` struct & `BuildMetadata`.
* **`[STABLE]`** `ReleaseSwitcher::prepare_version_dir(...)`
* **`[STABLE]`** `ReleaseSwitcher::switch_to_version(...)`
* **`[STABLE]`** `ReleaseSwitcher::cleanup_old_versions(...)`

### `aegis-artifact-store`
* **`[STABLE]`** `ArtifactStore::store(...) -> Result<StoredArtifact, AegisError>`
* **`[STABLE]`** `ArtifactStore::get(...) -> Result<PathBuf, AegisError>`
* **`[STABLE]`** `ArtifactStore::verify(...) -> Result<bool, AegisError>`
* **`[STABLE]`** `ArtifactStore::cleanup(...) -> Result<usize, AegisError>`
* **`[EXPERIMENTAL]`** `ArtifactStore::register(...)`, `verify_checksum(...)`, `fingerprint_path(...)`

### `aegis-builder`
* **`[EXPERIMENTAL]`** `BuildPipeline::new(artifact_store, reporter)` and `BuildPipeline::run(&BuildRequest) -> Result<BuildOutput, BuildFailure>`
* **`[EXPERIMENTAL]`** `trait StageReporter`, `SourceSpec` (`LocalDir`, `Git`)
* **`[STABLE]`** `PipelineStage` enum (`Clone`, `Install`, `Build`, `Test`, `Package`, `Verify`, `Promote`).

---

## 4. Runtime & Process Engine (`runtime/*`)

### `aegis-engine`
* **`[STABLE]`** `trait Runtime` (`name()`, `detect()`). It only detects the runtime; commands come from `aegis.toml`, and the process supervisor owns the process lifecycle.
* **`[EXPERIMENTAL]`** `ResolvedProjectConfig`: `aegis.toml` merged with detected defaults
* **`[STABLE]`** `RuntimeDetector::detect_runtime(...)`
* **`[STABLE]`** `NodeRuntime`, `RustRuntime`, `GoRuntime`, `PythonRuntime`, `GenericRuntime`.
* **`[INTERNAL]`** Process handle signal low-level interactions.

---

## 5. UI & Integrations (`ui/*`, `integrations/*`)

### `aegis-cli`
* **`[STABLE]`** Command line interface flags & gRPC client invocation handlers.

### `aegis-tui`
* **`[EXPERIMENTAL]`** Terminal user interface widget state and rendering components (`Ratatui`).

### `aegis-plugins`
* **`[STABLE]`** `trait Plugin` (`name()`, `on_init()`, `on_event()`)
* **`[STABLE]`** `PluginManager::register(...)`, `initialize_all()`, `start_event_loop()`
