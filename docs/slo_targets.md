# Aegis Service Level Objectives (SLOs) & Performance Standard

This document establishes the official Service Level Objectives (SLOs) and latency targets for Aegis. Every build, feature, and architectural change must satisfy these regression boundaries.

---

## 1. Core Platform SLO Performance Targets

| Operational Metric | Target Latency / Threshold | Measurement Method | CI Regression Gate |
| :--- | :--- | :--- | :--- |
| **Daemon Cold Startup** | `< 500 ms` | Time from binary launch to gRPC server bind | Hard Gate |
| **CLI Command Response** | `< 100 ms` | Execution latency of `aegis status / list` | Hard Gate |
| **Event Propagation Latency** | `< 10 ms` | Time from `EventBus.publish()` to receiver | Hard Gate |
| **TUI Dashboard Refresh Rate** | `60 FPS (~16.6 ms/frame)` | Render frame draw duration | Soft Gate |
| **Runtime Auto-Detection** | `< 250 ms` | Directory indicator scan duration | Hard Gate |
| **Health Probe Execution** | `< 100 ms` | Endpoint HTTP/TCP probe latency | Hard Gate |
| **Process Crash Restart** | `< 2.0 s` | Time from process exit to supervisor re-spawn | Hard Gate |
| **Rollback Execution Trigger** | `< 5.0 s` | Atomic version directory swap duration | Hard Gate |
| **Artifact Store Indexing** | `< 50 ms` | Metadata read & checksum verification | Hard Gate |

---

## 2. Benchmark Integration
Performance benchmarks located in `benchmarks/benches/event_throughput.rs` are executed in CI to track latency drift over time.
