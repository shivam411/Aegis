# Aegis Long-Running Soak Testing Protocol

This document defines how to soak-test Aegis to check long-term stability under continuous load.

> **Status: no soak test has been run yet.** There is no soak script in the repository. Results go in a report based on [`soak-report.md`](../validation/_templates/soak/soak-report.md), together with the script that produced them.

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
