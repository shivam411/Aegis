# Aegis Chaos Test Report (Template)

> **Template — results below are illustrative, not measured.**
> No chaos run has been recorded yet. An earlier version of this file showed made-up results (see git history). To publish a real report, run the scenarios with a checked-in script, fill in every `TBD`, link the script and CI job that produced it, and save the filled-in copy as `validation/chaos/chaos-report.md`.

Scenarios are defined in [`docs/chaos_testing.md`](../../../docs/chaos_testing.md).

---

## 0. Run details

| Field | Value |
| :--- | :--- |
| Date | `TBD` |
| Aegis commit | `TBD` |
| Host (OS, kernel, CPU, memory) | `TBD` |
| Script | `TBD` (path in this repo) |
| CI job / log | `TBD` (link) |

## 1. Results

| Scenario ID | Fault | Expected result | Observed result | Status |
| :--- | :--- | :--- | :--- | :---: |
| **CHAOS-01** | Process force kill (`kill -9`) | Auto-restart with backoff | `TBD` | `TBD` |
| **CHAOS-02** | Network outage (CLI ↔ daemon) | CLI retries; daemon keeps supervising | `TBD` | `TBD` |
| **CHAOS-03** | Disk full during build | Build aborts; live version untouched | `TBD` | `TBD` |
| **CHAOS-04** | Git remote timeout | Clone stage fails; live version untouched | `TBD` | `TBD` |
| **CHAOS-05** | Build command exits non-zero | Build stage fails; live version untouched | `TBD` | `TBD` |
| **CHAOS-06** | Health endpoint returns 500 | Automatic rollback | `TBD` | `TBD` |
| **CHAOS-07** | Corrupted artifact | Verify stage rejects the release | `TBD` | `TBD` |
| **CHAOS-08** | Malformed `aegis.toml` | Validation error; deploy blocked | `TBD` | `TBD` |
| **CHAOS-09** | Crash loop | Process marked `Failed` | `TBD` | `TBD` |
| **CHAOS-10** | Daemon killed during a release switch | State recovers on restart | `TBD` | `TBD` |

**Overall:** `TBD` / 10 passed.
