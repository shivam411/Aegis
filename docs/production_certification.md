# Aegis Production Certification Suite

This document defines the 10-point Production Certification Suite for Aegis. Every release tag must satisfy 100% of these operational verification scenarios before release artifacts are published.

---

## 1. Operational Verification Scenarios

| Scenario ID | Test Scenario | Operational Requirement | Gate Status |
| :--- | :--- | :--- | :--- |
| **CERT-01** | Process Start | Spawns managed child process and records valid PID | Hard Gate |
| **CERT-02** | Graceful Stop | Sends SIGTERM (or Taskkill), drains connections, and stops cleanly | Hard Gate |
| **CERT-03** | Forced Kill | Escalates to SIGKILL if process fails to terminate within drain window | Hard Gate |
| **CERT-04** | Crash Recovery | Auto-restarts crashed process with exponential backoff | Hard Gate |
| **CERT-05** | Health Verification | Executes HTTP/TCP health probes and updates process status | Hard Gate |
| **CERT-06** | Log Capture | Piped stdout/stderr streams captured into ring log buffers | Hard Gate |
| **CERT-07** | Event Ordering | Events published to `EventBus` arrive in strict chronological order | Hard Gate |
| **CERT-08** | Release History | Immutable releases recorded in SQLite store and accessible via CLI | Hard Gate |
| **CERT-09** | Atomic Rollback | `GracefulSwitchStrategy` restores prior active version directory | Hard Gate |
| **CERT-10** | Config Reload | Configuration updates merge deterministically across 5 tiers | Hard Gate |

---

## 2. Release Gate Automation
The certification suite is executed automatically in CI via `.github/workflows/ci.yml`.
