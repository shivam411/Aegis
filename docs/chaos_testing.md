# Aegis Chaos Engineering & Fault Injection Specification

This document defines the 10 fault-injection test scenarios for Aegis Release Candidate Gate RC2.

Rather than validating nominal success paths, Aegis chaos testing verifies predictable recovery during system failures.

---

## 1. Fault Injection Scenarios

| Scenario ID | Fault Type | Injected Failure | Expected System Behavior |
| :--- | :--- | :--- | :--- |
| **CHAOS-01** | Process Termination | `kill -9` or Taskkill force kill on active PID | Supervisor detects crash, triggers exponential backoff restart, emits `ProcessCrashed` event |
| **CHAOS-02** | Network Outage | Simulated drop of gRPC network connection | CLI retries with exponential backoff, daemon continues background supervision |
| **CHAOS-03** | Disk Space Saturation | Full storage disk during build stage | Builder aborts gracefully, emits `BuildFailed`, live version remains untouched |
| **CHAOS-04** | Git Remote Unavailable | Git clone/fetch timeout or unreachable host | Builder stage 1 fails cleanly, live traffic uninterrupted |
| **CHAOS-05** | Build Failure | Non-zero exit code during build command | Pipeline stops at Stage 3, emits `BuildStageBuild Failed`, release omitted |
| **CHAOS-06** | Health Probe Timeout | HTTP/TCP endpoint returning 500 or timing out | Strategy detects unhealthy target, triggers automatic rollback, live version active |
| **CHAOS-07** | Corrupted Artifact | Invalid SHA256 checksum in stored artifact | Verification stage fails at Stage 6, deployment rejected |
| **CHAOS-08** | Malformed Config | Invalid syntax or missing required fields in `aegis.toml` | `aegis validate` returns validation error (`AEGIS_1001`), deployment blocked |
| **CHAOS-09** | Restart Crash Loop | Process exiting immediately 5 consecutive times | Supervisor transitions process to `Stopped`, raises `ProcessFailedPermanent` alert |
| **CHAOS-10** | Power Interruption | Abrupt daemon termination during version swap | SQLite WAL log recovers event store state cleanly on restart |
