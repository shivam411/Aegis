# Aegis Performance Report (Template)

> **Template — results below are illustrative, not measured.**
> No performance run has been recorded yet. An earlier version of this file listed measured results (see git history); none came from a checked-in benchmark. CI compiles the Criterion benchmarks in `benchmarks/benches/` (`cargo check --benches`) but doesn't run them. To publish a real report, run the benchmarks and timing script, fill in every `TBD`, link the script and CI job, and save the filled-in copy as `validation/performance/performance-report.md`.

Targets are defined in [`docs/slo_targets.md`](../../../docs/slo_targets.md).

---

## 0. Run details

| Field | Value |
| :--- | :--- |
| Date | `TBD` |
| Aegis commit | `TBD` |
| Host (OS, CPU, memory) | `TBD` |
| Build profile | `TBD` |
| Script / command | `TBD` |
| Samples per metric | `TBD` |

## 1. Results

| Metric | Target | Measured (p50 / p95) | Status |
| :--- | :--- | :--- | :---: |
| **Daemon startup** | `< 500 ms` | `TBD` | `TBD` |
| **CLI command latency** | `< 100 ms` | `TBD` | `TBD` |
| **Runtime detection** | `< 250 ms` | `TBD` | `TBD` |
| **Config generation** | `< 100 ms` | `TBD` | `TBD` |
| **Process restart** | `< 2.0 s` | `TBD` | `TBD` |
| **Health probe** | `< 100 ms` | `TBD` | `TBD` |
| **Event propagation** | `< 10 ms` | `TBD` | `TBD` |
| **Daemon idle memory (RSS)** | `< 50 MB` | `TBD` | `TBD` |
| **Daemon idle CPU** | `< 1 %` | `TBD` | `TBD` |
