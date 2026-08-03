# Aegis Service Level Objectives (SLOs) & Performance Standard

This document establishes the official Service Level Objectives (SLOs) categorized into **Product SLOs**, **Runtime SLOs**, and **Developer SLOs**.

---

## 1. Product SLOs (Customer First-Run Experience)

| Metric | Target | Measurement Method | CI Regression Gate |
| :--- | :--- | :--- | :--- |
| **Install to First Deploy** | `< 5 minutes` | End-to-end VPS onboarding walkthrough | Manual / E2E Gate |
| **Runtime Detection** | `< 250 ms` | Indicator file scan & detector pipeline | Hard Gate |
| **Config Generation (`aegis.toml`)** | `< 100 ms` | Zero-boilerplate config synthesis duration | Hard Gate |

---

## 2. Runtime SLOs (Production Supervision Engine)

| Metric | Target | Measurement Method | CI Regression Gate |
| :--- | :--- | :--- | :--- |
| **Daemon Cold Startup** | `< 500 ms` | Time from binary launch to gRPC server bind | Hard Gate |
| **Process Crash Restart** | `< 2.0 s` | Time from process exit to supervisor re-spawn | Hard Gate |
| **Health Probe Execution** | `< 100 ms` | HTTP/TCP probe latency | Hard Gate |
| **Event Propagation Latency** | `< 10 ms` | Time from `EventBus.publish()` to receiver | Hard Gate |

---

## 3. Developer SLOs (Interactive CLI & Tooling)

| Metric | Target | Measurement Method | CI Regression Gate |
| :--- | :--- | :--- | :--- |
| **CLI Command Response** | `< 100 ms` | Execution latency of `aegis status / list / events` | Hard Gate |
| **`aegis doctor` Execution** | `< 500 ms` | Full environment & daemon check duration | Hard Gate |
| **`aegis validate` Execution** | `< 500 ms` | Pipeline readiness & config check duration | Hard Gate |
| **TUI Dashboard Refresh Rate** | `60 FPS (~16.6 ms/frame)` | Render frame draw duration | Soft Gate |

---

## 4. Benchmark Integration
Performance benchmarks located in `benchmarks/benches/event_throughput.rs` are executed in CI to track latency drift over time.
