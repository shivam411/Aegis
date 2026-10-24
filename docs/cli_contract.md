# Aegis CLI Interface Stability Contract

This document establishes the interface stability tier for every `aegis-cli` command.

Commands classified as **Stable** are guaranteed backwards-compatible public interfaces. Renaming or breaking stable CLI commands is strictly prohibited.

---

## 1. CLI Command Stability Matrix

| Command | Stability Tier | Purpose / Description |
| :--- | :--- | :--- |
| **`aegis status`** | `[STABLE]` | Query daemon health, version, loaded plugins, and event count. |
| **`aegis init`** | `[STABLE]` | Initialize project with runtime auto-detection or template (`--template`). |
| **`aegis deploy`** | `[STABLE]` | Build, health-check and deploy, with automatic rollback. |
| **`aegis rollback`** | `[STABLE]` | Revert project to a previous release version. |
| **`aegis list`** | `[STABLE]` | List active projects and monitored processes. |
| **`aegis logs`** | `[STABLE]` | Tail process logs with search and follow filters. |
| **`aegis stop`** | `[STABLE]` | Send graceful termination signal to process. |
| **`aegis restart`** | `[STABLE]` | Restart managed process. |
| **`aegis delete <app>`** | `[EXPERIMENTAL]` | Delete an app: stops its processes (draining), removes its domains, secrets, deploy key, webhook secret, schedule and releases; the history stays and the name and domains are free again. Asks to confirm in a terminal; `--yes` skips the question (required when not in a terminal). `--keep-releases` leaves the release directories and logs on disk. Refused while a deployment is running. |
| **`aegis source <app>`** | `[EXPERIMENTAL]` | Change an app's source: `--repo URL [--branch B]` or `--dir PATH` (relative to the current directory), plus `--subdir S` for a monorepo folder. Validated like registration; used from the next deployment. |
| **`aegis events`** | `[STABLE]` | Stream operational domain events in real-time. |
| **`aegis doctor`** | `[STABLE]` | Environment, gRPC, and SQLite diagnostic check (`--fix`). |
| **`aegis validate`** | `[STABLE]` | Validate project configuration and pipeline parameters. |
| **`aegis inspect`** | `[EXPERIMENTAL]` | Project state (releases, process, schedule, settings, limits, recent deployments) replayed from events; `--at <RFC 3339>` shows it as of a past time. |
| **`aegis explain`** | `[STABLE]` | Display runtime auto-detection rules. |
| **`aegis demo`** | `[STABLE]` | Interactive 2-minute feature tour. |
| **`aegis releases`** | `[STABLE]` | View immutable release history. |
| **`aegis timeline`** | `[STABLE]` | Display event-sourced execution timeline. |
| **`aegis incident`** | `[EXPERIMENTAL]` | Process status, crashes, failed deployments and resource alerts in a window (`--since 24h`), last output, and concrete advice such as the rollback command. |
| **`aegis investigate`** | `[EXPERIMENTAL]` | Timeline of a project's events in a window (`--since`) with the change that most likely triggered the first problem. |
| **`aegis replay`** | `[EXPERIMENTAL]` | A deployment's events step by step with time offsets, stage durations, failure reason and log tail. Takes a deployment id, release version or release id. |
| **`aegis migrate pm2` / `systemd`** | `[EXPERIMENTAL]` | Generate aegis.toml (name, start command, port, `[env]`, restart policy) from a PM2 ecosystem file (JSON; `.js` best effort) or a systemd unit. `--dry-run`, `--force`. |
| **`aegis upgrade`** | `[EXPERIMENTAL]` | `--check` compares with the latest GitHub release; otherwise, after confirmation (`--yes`), installs it over the binaries next to the CLI with the daemon's self-update code: the release signature (release key) and SHA-256 must verify. `--allow-unsigned` skips the signature requirement (warns). Not supported on Windows. |
