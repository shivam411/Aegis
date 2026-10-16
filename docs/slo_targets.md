# Aegis Service Level Objectives (SLOs) & Performance Targets

This document sets the performance targets for Aegis, grouped into **Product**, **Runtime** and **Developer** SLOs.

> **Status: these are targets, not measurements.** None of them is measured or enforced in CI yet. CI compiles the benchmarks but doesn't run them. The "Planned gate" column says how each target should be enforced once measured. Results go in a report based on [`performance-report.md`](../validation/_templates/performance/performance-report.md), and the [scorecard](scorecard.md) is updated when they exist.

---

## 1. Product SLOs (First-Run Experience)

| Metric | Target | Measurement Method | Planned gate |
| :--- | :--- | :--- | :--- |
| **Install to first deploy** | `< 5 minutes` | End-to-end VPS onboarding walkthrough | Manual / E2E |
| **Runtime detection** | `< 250 ms` | Indicator file scan and detector pipeline | CI (hard) |
| **Config generation (`aegis.toml`)** | `< 100 ms` | Config synthesis duration | CI (hard) |

---

## 2. Runtime SLOs (Supervision Engine)

| Metric | Target | Measurement Method | Planned gate |
| :--- | :--- | :--- | :--- |
| **Daemon cold startup** | `< 500 ms` | Time from binary launch to gRPC server bind | CI (hard) |
| **Process crash restart** | `< 2.0 s` | Time from process exit to re-spawn (first backoff step is 1 s) | CI (hard) |
| **Health probe execution** | `< 100 ms` | HTTP/TCP probe latency | CI (hard) |
| **Event propagation latency** | `< 10 ms` | Time from `EventBus.publish()` to receiver | CI (hard) |

---

## 3. Developer SLOs (CLI & Tooling)

| Metric | Target | Measurement Method | Planned gate |
| :--- | :--- | :--- | :--- |
| **CLI command response** | `< 100 ms` | Latency of `aegis status / list / events` | CI (hard) |
| **`aegis doctor`** | `< 500 ms` | Full environment and daemon check | CI (hard) |
| **`aegis validate`** | `< 500 ms` | Config check duration | CI (hard) |
| **Terminal UI refresh** | `60 FPS (~16.6 ms/frame)` | Frame draw duration | Soft. Applies once the `ratatui` TUI exists (roadmap Phase 6). |

---

## 4. Benchmarks
Criterion benchmarks live in `benchmarks/benches/event_throughput.rs` (event bus publish and projection folding). CI only checks that they compile (`cargo check --benches`). Running them in CI and failing on regressions is still to do.
