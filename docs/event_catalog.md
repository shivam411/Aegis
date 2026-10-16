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
| `ProjectCreated` | New project initialized | `project_id`, `name`, `repository_url`, `branch`, `runtime`, `source_dir` |
| `ProjectSettingsUpdated` | Build/run settings edited in the dashboard or API (override aegis.toml from the next deployment) | `project_id`, `settings`, `actor` |
| `ResourceLimitsChanged` | CPU/memory/process limits changed (applied live) | `project_id`, `limits`, `previous`, `applied_to_processes`, `forced`, `actor` |
| `ResourcePressureDetected` | An alert: `kind` is `oom_kill`, `cpu_throttled` (≥50% of periods for 30 s), `memory_pressure` (≥90% of the limit for 30 s) or `disk_low` (<10% free) | `kind`, `message`, `project_id` (not for `disk_low`), and figures such as `oom_kills`, `memory_limit_bytes` |
| `ScheduleConfigured` | Daily auto-deployment time set (UTC) | `project_id`, `hour`, `minute`, `branch` |
| `DomainMapped` | The built-in proxy serves the project on a domain (re-mapping an existing domain changes its TLS mode) | `project_id`, `domain` (lowercase), `tls` (`acme`, `files` or `off`), `cert_path` and `key_path` (files mode), `actor` |
| `DomainUnmapped` | The domain is no longer served | `project_id`, `domain`, `actor` |
| `SecretUpdated` | A secret environment variable was created or replaced. The value is never recorded; it is stored encrypted outside the event log | `project_id`, `name`, `created`, `actor` |
| `SecretDeleted` | A secret environment variable was removed | `project_id`, `name`, `actor` |
| `ProjectUpdated` | Config or environment updated | `project_id`, `updated_fields` |
| `ProjectDeleted` | Project removed from workspace | `project_id`, `deleted_at` |

### Release Aggregate

| Event Type | Description | Key Payload Fields |
| :--- | :--- | :--- |
| `ReleaseCreated` | A build finished and the release is ready to start | `release_id`, `project_id`, `deployment_id`, `version`, `commit_sha`, `commit_message`, `checksum`, `path` |
| `ReleasePromoted` | The release passed its health check and is now live | `release_id`, `project_id`, `version`, `previous_version`, `process_id` (the process now serving), `port` (where the proxy sends its traffic) |
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
| `BuildStagePromote` | Cutover: the new process started and health-checked, the old one stopped (behind the proxy: new started next to the old, traffic switched, old drained) | `deployment_id`, `project_id`, `stage`, `status`, `detail` |

### Process Aggregate

| Event Type | Description | Key Payload Fields |
| :--- | :--- | :--- |
| `ProcessStartRequested` / `ProcessStopRequested` / `ProcessRestartRequested` | An operator action (audit record) | `process_id`, `project_id` |
| `ProcessStarted` | The supervisor spawned the process (also after each automatic restart) | `process_id`, `project_id`, `pid`, `restart_count`, `release_version`, `command`, `cwd`, `port` (its `PORT`), `proxied` (port allocated for the proxy), `candidate` (a new release starting next to the live one) |
| `ProcessCrashed` | The process exited without being asked to | `process_id`, `project_id`, `exit_code`, `will_restart`, `restart_delay_ms` |
| `ProcessFailed` | Crash loop or spawn failure; the supervisor stopped restarting it | `process_id`, `project_id`, `reason` |
| `ProcessStopped` | The process was stopped | `process_id`, `project_id`, `reason` (`requested`, `replaced`, `restart`, `shutdown`), `exit_code` |

A `ProcessStopped` with reason `requested` or `replaced` marks the process as wanted-down, so it is not restarted when the daemon boots. With reason `shutdown` it is started again on the next boot.

---

### Access & Audit

Every event recorded because of an API call carries `actor`: `user:<name>`, `token:<name>`, `webhook:github`, `cli` (gRPC from the local CLI) or `local-admin` (`aegis-daemon admin …`). Secrets never appear in payloads.

| Event Type | Description | Key Payload Fields |
| :--- | :--- | :--- |
| `UserLoggedIn` | Successful sign-in | `username`, `ip` |
| `UserLoginFailed` | Wrong username or password (the attempted password is never recorded) | `username`, `ip` |
| `UserLoggedOut` | Session ended by the user | `username` |
| `PasswordChanged` / `PasswordReset` | Password changed by its owner / reset by an admin or on the server; that user's sessions end | `username` |
| `UserCreated` | An admin (or `aegis-daemon admin create-user`) added an account | `username`, `role`, `actor` |
| `UserRoleChanged` | An account's role changed; open sessions use the new role at once | `username`, `from`, `to`, `actor` |
| `UserDeleted` | An account was removed and signed out | `username`, `actor` |
| `ApiTokenCreated` / `ApiTokenRevoked` | API token lifecycle | `token_id`, plus `name` and `scope` when created |
| `WebhookSecretRotated` | A project's webhook secret was created or replaced | `project_id` |
| `WebhookReceived` | A delivery with a valid signature was handled (including dashboard test deliveries) | `project_id`, `delivery`, `event`, `status` (`queued`, `ignored`, `duplicate`, `pong`, `error`), `reason`, `deployment_id`, `actor` |

---

## 3. Projection Invalidation Rules
1. `ProjectCreated` creates or updates `ProjectState`.
2. `DeploymentQueued` → `Queued`, `DeploymentStarted` → `InProgress`, `DeploymentCompleted` → `Success`, `DeploymentFailed` → `Failed`, `DeploymentRolledBack` → `RolledBack`. `BuildStage*` events update the deployment's current stage.
3. `ReleaseCreated` → release `Built`. `ReleasePromoted` makes the release `Active`, marks the project's previously active release `Inactive`, records when it went live, and sets the project's current release. The time it went live, not build order, decides the default rollback target and which releases retention keeps. A built release whose deployment fails becomes `Failed`. `ReleaseArchived` → `Archived`.
4. `ProcessStarted` sets the project's current process, unless it is a `candidate`: then `ReleasePromoted` (via `process_id`) makes it current once it is healthy. Process events update status, PID, port, exit code and desired state.
5. `DomainMapped` adds the domain to the project (removing it from any other project); `DomainUnmapped` removes it. On boot, the proxy's routes are rebuilt from the projects' domains and their current processes' ports.
6. Events are applied in insertion order (`seq`), synchronously as they are stored, and replayed in the same order on startup.
