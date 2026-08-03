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
| `ReleaseCreated` | Versioned build package registered | `release_id`, `project_id`, `version`, `commit_sha`, `artifacts` |
| `ReleasePromoted` | Set as active live version | `release_id`, `project_id`, `previous_version`, `promoted_at` |
| `ReleaseArchived` | Cleaned up from retention disk | `release_id`, `project_id`, `archived_at` |

### Deployment Aggregate

| Event Type | Description | Key Payload Fields |
| :--- | :--- | :--- |
| `DeploymentQueued` | Scheduled or manual deploy enqueued | `deployment_id`, `project_id`, `release_id`, `strategy`, `trigger_source` |
| `DeploymentStarted` | Active strategy execution initiated | `deployment_id`, `project_id`, `strategy` |
| `DeploymentSucceeded`| Deployment finished & health verified | `deployment_id`, `project_id`, `duration_ms` |
| `DeploymentFailed` | Build or health check failed | `deployment_id`, `project_id`, `reason` |
| `DeploymentRolledBack`| Reverted to prior stable release | `deployment_id`, `project_id`, `target_version`, `reason` |

### Build Pipeline Aggregate

| Event Type | Description | Key Payload Fields |
| :--- | :--- | :--- |
| `BuildStageClone` | Git checkout stage status | `project_id`, `stage: "Clone"`, `status` |
| `BuildStageInstall` | Dependency installation status | `project_id`, `stage: "Install"`, `status` |
| `BuildStageBuild` | Artifact compilation status | `project_id`, `stage: "Build"`, `status` |
| `BuildStageTest` | Verification test suite status | `project_id`, `stage: "Test"`, `status` |
| `BuildStagePackage` | Artifact packaging status | `project_id`, `stage: "Package"`, `status` |
| `BuildStageVerify` | Checksum & artifact verify status| `project_id`, `stage: "Verify"`, `status` |
| `BuildStagePromote` | Final promotion status | `project_id`, `stage: "Promote"`, `status` |

### Process Aggregate

| Event Type | Description | Key Payload Fields |
| :--- | :--- | :--- |
| `ProcessStarted` | Managed runtime process spawned | `process_id`, `project_id`, `pid`, `runtime` |
| `ProcessStopped` | Gracefully stopped by supervisor | `process_id`, `project_id`, `exit_code` |
| `ProcessCrashed` | Unexpected process termination | `process_id`, `project_id`, `exit_code`, `restart_count` |
| `ProcessRestarted` | Restart policy triggered | `process_id`, `project_id`, `pid` |

---

## 3. Projection Invalidation Rules
1. `ProjectCreated` / `ProjectDeleted` update the `ProjectState` store.
2. `DeploymentQueued` / `DeploymentStarted` / `DeploymentSucceeded` / `DeploymentFailed` transition `DeploymentState.status`.
3. `ProcessStarted` / `ProcessStopped` mutate PID handles in `ProcessState`.
