# ADR 0002: SQLite First Persistence

## Status
Accepted

## Context
Aegis aims to provide a zero-config, terminal-first deployment platform that developers can install on any fresh VPS and have running in under 10 minutes.
Requiring external databases (like PostgreSQL or MySQL) or caching servers (like Redis) adds significant installation, maintenance, and security overhead. It also violates our core principle of "Convention over configuration".

## Decision
We choose SQLite as the default, built-in storage engine.
* **SQLx Integration**: We will use SQLx with the SQLite driver to provide async-safe queries, connection pooling, and standard SQL operations.
* **Auto-Migrations**: The database migrations will be embedded in the daemon executable using SQLx macro utilities (`sqlx::migrate!`), executing automatically on startup.
* **Zero Dependency**: Developers do not need to install an external database server. The database is represented as a single file on disk (e.g. `aegis.db`).

## Consequences
* **Simplicity**: Extremely simple local deployments and testing.
* **Performance**: SQLite handles concurrent reads and serial writes exceptionally well for process management and single-host deployment scales, especially when WAL (Write-Ahead Logging) mode is active.
* **Portability**: Database files are trivial to back up, restore, or copy for troubleshooting.
* **Scalability**: If the platform scales to enterprise multi-server clusters, the event store and data repository layers can be configured with an optional PostgreSQL backend.
