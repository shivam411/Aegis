# Aegis Event Catalog & Taxonomy Specification

This specification defines the complete domain event taxonomy, metadata header structure, and payload contracts for Aegis event sourcing.

---

## 1. Event Header Schema

Every event published to `EventBus` or stored in SQLite must contain standard metadata:

```json
{
  "id": "01910a3b-7f12-7890-8b01-123456789abc",
  "schema_version": 1,
  "event_type": "DeploymentStarted",
  "aggregate_type": "Deployment",
  "aggregate_id": "01910a3b-7f12-7890-8b01-987654321def",
  "correlation_id": "01910a3b-7f12-7890-8b01-111111111111",
  "causation_id": "01910a3b-7f12-7890-8b01-222222222222",
  "created_at": "2026-08-03T08:00:00Z",
  "metadata": {
    "actor": "cli",
    "host": "vps-01"
  },
  "payload_json": "{ ... }"
}
```

### Metadata Header Fields
* **`id`** (`EventId`): UUIDv7 unique event identifier.
* **`schema_version`** (`u32`): Version of the payload schema (default: `1`).
* **`event_type`** (`String`): PascalCase domain event identifier.
* **`aggregate_type`** (`String`): Domain boundary aggregate (`Project`, `Release`, `Deployment`, `Build`, `Process`, `Server`).
* **`aggregate_id`** (`String`): Primary UUIDv7 target identifier.
* **`correlation_id`** (`Option<String>`): Tracing ID connecting all related events in an execution lifecycle.
* **`causation_id`** (`Option<String>`): ID of the exact trigger event or command that caused this event.
* **`created_at`** (`String`): RFC3339 UTC timestamp.

---

## 2. Domain Taxonomy by Aggregate

### Project Aggregate

| Event Type | Description | Key Payload Fields |
| :--- | :--- | :--- |
| `ProjectCreated` | New project initialized | `project_id`, `name`, `repository_url`, `branch`, `runtime` |
| `ProjectUpdated` | Config or environment updated | `project_id`, `updated_fields` |
| `ProjectDeleted` | Project removed from workspace | `project_id`, `deleted_at` |

### Release Aggregate

| Event Type | Description | Key Payload Fields |
| :--- | :--- | :--- |
| `ReleaseCreated` | A build finished and the release is ready to start | `release_id`, `project_id`, `deployment_id`, `version`, `commit_sha`, `commit_message`, `checksum`, `path` |
| `ReleasePromoted` | The release passed its health check and is now live | `release_id`, `project_id`, `version`, `previous_version` |
| `ReleaseArchived` | The release directory was removed by retention | `project_id`, `version` |

### Deployment Aggregate

| Event Type | Description | Key Payload Fields |
| :--- | :--- | :--- |
| `DeploymentQueued` | A deployment was requested (CLI, schedule or webhook) | `deployment_id`, `project_id`, `strategy`, `branch`, `source_dir` or `repository_url`, `commit_sha`, `version`, `trigger_source` |
| `DeploymentStarted` | The daemon began building | `deployment_id`, `project_id`, `version`, `strategy` |
| `DeploymentCompleted` | The new release is live and healthy | `deployment_id`, `project_id`, `release_id`, `version` |
| `DeploymentFailed` | A build stage or the health check failed | `deployment_id`, `project_id`, `stage`, `reason`, `log_tail`, `rolled_back_to` |
| `DeploymentRolledBack` | After a failed health check, the previous release was started again | `deployment_id`, `project_id`, `target_version`, `reason` |
| `RollbackRequested` | An operator asked to switch to an earlier release | `project_id`, `from_version`, `target_version` |
| `RollbackCompleted` | The earlier release is live | `project_id`, `from_version`, `to_version` |
| `RollbackFailed` | The earlier release did not become healthy | `project_id`, `target_version`, `reason`, `restored` |

`DeploymentSucceeded` is accepted as a synonym for `DeploymentCompleted` when replaying history.

### Build Pipeline Aggregate

Each stage reports `Started`, then `Success`, `Skipped` (no command configured) or `Failed`.

| Event Type | Description | Key Payload Fields |
| :--- | :--- | :--- |
| `BuildStageClone` | Source copied or cloned into the release directory | `deployment_id`, `project_id`, `stage`, `status`, `detail` (commit) |
| `BuildStageInstall` | `build.install_command` | `deployment_id`, `project_id`, `stage`, `status`, `detail` |
| `BuildStageBuild` | `build.build_command` | `deployment_id`, `project_id`, `stage`, `status`, `detail` |
| `BuildStageTest` | `build.test_command` | `deployment_id`, `project_id`, `stage`, `status`, `detail` |
| `BuildStagePackage` | Content checksum of the release recorded | `deployment_id`, `project_id`, `stage`, `status`, `detail` (sha256) |
| `BuildStageVerify` | Artifact metadata and files present | `deployment_id`, `project_id`, `stage`, `status` |
| `BuildStagePromote` | Cutover: old process stopped, new process started and health-checked | `deployment_id`, `project_id`, `stage`, `status`, `detail` |

### Process Aggregate

| Event Type | Description | Key Payload Fields |
| :--- | :--- | :--- |
| `ProcessStartRequested` / `ProcessStopRequested` / `ProcessRestartRequested` | An operator action (audit record) | `process_id`, `project_id` |
| `ProcessStarted` | The supervisor spawned the process (also after each automatic restart) | `process_id`, `project_id`, `pid`, `restart_count`, `release_version`, `command`, `cwd` |
| `ProcessCrashed` | The process exited without being asked to | `process_id`, `project_id`, `exit_code`, `will_restart`, `restart_delay_ms` |
| `ProcessFailed` | Crash loop or spawn failure; the supervisor stopped restarting it | `process_id`, `project_id`, `reason` |
| `ProcessStopped` | The process was stopped | `process_id`, `project_id`, `reason` (`requested`, `replaced`, `restart`, `shutdown`), `exit_code` |

A `ProcessStopped` with reason `requested` or `replaced` marks the process as wanted-down, so it is not restarted when the daemon boots. With reason `shutdown` it is started again on the next boot.

---

## 3. Projection Invalidation Rules
1. `ProjectCreated` creates or updates `ProjectState`.
2. `DeploymentQueued` → `Queued`, `DeploymentStarted` → `InProgress`, `DeploymentCompleted` → `Success`, `DeploymentFailed` → `Failed`, `DeploymentRolledBack` → `RolledBack`. `BuildStage*` events update the deployment's current stage.
3. `ReleaseCreated` → release `Built`. `ReleasePromoted` makes the release `Active`, marks the project's previously active release `Inactive`, and sets the project's current release. A built release whose deployment fails becomes `Failed`. `ReleaseArchived` → `Archived`.
4. `ProcessStarted` sets the project's current process. Process events update status, PID, exit code and desired state.
5. Events are applied in insertion order (`seq`), synchronously as they are stored, and replayed in the same order on startup.
