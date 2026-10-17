# Aegis Long-Running Soak Testing Protocol

This document defines how to soak-test Aegis to check long-term stability under continuous load.

> **Status: short soaks run in CI; the long ones below haven't been run.** [`scripts/soak.sh`](../scripts/soak.sh) implements most of this protocol (steady load through the proxy, deploys, rollbacks, restarts, limit changes, daemon restarts; RSS, fds, threads, zombies, database and log size). It runs for 5 minutes on every push to `main` and for 60 minutes nightly; see [`benchmarks.md`](benchmarks.md) for how it judges a run and how to read its report. The 24 h / 72 h / 168 h runs need a dedicated host. Not covered yet: failing builds and unhealthy releases mixed into the load, `kill -9` of the app, daemon idle CPU, and gRPC `GetStatus` latency.

---

## 1. Test Durations & Milestones

| Duration | Goal | Acceptance Criteria |
| :--- | :--- | :--- |
| **24 Hours** | Short-term stability | No daemon crashes, daemon RSS growth `< 5%` |
| **72 Hours** | Medium-term reliability | No zombie processes, event propagation latency `< 10 ms` |
| **168 Hours (7 Days)** | Release readiness | No scheduler drift, database and WAL size bounded, health checks accurate |

---

## 2. Monitored Metrics
Record these continuously during the run:
1. **Resident set size (RSS):** the daemon and each supervised app.
2. **CPU:** daemon idle CPU must stay `< 1%`.
3. **Zombie processes:** count from the process tree (`ps`).
4. **SQLite storage:** database and WAL file sizes must stay bounded.
5. **API responsiveness:** gRPC `GetStatus` and HTTP `/healthz` latency.

## 3. Suggested workload
- Two or more apps (for example `examples/node-app`) with health checks.
- A deploy every few minutes, with some failing builds and unhealthy releases mixed in.
- Periodic `kill -9` of an app process and occasional daemon restarts.
- Resource-limit changes through the API on Linux.
