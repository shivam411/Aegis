# Aegis Dogfooding Plan

This document describes the planned "Aegis deploying Aegis" pipeline.

> **Status: planned, not running.** Aegis does not deploy itself yet, and there are no dogfooding results. When it does, results go in a report based on [`dogfood-report.md`](../validation/_templates/dogfood/dogfood-report.md).

---

## 1. Planned Pipeline

```
GitHub push (main branch)
       ↓
GitHub Actions CI build (Linux amd64 / arm64)
       ↓
Publish Aegis release artifacts
       ↓
Signed webhook to a live Aegis daemon
       ↓
Aegis builder: package and verify the Aegis binary
       ↓
Health check of the new daemon
       ↓
Switch to the new release
       ↓
Graceful drain and restart of the daemon process
```

The webhook endpoint, builder, health checks and release switch exist and are tested in CI (see [`feature_maturity.md`](feature_maturity.md)). What's missing is a host running this pipeline, and a safe way for a daemon to replace itself (planned with self-update in [roadmap](roadmap_web_control_plane.md) Phase 6).

---

## 2. Why dogfood
1. **Fast feedback:** problems with signal handling, log streaming or config reloading show up internally before users hit them.
2. **Real load:** it exercises event persistence and live streams during real upgrades.
