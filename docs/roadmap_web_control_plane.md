# Roadmap: Aegis Web Control Plane

**Goal:** Aegis runs on a VPS and serves a web dashboard on a configurable port. From that page, an operator can manage everything they can manage today from the CLI, plus live resource control: deploy, roll back, start/stop/restart apps, read logs, watch CPU/memory, and change each app's CPU and memory limits with sliders.

**Status:** Phases 0–2 complete · Phase 3 next · **Target:** `v0.5.0` (web MVP), `v0.6.0` (resource control)

---

## 0. Where we are today (audit, 2026-09-28)

### What works

| Check | Result |
| :--- | :--- |
| `cargo build --workspace --all-targets` | ✅ passes |
| `cargo fmt --all -- --check` | ✅ passes |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ passes |
| `cargo test --workspace --all-targets` | ✅ all pass |
| CI smoke flow (`daemon` → `init` → `deploy` → `status`) | ✅ passes after the fixes below |

The foundation is sound and worth building on: the event store, event bus, projection engine, scheduler, detector pipeline, release switcher, and gRPC API all work and have tests.

### Bugs fixed as part of this roadmap

| Bug | Impact | Fix |
| :--- | :--- | :--- |
| The daemon wrote a daemon-only `aegis.toml` into its working directory on startup | `aegis init` saw an "existing" config and skipped writing `[project]`, so **every `aegis deploy` invented a new random project ID**. The CI smoke test hit this. | The daemon no longer creates `aegis.toml`. `init` appends `[project]` to an existing file that lacks one. `[daemon]` is now optional in `aegis.toml`. |
| `ProcessStarted` always recorded `"pid": 9000` | Wrong PID in the audit trail and projections | Records the real OS PID |
| The daemon reported a hardcoded version `"0.1.0"` | Version drifts from the release | Uses `CARGO_PKG_VERSION` |

### Simulated or stubbed behaviour found in the audit

These passed tests but didn't do what the docs and CLI claimed. Phase 0 fixed them; each row says how.

| Area | Before | After Phase 0 |
| :--- | :--- | :--- |
| Deploy saga starts the app | Spawned `echo Deploy success` | Runs `build.start_command` from the release's `aegis.toml`, supervised, with `PORT` and `[env]` set |
| Health checks | Never ran; the target was hardcoded to `127.0.0.1:8080` | `deploy.health_check_url` (HTTP path, `tcp://`, or "stays up") is polled until `health_check_timeout_secs` |
| Previous release / drain | `previous_release` was always `None` | The old process is stopped with SIGTERM and a drain timeout; if the new release is unhealthy, the old one is started again |
| Release versions | Every deploy was `v1.0.0` | Unique, sortable versions (`20260928-051203`) or `--release <name>`; names are validated so they can't escape the releases directory |
| Build stages | Clone and Test only logged | Git clone or git-aware copy; install/build/test commands with timeouts and a build log per deployment |
| Artifact store | Hashed the path string and didn't copy directories | Hashes file contents; copies directory trees; checksum verification detects tampering |
| `stop` / `restart` / `rollback` | Appended events nothing acted on | Typed commands that the daemon executes, recording both the request and the outcome |
| Restart policy | Restarted once, then gave up | Exponential backoff (1 s → 30 s); crash-loop detection marks the process `Failed` |
| Process logs | Only in the daemon's tracing output | Per-process log files (rotated at 10 MiB), `aegis logs [-f]` |
| Event ordering | Replayed by `created_at` text | Explicit `seq` column (migrated); the projection is updated synchronously in `seq` order |
| Daemon restart | Apps were orphaned (SIGTERM wasn't handled) and never restarted | SIGTERM/SIGINT stop apps cleanly; on boot, apps the operator didn't stop are restarted; interrupted deployments are marked failed |
| Runtime trait | `start`/`stop`/`health` returned fake PIDs | Reduced to runtime detection; the supervisor owns process lifecycle |
| CLI | Required the daemon even for `init`/`doctor`; `list`, `releases` and `timeline` showed only event counts | Offline commands work without the daemon; the list/query commands show real data |

### Still open after Phase 0 (tracked in later phases)

- **Cutover has a brief gap.** Without a reverse proxy, the old and new process can't share a port, so there is a gap between stopping the old process and the new one accepting connections. The gap lasts as long as the app takes to start listening: about 0.5 s for `examples/node-app`, and several seconds for slow-starting apps such as JVM services. It will be removed by the Phase 5 proxy, which also enables `Rolling`/`BlueGreen`.
- ~~**Superseded crates.**~~ `deployment/strategy` and `deployment/rollback` were removed in Phase 1.
- **Stub crates.** `metrics`, `auth`, `secrets`, `ssh`, `state`, `analytics`, `action`, `docker` and `github` are still stubs; they are scheduled in Phases 2, 4 and 5.
- **Experimental CLI commands.** `replay`, `incident`, `investigate`, `inspect --at` and `upgrade` still print placeholder output.
- **No webhook endpoint yet.** A webhook HTTP endpoint needs the Phase 1 HTTP server.

---

## 1. Target architecture

The web control plane lives **inside the existing daemon** as a second listener. It keeps the single-binary, no-external-dependencies promise.

```
                       Browser (operator)
                              │  HTTPS :8420  (cookie session + CSRF)
                              ▼
┌───────────────────────── aegis-daemon ─────────────────────────┐
│                                                                │
│  ┌───────────────┐   ┌────────────────┐   ┌─────────────────┐  │
│  │ gRPC :50051   │   │ HTTP :8420     │   │ Webhook         │  │
│  │ (CLI / TUI)   │   │ REST + SSE +   │   │ POST /hooks/*   │  │
│  │ localhost     │   │ embedded UI    │   │ (HMAC-verified) │  │
│  └──────┬────────┘   └───────┬────────┘   └────────┬────────┘  │
│         └────────────┬───────┴─────────────────────┘           │
│                      ▼                                         │
│          ┌──────────────────────┐   single command/query API,  │
│          │  ControlPlane core   │   shared by every transport  │
│          └───┬──────────┬───────┘                              │
│   commands   │          │ queries                              │
│              ▼          ▼                                      │
│      Event Store ──► Projection Engine                         │
│         │                                                      │
│         ▼                                                      │
│   Deployment saga ─► Process Supervisor ─► cgroup v2 / systemd │
│                          │                     (CPU, memory)   │
│                          ▼                                     │
│                  Log store + Metrics sampler                   │
└────────────────────────────────────────────────────────────────┘
```

### Key decisions

| Decision | Choice | Why |
| :--- | :--- | :--- |
| HTTP server | `axum` inside the daemon (same Tokio runtime as `tonic`) | Same ecosystem as `tonic`; no second process |
| Business logic | Extract a `ControlPlane` service used by gRPC, HTTP and webhooks | Today, logic sits in the gRPC handler and a 200-line saga closure in `main.rs`; a third transport would duplicate it |
| Live updates | Server-Sent Events (SSE) for events, deploy progress, metrics and logs | One-directional, works through proxies, auto-reconnects, simpler than WebSockets |
| Frontend | Server-rendered HTML (`askama`) + htmx + a small vendored JS chart library (uPlot), embedded in the binary with `rust-embed` | Keeps the build Rust-only (no Node toolchain in CI or for source builds), so it stays a single binary. *Alternative:* a Vite + Svelte SPA if the UI outgrows htmx, at the cost of a Node build step. |
| Resource limits | cgroup v2 on Linux: one cgroup per managed app, with `cpu.max`, `memory.max` and `memory.high` | Limits can be changed **live, without restarting the app**, which is what the sliders need. Fallback: `systemd-run --scope` plus `systemctl set-property`. |
| Metrics | `sysinfo` crate for the host; `/proc/<pid>` and cgroup `cpu.stat` / `memory.current` for each app; in-memory ring buffer plus 1-minute rollups in SQLite | No Prometheus required. A `/metrics` Prometheus endpoint is optional later. |
| Config | New `[web]` section in `aegis.toml` | See §3 |

---

## 2. Phases

Each phase ends in a shippable, tested state. Acceptance criteria are what CI or a manual VPS test must show.

### Phase 0 — Make the core real ✅ *done*

Delivered:

1. **Real app start:** the saga reads `build.start_command`, the port, the health URL and `[env]` from the release's own `aegis.toml`.
2. **Real health checks,** with automatic rollback to the previous release when the new one isn't healthy in time.
3. **Release lineage:** unique versions, previous release tracked through `ReleasePromoted`, graceful drain, and retention pruning driven by release history.
4. **Daemon-executed commands:** `stop`, `start`, `restart` and `rollback` are typed gRPC commands. Requests (`Process*Requested`, `RollbackRequested`) and outcomes (`ProcessStopped`, `RollbackCompleted`, …) are both recorded.
5. **Supervisor:** process groups, backoff, crash-loop limit, SIGKILL escalation, and per-process rotating logs with live streaming.
6. **Builder:** git clone or git-aware copy, install/build/test commands with timeouts and logs, and content-hashed artifacts.
7. **Event store:** `seq` column with migration; ordered, synchronous projection updates.
8. **Read API:** `ListProjects`, `ListReleases`, `ListDeployments`, `GetDeployment`, `ListProcesses`, `GetLogs`, `StreamLogs` and `ListEvents`. These back the CLI now and will back the web API later.
9. **Refactor:** the saga moved to `core/control` (`ControlPlane`), which Phase 1 exposes over HTTP.

Evidence:

- 64 unit and integration tests. They include control-plane tests with real HTTP processes covering deploy, redeploy, failed build, unhealthy release, rollback, retention and daemon restart.
- [`scripts/e2e-smoke.sh`](../scripts/e2e-smoke.sh), run in CI, drives the real binaries through the same scenarios with `examples/node-app`.

### Phase 1 — HTTP API in the daemon ✅ *done*

Delivered:

- **`core/web` (`aegis-web`):** an `axum` HTTP/JSON API served by the daemon next to gRPC. It is enabled with `[web] enabled = true` and is **loopback-only**: the daemon refuses any other bind until Phase 2 adds authentication. Every handler calls `ControlPlane`, so gRPC and HTTP share one implementation.
- **Endpoints:** status; projects (list, create, get); deploy; rollback; schedule; releases; deployments (list, get); processes (list, start/stop/restart); logs; events. Server-Sent Events streams exist for **events** and **logs**, and streams end on daemon shutdown so they never block it. See [http_api.md](http_api.md).
- **Browser-attack protection for the local API:** a loopback `Host` check blocks DNS rebinding; an `Origin`/`Sec-Fetch-Site` check blocks cross-site requests; and state-changing requests require JSON, which forces a CORS preflight that is never granted.
- **OpenAPI 3:** generated with `utoipa`, served at `/api/v1/openapi.json`, and checked in as [`docs/openapi.json`](openapi.json). A test fails if the two drift apart.
- **Parity additions:** a typed `ConfigureSchedule` command (gRPC, HTTP and CLI) replaces raw event emission; schedules are visible on projects; `aegis status` shows the API URL.
- **Bug found by the new end-to-end checks and fixed:** after a rollback, both the default rollback target and retention used *build* order instead of *live* order. So `rollback` could pick the wrong release, and retention could delete the release that had just been live. Both now use `ReleasePromoted` order, with a regression test.
- **Cleanup:** the superseded `deployment/strategy` and `deployment/rollback` crates were removed; the daemon now warns when the (also unauthenticated) gRPC API is bound to a non-loopback address.

Evidence: 7 HTTP API tests (guard rules, validation and errors, a full deploy → restart → stop/start → logs → rollback lifecycle against a real app, SSE filtering, shutdown ending streams, log follow) and new HTTP steps in `scripts/e2e-smoke.sh` (run in CI).

### Phase 2 — Security and VPS hardening ✅ *done*

Delivered (details in [security.md](security.md)):

- **Authentication (`core/auth`):** an `admin` account created on first start, with the password written once to a `0600` file and deleted after it is changed. Passwords are Argon2id-hashed. `aegis-daemon admin reset-password` / `create-token` work locally through database file access.
- **Sessions:** 256-bit random ids, stored only as SHA-256 hashes. Cookies are `HttpOnly`, `SameSite=Strict`, and `Secure` over HTTPS. Sessions have idle and absolute expiry; a CSRF token is required on every change; a password change ends all sessions.
- **Login throttling:** separate limits per client address (5 per 15 min) and per username (20 per 15 min), so one attacker can't lock the admin out. Behind a proxy, only the proxy's `X-Forwarded-For` entry is trusted.
- **API tokens:** `read` or `deploy` scope, hashed at rest, revocable. Tokens can never manage tokens, passwords or webhook secrets.
- **Every `/api/v1` route returns 401 without credentials** except sign-in; a test enumerates all routes. `/healthz` is the only other public route.
- **Audit:** events carry `actor` (`user:…`, `token:…`, `webhook:github`, `cli`). Sign-ins, failed sign-ins (never the password), sign-outs, password and token changes are recorded.
- **TLS:** rustls (ring) serving from PEM files, with **certificate hot-reload** so renewals don't restart apps. Behind a proxy (Caddy/nginx), `behind_proxy` turns on Secure cookies and HSTS.
- **Safe binds enforced:** the daemon (and `aegis-daemon check-config`) refuses a public API without TLS, public access without `allowed_hosts`, half-configured TLS, and gRPC on a non-loopback address. A config file that doesn't parse is now an error instead of being silently replaced by defaults. CI checks these refusals against the real binary.
- **Security headers** on every response: CSP `default-src 'none'`, `X-Frame-Options: DENY`, nosniff, no-referrer, no-store, and HSTS over HTTPS.
- **Webhooks:** `POST /hooks/github/{project}` with HMAC-SHA256 (checked against GitHub's published test vector), per-project rotatable secrets, redelivery de-duplication, and push-to-branch → deploy of the pushed commit from the project's **configured** repository.
- **Service install:** `install.sh --server [--domain …]` creates an `aegis` system user, `/etc/aegis/aegis.toml` (`0640`), `/var/lib/aegis` (`0750`, database `0600`), and a hardened systemd unit (`ProtectSystem=strict`, `NoNewPrivileges`, `Delegate=yes`, `ExecStartPre=check-config`, `KillMode=mixed`). It verifies cleanly with `systemd-analyze` and was exercised as a real install in a container.

**Independent security review.** It found no authentication bypass, and it checked routing and path normalisation, CSRF, sessions, timing, webhook HMAC and config validation. It reported nine lower-severity issues, all fixed in this phase:

- **HTTP/2 requests failed the Host check.** This broke every browser under direct TLS. It was reproduced with a real HTTP/2 client test before fixing.
- **Login throttling could be raced**, and parallel Argon2 checks could exhaust memory. Attempts are now reserved atomically before the check, and at most 4 checks run at once.
- **Open event and log streams outlived** token revocation and password changes.
- **Webhooks:** replay of old signed pushes.
- **git argument injection hardening:** `--`, `--branch=`, and rejecting `-…` and `ext::` URLs. Credentials in URLs are redacted from build logs.
- **Installer:** a `--domain` newline bypass.
- **File permissions:** the database is created `0600` before SQLite opens it.
- **Certificate reload:** mismatched certificate/key pairs are rejected.
- **Audit log:** attempted usernames are recorded only for existing accounts.
- **Documentation:** the proxy's `X-Forwarded-For` requirement, and the fact that a deploy token is admin-equivalent.

Deliberately not included: built-in ACME. It can't be tested without a public domain, so the recommended path is Caddy (automatic HTTPS) or certbot with `[web.tls]`. Multiple users and roles remain Phase 6.

Evidence: 12 security tests in `core/web/tests/auth.rs` (route-by-route 401, cookie flags and CSRF, throttling, scopes, password change, streams closing on revocation, Host allowlist, signed webhooks deploying a real commit and ignoring replays, HTTPS over HTTP/1.1 and HTTP/2 with certificate renewal), 7 in `core/auth` (including 50 parallel sign-in attempts → exactly 5 allowed), config refusal tests, an independent security review of the diff, and new end-to-end steps (authentication, read-only token, unsafe-config refusals) in `scripts/e2e-smoke.sh`.

### Phase 3 — Web dashboard MVP *(~2–3 weeks)* → **v0.5.0**

Pages, served from the embedded assets:

1. **Login**
2. **Overview:** host CPU, memory, disk and load; a card per app with status, current version, uptime, restart count and quick actions.
3. **App detail:**
   - Deploy button (branch or commit, strategy) with **live 7-stage pipeline progress** over SSE.
   - Release history with one-click **rollback**.
   - Process controls: start, stop, restart.
   - **Live log viewer:** follow, pause, search, and download.
4. **Deployments timeline:** all deploys across apps with status and duration; click through to stage logs.
5. **Events / audit log:** filterable view of the event store (who did what, when).
6. **New app wizard:** repo URL → auto-detect runtime (reusing `DetectorPipeline`) → edit build/start commands, port and health path → save.

UX: responsive (usable on a phone for incident response), light and dark themes, keyboard shortcuts, and confirmation dialogs for destructive actions.

**Done when:** everything in the Phase 0 "done" scenario can be done from the browser without SSH, with Playwright end-to-end tests in CI.

### Phase 4 — Resource monitoring and control *(~2–3 weeks)* → **v0.6.0**

This is the "drag CPU and memory" feature.

- **Implement `runtime/metrics`:** sample every 2 s from `/proc` and the cgroup files: CPU %, RSS/memory, threads, open file descriptors, network I/O, restarts, and OOM kills. Keep 1 h at full resolution in memory and 1-minute rollups for 7 days in SQLite (retention is configurable).
- **Per-app cgroup:** the supervisor places each app in `aegis.slice/<project>.scope` (cgroup v2, delegated). Limits:
  - `cpu.max`: CPU quota, shown as a **CPU slider** from 0.1 to the number of host cores.
  - `memory.max`: hard limit, shown as a **memory slider**, with `memory.high` as a soft limit set to 90% of it.
  - Optional: `pids.max` and `io.max`.
- **Live apply:** moving a slider → `PUT /api/v1/projects/{id}/resources` → validation (reject a limit below current usage without a confirmation, and reject over-commit beyond host capacity unless forced) → write to the cgroup → `ResourceLimitsChanged` event. **No restart needed.** Limits are re-applied on every restart and deploy.
- **Charts:** live and historical CPU and memory per app, with the limit drawn as a line so throttling and pressure are visible.
- **Alerts:** CPU throttling, memory pressure (`memory.events` high/oom), crash loops, and disk space, shown in the UI and sent through the existing plugin system (Slack).
- **Fallbacks:** if cgroup v2 delegation isn't available, use `systemd-run --scope` with `systemctl set-property`. If neither is available (macOS/dev), show metrics and disable the sliders with a clear explanation.
- *(Optional)* Instances/replicas slider: run N copies of an app on consecutive ports. This needs the Phase 5 proxy.

**Done when:** on a VPS, dragging the memory slider on a running app changes `memory.max` within 1 s without a restart; a test app that exceeds it gets OOM-killed, restarted by the supervisor, and the event shows in the UI; the CPU limit visibly caps a busy-loop app's CPU % on the chart.

### Phase 5 — App configuration from the UI *(~2–3 weeks)*

- **Environment variables and secrets:** implement `core/secrets` (encrypted at rest with a key file outside the database). Edit them in the UI; values are masked; applying changes triggers a restart or redeploy.
- **Domains and reverse proxy:** map a domain to an app, with automatic TLS. Either embed a small `hyper`-based proxy or generate Caddy config. This enables true zero-downtime switching by flipping the upstream port and completes the "Ingress Engine" item from the public roadmap.
- **Schedules:** manage daily auto-deploys (`SchedulerEngine`) from the UI; persist and replay them on restart.
- **Webhook setup:** show the webhook URL and secret for each app, with a "test delivery" button.
- **Configuration as code:** the UI edits and `aegis.toml` stay in sync; the UI shows the diff before saving.

### Phase 6 — Operations polish *(ongoing)*

- Multiple users with roles (viewer, deployer, admin).
- Backup and restore of `aegis.db` and releases from the UI; scheduled backups.
- Self-update: "a new Aegis version is available → upgrade" (building on `aegis upgrade`).
- A real `ratatui` TUI on the same read API as the web UI.
- Docker runtime (`integrations/docker`): container apps with `docker update --cpus/--memory` for the same sliders.
- Later: multi-node fleet. A remote node agent reuses the `ControlPlane` API, and the dashboard gets a server selector.

---

## 3. Configuration additions

```toml
[daemon]
host = "127.0.0.1"
port = 50051                    # gRPC, localhost only (CLI/TUI)

[web]
enabled = true
host = "127.0.0.1"              # "0.0.0.0" requires tls or behind_proxy
port = 8420
behind_proxy = false            # trust X-Forwarded-* from 127.0.0.1 only
session_ttl_minutes = 720

[web.tls]
mode = "acme"                   # "off" | "files" | "acme"
domain = "aegis.example.com"
email = "ops@example.com"
# cert_path / key_path when mode = "files"

[resources.defaults]            # applied to new apps; per-app overrides in the UI
cpu_cores = 1.0
memory_mb = 512
```

## 4. New domain events

`ProcessStopRequested`, `ProcessRestartRequested`, `RollbackRequested`, `ResourceLimitsChanged`, `ResourcePressureDetected`, `UserLoggedIn`, `UserLoginFailed`, `ApiTokenCreated`, `ApiTokenRevoked`, `SecretUpdated` (the value is never stored in the event), `DomainMapped`. Every event triggered from the web gains an `actor` field. Add each to `docs/event_catalog.md` when it is implemented.

## 5. Risks

| Risk | Mitigation |
| :--- | :--- |
| A public dashboard becomes a remote-code-execution vector | Phase 2 is a hard gate before any public bind; safe defaults; the daemon refuses unsafe configurations |
| cgroup v2 delegation varies by distro and VPS provider | Detect at startup and show capability status in the UI; systemd fallback; test matrix of Ubuntu 22.04/24.04 and Debian 12 |
| Daemon restart kills managed apps | Run apps in their own cgroup scopes so they survive a daemon restart; re-adopt them by PID and cgroup on boot |
| Metrics storage growth in SQLite | Rollups plus retention pruning; metrics in a separate table/DB file from events |
| Scope creep | Phases 0 → 3 are the MVP; Phases 4+ ship incrementally behind the MVP |

## 6. Milestone summary

| Milestone | Phases | Outcome |
| :--- | :--- | :--- |
| **M-Core** ✅ | 0 | Deploy/stop/restart/rollback do real work |
| **M-API** ✅ | 1 + 2 | Secure HTTP API reachable on the VPS |
| **v0.5.0 — Web MVP** | 3 | Manage all apps from the browser |
| **v0.6.0 — Resource Control** | 4 | Live CPU/memory monitoring and slider limits |
| **v0.7.0 — Self-serve Ops** | 5 | Env/secrets, domains + TLS, webhooks, schedules in the UI |
