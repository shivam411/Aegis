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
| `ProjectCreated` | New project initialized | `project_id`, `name`, `repository_url`, `branch`, `runtime`, `source_dir`, `source_subdir` (the folder of the source that is built and run, or `null`) |
| `AppsImported` | Apps registered in bulk from a folder scan (each also has its own `ProjectCreated`; none is deployed) | `count`, `apps` (`[{project_id, name}]`), `actor` |
| `DeployKeyCreated` | An SSH deploy key was generated for the project. Only the fingerprint is recorded, never key material; the private key is sealed in the database | `project_id`, `fingerprint` (`SHA256:…`), `replaced` (the previous key's fingerprint, or `null`), `actor` |
| `DeployKeyDeleted` | The project's deploy key was deleted | `project_id`, `fingerprint`, `actor` |
| `ProjectSettingsUpdated` | Build/run settings edited in the dashboard or API (override aegis.toml from the next deployment), including Variables, the health-check and drain timeouts (`settings.health_check_timeout_secs`, `settings.drain_timeout_secs`) and the env file path (`settings.env_file`; the file's contents are never recorded). A `.env` import that adds Variables records one | `project_id`, `settings`, `actor` |
| `ResourceLimitsChanged` | CPU/memory/process limits changed (applied live) | `project_id`, `limits`, `previous`, `applied_to_processes`, `forced`, `actor` |
| `ResourcePressureDetected` | An alert: `kind` is `oom_kill`, `cpu_throttled` (≥50% of periods for 30 s), `memory_pressure` (≥90% of the limit for 30 s) or `disk_low` (<10% free) | `kind`, `message`, `project_id` (not for `disk_low`), and figures such as `oom_kills`, `memory_limit_bytes` |
| `ScheduleConfigured` | Daily auto-deployment time set (UTC) | `project_id`, `hour`, `minute`, `branch` |
| `DomainMapped` | The built-in proxy serves the project on a domain (re-mapping an existing domain changes its TLS mode) | `project_id`, `domain` (lowercase), `tls` (`acme`, `files` or `off`), `cert_path` and `key_path` (files mode), `actor` |
| `DomainUnmapped` | The domain is no longer served | `project_id`, `domain`, `actor` |
| `SecretUpdated` | A secret environment variable was created or replaced. The value is never recorded; it is stored encrypted outside the event log | `project_id`, `name`, `created`, `actor` |
| `SecretDeleted` | A secret environment variable was removed | `project_id`, `name`, `actor` |
| `ProjectUpdated` | Config or environment updated | `project_id`, `updated_fields` |
| `ProjectDeleted` | The app was deleted (dashboard Settings, `DELETE /api/v1/projects/{project}` or `aegis delete`). Its processes were stopped first; after this event its secrets, deploy key, webhook secret and (unless `keep_releases`) release directories, logs and artifact metadata are removed, and that cleanup is repeated on every daemon start until done. Read models drop the project, its releases, deployments and processes, and ignore later events about it (e.g. a late `ProcessStopped`); its earlier events stay as history. Registering the same project id again (`ProjectCreated`) starts it afresh. Can't be injected with `aegis emit-event` | `project_id`, `name`, `keep_releases`, `kept_dir`, `domains` (no longer served), `repository_url`, `source_dir`, `stopped_process_ids`, `release_count`, `actor` |
| `ProjectSourceChanged` | The app's source changed between a repository and a server folder (or to another repository, branch or subfolder); used from the next deployment | `project_id`, `kind` (`git` or `folder`), `repository_url` (empty for a folder), `branch`, `source_dir`, `source_subdir`, `previous` (the same four fields before), `actor` |

### Groups

Apps deployed together from one repository and branch; see [groups.md](groups.md).

| Event Type | Description | Key Payload Fields |
| :--- | :--- | :--- |
| `GroupCreated` / `GroupUpdated` | A group was created or changed | `group_id`, `name`, `repository_url`, `branch`, `rollback_on_failure`, `actor` |
| `GroupMembersChanged` | The group's apps, in deploy order, with their watch paths (the whole list) | `group_id`, `members` (`[{project_id, watch_paths}]`), `actor` |
| `GroupDeleted` | The group was deleted (its apps stay) | `group_id`, `name`, `actor` |
| `GroupDeploymentStarted` | A group deployment began | `group_id`, `group_deployment_id`, `commit` (if given), `only_changed`, `trigger` (`http`, `cli`, `github-push`), `members` |
| `GroupCheckoutReady` | The repository was cloned once and checked out at the commit every app is built from | `group_id`, `group_deployment_id`, `commit`, `checkout` |
| `GroupMemberSkipped` | An app was not deployed: nothing it watches changed, or it is already live at the commit | `group_id`, `group_deployment_id`, `project_id`, `reason` |
| `GroupMemberDeploying` / `GroupMemberDeployed` | An app's deployment was queued / went live | `group_id`, `group_deployment_id`, `project_id`, `deployment_id`, `previous_version`, `version`, `commit` |
| `GroupMemberFailed` | An app's deployment failed; the apps after it are not deployed | `group_id`, `group_deployment_id`, `project_id`, `reason` |
| `GroupMembersRolledBack` | Apps went back to their previous release: after a failure (`reason: failure`, with roll back on failure) or on request (`reason: requested`) | `group_id`, `group_deployment_id`, `project_ids`, `reason`, `failed` |
| `GroupRollbackStarted` | **Roll back group** began | `group_id`, `group_deployment_id`, `project_ids` |
| `GroupDeploymentFinished` | Every app was deployed or skipped | `group_id`, `group_deployment_id`, `commit`, `deployed` |
| `GroupDeploymentFailed` | The group deployment stopped: an app failed, the clone failed, or the daemon restarted meanwhile | `group_id`, `group_deployment_id`, `reason`, `failed_project_id`, `rolled_back`, `not_deployed` |

`WebhookReceived` and `WebhookSecretRotated` carry `group_id` (instead of `project_id`) for a group's webhook.

### Release Aggregate

| Event Type | Description | Key Payload Fields |
| :--- | :--- | :--- |
| `ReleaseCreated` | A build finished and the release is ready to start | `release_id`, `project_id`, `deployment_id`, `version`, `commit_sha`, `commit_message`, `checksum`, `path` |
| `ReleasePromoted` | The release passed its health check and is now live | `release_id`, `project_id`, `version`, `previous_version`, `process_id` (the process now serving; the first instance), `port` (where the proxy sends its traffic), and for multi-instance cutovers `process_ids` and `ports` (every instance, in instance order) |
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
| `RollbackCompleted` | The earlier release is live | `project_id`, `from_version`, `to_version`, `instant` (switched back to a BlueGreen standby without starting anything) |
| `RollbackFailed` | The earlier release did not become healthy | `project_id`, `target_version`, `reason`, `restored` |

`DeploymentSucceeded` is accepted as a synonym for `DeploymentCompleted` when replaying history.

### Rollouts and instances

Recorded for apps with several instances and for the Rolling and BlueGreen strategies (see [Deployment strategies](configuration.md#deployment-strategies-and-instances)). An instance in these payloads is `{"process_id", "instance" (1-based), "port"}`. Events of a deployment carry its `deployment_id`, so they appear in its timeline.

| Event Type | Description | Key Payload Fields |
| :--- | :--- | :--- |
| `RolloutStepCompleted` | One Rolling step finished: new instances healthy and in the proxy, old ones drained and stopped | `deployment_id`, `project_id`, `strategy`, `version`, `step`, `steps`, `started` (instances), `stopped` (instances), `new_serving`, `old_serving` |
| `RolloutAborted` | A new instance did not become healthy; the rollout stopped and the old set was restored | `deployment_id`, `project_id`, `strategy`, `version`, `step`, `steps`, `reason`, `restored_instances` |
| `TrafficSwitched` | The proxy moved from one set of instances to another in one step (GracefulSwitch/Immediate/BlueGreen with several instances, or an instant BlueGreen rollback) | `deployment_id` (none for a rollback), `project_id`, `strategy`, `from_version`, `to_version`, `instances`, `previous_instances`, `instant_rollback` |
| `StandbyKept` | BlueGreen: the previous set keeps running as an instant rollback target | `deployment_id`, `project_id`, `version`, `process_ids`, `until` (RFC 3339) |
| `StandbyStopped` | The BlueGreen standby was stopped | `project_id`, `version`, `process_ids`, `reason` (`expired`, `replaced` by another release going live, `stopped` with the app) |
| `InstanceSetChanged` | The live set of instances changed without a new release (on boot, missing instances were started) | `project_id`, `version`, `process_ids`, `ports`, `reason` (`recovered`) |

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
| `ProcessStarted` | The supervisor spawned the process (also after each automatic restart) | `process_id`, `project_id`, `pid`, `restart_count`, `release_version`, `command`, `cwd`, `port` (its `PORT`), `proxied` (port allocated for the proxy), `candidate` (a new release starting next to the live one, or an instance outside the live set such as a BlueGreen standby), `instance` (1-based number, for apps with several instances), `runtime` (`process` or `docker`), `container` and `image` (container apps; `pid` is then the container's main process and `command` is `docker: <image>`) |
| `ProcessCrashed` | The process exited without being asked to | `process_id`, `project_id`, `exit_code`, `will_restart`, `restart_delay_ms` |
| `ProcessFailed` | Crash loop or spawn failure; the supervisor stopped restarting it | `process_id`, `project_id`, `reason` |
| `ProcessStopped` | The process was stopped | `process_id`, `project_id`, `reason` (`requested`, `replaced`, `restart`, `shutdown`), `exit_code` |

A `ProcessStopped` with reason `requested` or `replaced` marks the process as wanted-down, so it is not restarted when the daemon boots. With reason `shutdown` it is started again on the next boot. An exception: `replaced` for an instance that is still in its app's live set of several (a Rolling update replaced it but the new set was never promoted) keeps it wanted, so an interrupted rollout comes back as the old set.

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

### Backups

See [`disaster_recovery.md`](disaster_recovery.md). Payloads name archives only; their contents (the database, the secrets key) never appear in events. A backup's own database snapshot is taken before its `BackupCreated` is recorded, so the archive doesn't contain that event.

| Event Type | Description | Key Payload Fields |
| :--- | :--- | :--- |
| `BackupCreated` | An archive was written to `<data_dir>/backups` (or `--out`) | `name`, `size_bytes`, `includes_releases`, `file_count`, `trigger` (`manual` from the dashboard/API, `scheduled`, `cli`, `update` before an update from the dashboard or `aegis-daemon self-update`), `actor` |
| `BackupFailed` | Creating a backup failed | `trigger`, `include_releases`, `error`, `actor` |
| `BackupDeleted` | An archive was deleted | `name`, `reason` (`manual` or `retention`), `actor` |

Restores are offline (the daemon is stopped), so they are not recorded as events; the restored database's history ends where the backup was taken.

### Aegis updates

See [`upgrading.md`](upgrading.md). Payloads hold versions, URLs and error messages only; no credentials or file contents. A pre-update backup also records its own `BackupCreated` with `trigger: "update"`.

| Event Type | Description | Key Payload Fields |
| :--- | :--- | :--- |
| `UpdateAvailable` | An update check found a newer stable release (recorded once per release) | `current_version`, `latest_version`, `url` (release notes) |
| `UpdateStarted` | An admin started an upgrade from the dashboard/API | `from_version`, `to_version`, `mode` (`in_place` or `helper`), `actor` |
| `UpdateApplied` | Recorded by the **new** version when it starts after an update; also when the daemon notices it was upgraded by other means | `from_version`, `to_version`, `method` (`in_place`, `helper`, `self-update` or `external`), `backup` (the pre-update backup's name), `actor`, `signed_by` (ID of the release key whose signature was verified, when known), `allow_unsigned` (`true`: installed with `aegis-daemon self-update --allow-unsigned`, without a signature check) |
| `UpdateFailed` | The upgrade stopped; unless `stage` is `restart`, nothing was replaced | `from_version`, `to_version`, `stage` (`download`, `signature` (release signature missing, invalid, by an untrusted key or for another release, or no key in this build), `verify`, `install`, `backup`, `request`, `helper`, `restart`), `error`, `actor` |

---

## 3. Projection Invalidation Rules
1. `ProjectCreated` creates or updates `ProjectState`.
2. `DeploymentQueued` → `Queued`, `DeploymentStarted` → `InProgress`, `DeploymentCompleted` → `Success`, `DeploymentFailed` → `Failed`, `DeploymentRolledBack` → `RolledBack`. `BuildStage*` events update the deployment's current stage.
3. `ReleaseCreated` → release `Built`. `ReleasePromoted` makes the release `Active`, marks the project's previously active release `Inactive`, records when it went live, and sets the project's current release. The time it went live, not build order, decides the default rollback target and which releases retention keeps. A built release whose deployment fails becomes `Failed`. `ReleaseArchived` → `Archived`.
4. `ProcessStarted` sets the project's current process (and makes it the only instance), unless it is a `candidate` or already one of the project's instances: then `ReleasePromoted` (via `process_id`, or `process_ids` for several instances) makes it current once it is healthy. `ReleasePromoted` and `InstanceSetChanged` set the project's live set of instances; the current process is the first. Process events update status, PID, port, instance number, exit code and desired state.
5. `DomainMapped` adds the domain to the project (removing it from any other project); `DomainUnmapped` removes it. On boot, the proxy's routes are rebuilt from the projects' domains and the ports of their live instances.
6. Events are applied in insertion order (`seq`), synchronously as they are stored, and replayed in the same order on startup.
