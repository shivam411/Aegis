# Aegis Compatibility Report (Template)

> **Template — results below are illustrative, not measured.**
> No compatibility run has been recorded yet. An earlier version of this file marked every cell PASS without a test behind it (see git history). To publish a real report, deploy a sample app per stack on each OS with a checked-in script, fill in every `TBD`, link the script and CI job, and save the filled-in copy as `validation/compatibility/compatibility-report.md`.

What CI covers today: `cargo test` and the deploy scenarios run on Ubuntu only, using a Node.js app (`examples/node-app`). macOS and Windows are only built, not tested. Detector unit tests cover file-based detection for Rust, Node.js and a few other runtimes, but don't build or run apps.

---

## 0. Run details

| Field | Value |
| :--- | :--- |
| Date | `TBD` |
| Aegis commit | `TBD` |
| Script | `TBD` |
| CI job / log | `TBD` |

## 1. Stack × OS matrix

Each cell means: detected, built, started, passed its health check, rolled back.

| Application stack | Linux | macOS | Windows |
| :--- | :---: | :---: | :---: |
| **Rust (Cargo)** | `TBD` | `TBD` | `TBD` |
| **Node.js (npm/yarn/pnpm)** | `TBD` | `TBD` | `TBD` |
| **Java (Gradle/Maven)** | `TBD` | `TBD` | `TBD` |
| **Go (go.mod)** | `TBD` | `TBD` | `TBD` |
| **Python (pip/poetry)** | `TBD` | `TBD` | `TBD` |
| **Docker (Dockerfile)** | `TBD` | `TBD` | `TBD` |
