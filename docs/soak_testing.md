# Aegis Long-Running Soak Testing Protocol

This document defines the soak testing protocol for Aegis to verify long-term stability under production workloads.

---

## 1. Test Durations & Milestones

| Duration | Goal | Acceptance Criteria |
| :--- | :--- | :--- |
| **24 Hours** | Short-Term Stability | Zero process crashes, zero memory leaks, RSS memory delta `< 5%` |
| **72 Hours** | Medium-Term Reliability | Zero zombie processes, event bus propagation latency `< 10ms` |
| **168 Hours (7 Days)** | Production Certification | Zero scheduler drift, SQLite WAL log bounded, 100% health probe accuracy |

---

## 2. Monitored System Metrics
During soak testing, the following metrics are recorded continuously:
1. **Resident Set Size (RSS) Memory**: Tracked for daemon process and managed child processes.
2. **CPU Utilization**: Idle CPU usage must remain `< 1%`.
3. **Zombie Process Count**: Monitored via OS process tree traversal (`ps` / Task Manager).
4. **SQLite WAL Storage**: Database file size must remain bounded.
5. **gRPC Channel Health**: Connection ping latency must remain `< 5ms`.
