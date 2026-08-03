# Aegis Persistence Versioning & Schema Migration Specification

This document defines the schema versioning policy and forward/backward compatibility guidelines for the Aegis SQLite event store, domain events, release manifests, and configuration files.

---

## 1. Schema Versioning Principles

1. **Explicit Schema Version Field**: Every persisted entity (`Event`, `Release`, `ProjectState`, `StoredArtifact`) must include a `schema_version: u32` field.
2. **Append-Only Event Store**: SQLite `events` table rows are strictly immutable. Upgrading event structures must use additive fields or schema migration adapters rather than in-place mutation.
3. **Automatic Replay Compatibility**: The `ProjectionEngine` must be capable of processing historic `schema_version = 1` events alongside newer `schema_version = N` events.

---

## 2. SQLite Database Schema & Migrations

The `events` database table includes schema tracking:

```sql
CREATE TABLE IF NOT EXISTS events (
    id TEXT PRIMARY KEY,
    schema_version INTEGER NOT NULL DEFAULT 1,
    event_type TEXT NOT NULL,
    aggregate_type TEXT NOT NULL DEFAULT 'System',
    aggregate_id TEXT NOT NULL DEFAULT '',
    correlation_id TEXT,
    causation_id TEXT,
    payload_json TEXT NOT NULL,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL
);
```

### Database Migration Rules
* SQL migrations are managed directly via embedded `sqlx::migrate!`.
* Schema modifications must be backward-compatible (e.g. `ADD COLUMN` with defaults).

---

## 3. Persistent Object Versions

### `Release` Manifest Versioning
Release metadata files (`metadata.json`) stored in `ArtifactStore` include:

```json
{
  "schema_version": 1,
  "release_id": "01910a3b-7f12-7890-8b01-123456789abc",
  "version": "v1.2.0",
  "commit_sha": "a1b2c3d4",
  ...
}
```

### Event Upcasting (Migration Strategy)
When deserializing older domain events:
1. `schema_version == 1`: Deserialized with default values for newly added fields.
2. `schema_version == N`: Directly parsed into target Rust struct.
