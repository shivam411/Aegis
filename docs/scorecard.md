# Aegis Scorecard

What is verified today, and what isn't measured yet.

Everything in "Verified by CI" runs on every push and pull request to `main` and `develop`, in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml). Nothing on this page is generated; it is edited by hand when CI or a published report changes. There are no production, dogfooding, soak or benchmark numbers yet. Earlier local drafts of this page showed such numbers, but they did not come from any real run.

---

## 1. Verified by CI

All CI jobs run on GitHub-hosted runners. Tests run on Ubuntu only; macOS and Windows are built but not tested.

| Check | What it shows | Where |
| :--- | :--- | :--- |
| `cargo fmt --check`, `cargo clippy -D warnings` | Formatting and lints are clean | `check-and-test` job |
| `cargo test --workspace --all-targets` | ~120 unit and integration tests pass, including the deploy lifecycle against real processes (deploy, redeploy, failed build, unhealthy release → automatic rollback, manual rollback, retention, daemon restart) | [`core/control/tests/lifecycle.rs`](../core/control/tests/lifecycle.rs), [`core/web/tests/`](../core/web/tests/), unit tests in each crate |
| cgroup tests as root | CPU cap on a busy loop, live limit changes, OOM kill and restart, against the runner's kernel (cgroup v2) | [`runtime/resources/tests/kernel.rs`](../runtime/resources/tests/kernel.rs), [`core/web/tests/resources.rs`](../core/web/tests/resources.rs) |
| `cargo check --benches` | The Criterion benchmarks compile (they are **not run**) | [`benchmarks/benches/event_throughput.rs`](../benchmarks/benches/event_throughput.rs) |
| Release build on Linux, macOS, Windows | The workspace compiles on all three | `cross-platform-build` job |
| End-to-end script | The real `aegis-daemon` and `aegis-cli` binaries deploy `examples/node-app`, restart, stop/start, reject a broken build, roll back an unhealthy release, roll back manually, survive daemon restarts, and serve the authenticated HTTP API; unsafe configs are refused | [`scripts/e2e-smoke.sh`](../scripts/e2e-smoke.sh) |
| Playwright browser tests | Sign-in, new-app wizard, live deploy pipeline, logs, process control, rollback, timeline, audit log, API tokens, CPU/memory sliders (busy loop capped, OOM kill restarted), phone-sized viewport; any CSP violation or script error fails the run | [`ui-tests/tests/`](../ui-tests/tests/) |

## 2. Not yet measured

| Claim | Status | How to measure it |
| :--- | :--- | :--- |
| Aegis deploys Aegis (dogfooding) | Not yet measured. Aegis doesn't deploy itself yet. | Run the pipeline in [`dogfooding.md`](dogfooding.md) on a real host; publish counts exported from the event store. Template: [`dogfood-report.md`](../validation/_templates/dogfood/dogfood-report.md). |
| Deployment success rate in real use | Not yet measured | Same as above, over a stated period. |
| Long-running stability (24 h / 72 h / 168 h soak) | Not yet measured | Script the protocol in [`soak_testing.md`](soak_testing.md); record daemon RSS, CPU, zombies and database size over time. Template: [`soak-report.md`](../validation/_templates/soak/soak-report.md). |
| Chaos scenarios | Partly covered. Build failure, unhealthy-release rollback, crash-loop detection and malformed or unsafe config are tested in CI (see [`chaos_testing.md`](chaos_testing.md)); the rest are not. | Add a script for the remaining scenarios. Template: [`chaos-report.md`](../validation/_templates/chaos/chaos-report.md). |
| PM2 / systemd migration accuracy | Not yet measured. The `migrate` commands are minimal. | Check in a corpus of real config files and a conversion test. Template: [`migration-report.md`](../validation/_templates/migration/migration-report.md). |
| SLO latencies (startup, CLI, event propagation, restart, memory) | Not yet measured | Run `cargo bench` plus a timing script on a stated host; add a CI job that fails on regressions. Template: [`performance-report.md`](../validation/_templates/performance/performance-report.md). |
| Cross-platform and multi-language deploys | Not yet measured beyond Node.js on Ubuntu | Deploy one sample app per stack on each OS in CI. Template: [`compatibility-report.md`](../validation/_templates/compatibility/compatibility-report.md). |
| External users in production | None recorded | See [`beta_program.md`](beta_program.md). |

When a real report is published, move it out of `validation/_templates/`, link the script and CI run that produced it, and update this page.
