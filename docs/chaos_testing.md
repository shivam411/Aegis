# Aegis Chaos Engineering & Fault Injection Specification

This document defines the 10 fault-injection scenarios for Release Candidate gate RC2.

Chaos testing checks that Aegis recovers predictably when things fail, not just that the happy path works.

> **Status: no chaos run has been done yet.** Some scenarios are already covered by tests that run in CI; the "CI coverage" column says which. Results of a full run go in a report based on [`chaos-report.md`](../validation/_templates/chaos/chaos-report.md).

---

## 1. Fault Injection Scenarios

| ID | Fault | Injected failure | Expected behaviour | CI coverage today |
| :--- | :--- | :--- | :--- | :--- |
| **CHAOS-01** | Process termination | `kill -9` on the app's PID | Supervisor restarts it with exponential backoff and records the crash | Partly: crashes by exit are tested (`test_restart_policy_with_backoff_and_crash_loop`); `kill -9` is not |
| **CHAOS-02** | Network outage | CLI loses its connection to the daemon | CLI reports the error; daemon keeps supervising | None |
| **CHAOS-03** | Disk full | Storage fills up during a build | Build fails cleanly and the live release is untouched | None |
| **CHAOS-04** | Git remote unavailable | Clone/fetch times out or the host is unreachable | Clone stage fails and the live release is untouched | None |
| **CHAOS-05** | Build failure | Build command exits non-zero | Pipeline stops at the build stage; the live release keeps serving | Yes: `failed_build_leaves_live_release_untouched` in [`lifecycle.rs`](../core/control/tests/lifecycle.rs); [`e2e-smoke.sh`](../scripts/e2e-smoke.sh) |
| **CHAOS-06** | Health check failure | Health endpoint returns 500 or never answers | Automatic rollback to the previous release | Yes: `unhealthy_release_is_rolled_back_automatically`; `e2e-smoke.sh` |
| **CHAOS-07** | Corrupted artifact | Stored artifact files are modified | Checksum verification fails | Partly: tampering is detected in [`artifact_store`](../deployment/artifact_store/src/lib.rs) tests; rejection during a deploy is not tested |
| **CHAOS-08** | Malformed config | Invalid syntax or unsafe settings in `aegis.toml` | Validation error; the deploy or daemon start is blocked | Yes: [`core/config`](../core/config/src/lib.rs) tests; unsafe-config steps in `e2e-smoke.sh` |
| **CHAOS-09** | Crash loop | App exits immediately on every start | Supervisor marks the process `Failed` | Yes: `test_restart_policy_with_backoff_and_crash_loop` |
| **CHAOS-10** | Power loss | Daemon killed abruptly during a release switch | Event store recovers cleanly on restart; interrupted deployments are marked failed | Partly: daemon restarts are tested (`daemon_restart_restores_running_apps_only`), but not an abrupt kill mid-switch |
