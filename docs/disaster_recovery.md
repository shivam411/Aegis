# Aegis Disaster Recovery & Fault Tolerance Guide

This document defines disaster recovery procedures, failure recovery objectives, and manual fallback steps for Aegis.

---

## 1. Disaster Recovery Matrix

| Incident Scenario | Expected Automated Behavior | Manual Recovery Protocol | Recovery Time Objective (RTO) |
| :--- | :--- | :--- | :---: |
| **Daemon DB Corruption** | SQLite WAL checksum failure detected | Restore state replay from immutable event store backup (`aegis-event-store.db`) | `< 60 seconds` |
| **Host Reboot Mid-Deployment** | Process supervisor detects interrupted deployment state on startup | Run `aegis rollback` or `aegis deploy` to reactivate prior active symlink | `< 30 seconds` |
| **Artifact Storage Loss** | Verification stage detects missing artifact package | Re-trigger build stage via `aegis deploy` to re-package release artifact | `< 120 seconds` |
| **Failed Migration** | Parser halts with validation error | Original systemd/PM2 configuration left untouched; run `aegis validate` | `< 10 seconds` |
| **Power Loss Outage** | Daemon performs WAL log recovery on boot | Process supervisor restarts active release PIDs automatically | `< 15 seconds` |

---

## 2. Emergency Manual Fallback
If the Aegis daemon is completely unreachable, managed application binaries remain intact in release storage directories (`~/.aegis/releases/<project_id>/<release_id>/`). They can be executed directly via terminal.
