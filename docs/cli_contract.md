# Aegis CLI Interface Stability Contract

This document establishes the interface stability tier for every `aegis-cli` command.

Commands classified as **Stable** are guaranteed backwards-compatible public interfaces. Renaming or breaking stable CLI commands is strictly prohibited.

---

## 1. CLI Command Stability Matrix

| Command | Stability Tier | Purpose / Description |
| :--- | :--- | :--- |
| **`aegis status`** | `[STABLE]` | Query daemon health, version, loaded plugins, and event count. |
| **`aegis init`** | `[STABLE]` | Initialize project with runtime auto-detection or template (`--template`). |
| **`aegis deploy`** | `[STABLE]` | Trigger build and zero-downtime deployment. |
| **`aegis rollback`** | `[STABLE]` | Revert project to a previous release version. |
| **`aegis list`** | `[STABLE]` | List active projects and monitored processes. |
| **`aegis logs`** | `[STABLE]` | Tail process logs with search and follow filters. |
| **`aegis stop`** | `[STABLE]` | Send graceful termination signal to process. |
| **`aegis restart`** | `[STABLE]` | Restart managed process. |
| **`aegis events`** | `[STABLE]` | Stream operational domain events in real-time. |
| **`aegis doctor`** | `[STABLE]` | Environment, gRPC, and SQLite diagnostic check (`--fix`). |
| **`aegis validate`** | `[STABLE]` | Validate project configuration and pipeline parameters. |
| **`aegis inspect`** | `[STABLE]` | Detailed resource graph hierarchy inspection. |
| **`aegis explain`** | `[STABLE]` | Display runtime auto-detection rules. |
| **`aegis demo`** | `[STABLE]` | Interactive 2-minute feature tour. |
| **`aegis releases`** | `[STABLE]` | View immutable release history. |
| **`aegis timeline`** | `[STABLE]` | Display event-sourced execution timeline. |
| **`aegis incident`** | `[EXPERIMENTAL]` | Operational incident diagnostic and auto-rollback advisor. |
