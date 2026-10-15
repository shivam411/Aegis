# Aegis Production Certification Suite

This document defines 10 operational scenarios that every release should pass, and shows which tests cover each one today.

> **Status:** the tests below run in CI on every push and pull request to `main` and `develop` (Ubuntu only). The release workflow ([`release.yml`](../.github/workflows/release.yml)) builds binaries but does not itself run these tests or block on them yet. Windows behaviour (for example `taskkill`) is not tested.

---

## 1. Scenarios

| ID | Scenario | Requirement | Covered by |
| :--- | :--- | :--- | :--- |
| **CERT-01** | Process start | Spawns the app and records its real PID | `test_spawn_stop_and_events` in [`runtime/process`](../runtime/process/src/lib.rs); [`lifecycle.rs`](../core/control/tests/lifecycle.rs) |
| **CERT-02** | Graceful stop | Sends SIGTERM to the process group and waits for it to exit | `test_spawn_stop_and_events`, `test_stop_kills_whole_process_group` |
| **CERT-03** | Forced kill | Escalates to SIGKILL after the drain timeout | `test_sigterm_ignoring_process_is_killed_after_drain` |
| **CERT-04** | Crash recovery | Restarts a crashed app with exponential backoff; marks a crash loop `Failed` | `test_restart_policy_with_backoff_and_crash_loop` |
| **CERT-05** | Health verification | HTTP/TCP health checks gate cutover; unhealthy releases roll back | [`deployment/health`](../deployment/health/src/lib.rs) tests; `unhealthy_release_is_rolled_back_automatically` in `lifecycle.rs`; [`e2e-smoke.sh`](../scripts/e2e-smoke.sh) |
| **CERT-06** | Log capture | stdout/stderr captured to rotating per-process log files and streamed live | `test_logs_file_tail_and_live_stream`, `test_log_rotation` |
| **CERT-07** | Event ordering | Events are stored and replayed in strict `seq` order | `test_replay_follows_insertion_order_and_listeners_run` in [`core/event_store`](../core/event_store/src/lib.rs) |
| **CERT-08** | Release history | Releases recorded in SQLite and listed by the CLI | `lifecycle.rs`; `aegis releases` in `e2e-smoke.sh` |
| **CERT-09** | Rollback | Switches back to a previous release without rebuilding, in live (not build) order | `deploy_redeploy_process_control_and_rollback`, `rollback_and_retention_follow_live_order_not_build_order`; `e2e-smoke.sh` |
| **CERT-10** | Configuration | Config files and overrides merge deterministically; invalid or unsafe configs are refused | [`core/config`](../core/config/src/lib.rs) tests; unsafe-config steps in `e2e-smoke.sh`. Live config reload is not tested. |

---

## 2. Release gate

Not automated yet. To make this a real gate, the release workflow should depend on the CI jobs (or re-run them) before publishing artifacts.
