# Roadmap: Aegis Web Control Plane

**Goal:** Aegis runs on a VPS and serves a web dashboard on a configurable port. From that page, an operator can manage everything they can manage today from the CLI, plus live resource control: deploy, roll back, start/stop/restart apps, read logs, watch CPU/memory, and change each app's CPU and memory limits with sliders.

**Status:** Proposed · **Target:** `v0.5.0` (web MVP), `v0.6.0` (resource control)

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

### Simulated or stubbed behaviour (must be made real before a web UI is useful)

These pass tests but don't do what the docs and CLI claim. A dashboard that shows these states would be showing fiction, so **Phase 0 comes first**.

| Area | Current behaviour | Location |
| :--- | :--- | :--- |
| Deploy saga starts the app | Spawns `echo Deploy success` instead of the project's `start_command` | `core/daemon/src/main.rs` (saga, `"echo"`) |
| Health checks | Never run: strategies receive `None` for the checker, and the target is hardcoded to `127.0.0.1:8080` | `core/daemon/src/main.rs` (`execute(&ctx, None)`) |
| Previous release / graceful drain | `previous_release` is always `None`, so the old process is never drained | `core/daemon/src/main.rs`, `deployment/strategy/src/lib.rs` |
| Release versions | Every deploy is `v1.0.0`, so release directories collide and retention never prunes | `core/daemon/src/main.rs` (`unwrap_or("v1.0.0")`) |
| Build pipeline stages | Clone and Test only log; they don't run anything | `deployment/builder/src/lib.rs` |
| Artifact store | "SHA-256" hashes the *path string*, and directories are not copied | `deployment/artifact_store/src/lib.rs` |
| `aegis stop` / `restart` / `rollback` | Append `ProcessStopped` / `ProcessRestarted` / `RollbackTriggered`, but **nothing in the daemon acts on them**. `stop` records a stop that never happened. | `ui/cli/src/main.rs`, `core/daemon/src/main.rs` |
| Restart policy | `Always` restarts only once, then gives up; no backoff | `runtime/process/src/lib.rs` |
| Process logs | Only go to the daemon's tracing output; not stored or queryable per process | `runtime/process/src/lib.rs` |
| Rolling strategy | Returns `Success` without doing anything | `deployment/strategy/src/lib.rs` |
| Webhook | Publishes to an `EventBus` but there is no HTTP endpoint to receive webhooks | `git/webhook/src/lib.rs` |
| Stub crates (1-line) | `metrics`, `auth`, `secrets`, `ssh`, `state`, `analytics`, `action`, `docker`, `github` | various |
| TUI | Prints a status line and an event stream; not the `ratatui` dashboard the README describes | `ui/tui/src/main.rs` |
| Event ordering | Replays by `created_at` text instead of an insertion sequence | `core/event_store/src/lib.rs` |

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

### Phase 0 — Make the core real *(prerequisite, ~2–3 weeks)*

The web UI will expose these operations directly, so they must be correct first.

1. **Real app start:** the saga reads `[build].start_command` and the health URL from the project's `aegis.toml` and supervises the real process, not `echo`.
2. **Real health checks:** wire `TcpHealthChecker` / `HttpHealthChecker` into the strategies with the configured target and timeout. A failed check stops the switch and records `DeploymentFailed`.
3. **Release lineage:** unique version per deploy (timestamp or commit SHA), `previous_release` from projections, and graceful drain of the old process after a successful switch.
4. **Command handlers in the daemon:** turn `stop`, `restart` and `rollback` into *requests* (`ProcessStopRequested`, `ProcessRestartRequested`, `RollbackRequested`). The daemon executes them and then emits the fact (`ProcessStopped`, …). Stop recording a fact before it has happened.
5. **Supervisor:** restart loop with exponential backoff and a crash-loop limit; stdout/stderr go to per-process rotating log files plus an in-memory tail buffer.
6. **Builder:** real `git clone`/`fetch` into an isolated build directory; run the configured test command. The artifact store copies the build output and hashes the file contents.
7. **Event store:** add an autoincrement `seq` column and replay in `seq` order (migration).
8. **Read API:** gRPC queries for projects, deployments, releases, processes and logs, so the CLI `list` shows real data. These same queries back the web API.
9. **Refactor:** move the saga out of `main.rs` into a `ControlPlane` crate (`core/control`) with unit tests.

**Done when:** on a clean Ubuntu VPS, `aegis init && aegis deploy` on `examples/node-app` serves HTTP on its port, `aegis stop`/`restart` really stop and restart it, a broken build leaves the old version serving, and `aegis rollback` switches back. An integration test in CI covers this flow with `examples/node-app` or a tiny Rust fixture.

### Phase 1 — HTTP API in the daemon *(~1–2 weeks)*

- Add an `axum` listener next to `tonic`, controlled by `[web]` config and **disabled unless enabled**.
- REST endpoints (JSON), all served by `ControlPlane`:

| Method & path | Purpose |
| :--- | :--- |
| `GET /api/v1/status` | Daemon version, uptime, host summary |
| `GET /api/v1/projects` · `GET /api/v1/projects/{id}` | Projects with current release and process state |
| `POST /api/v1/projects` | Register a project (repo URL, branch, commands, port) |
| `POST /api/v1/projects/{id}/deploy` | Queue a deployment (branch or commit, strategy) |
| `POST /api/v1/projects/{id}/rollback` | Roll back to a release |
| `GET /api/v1/projects/{id}/releases` · `/deployments` | History |
| `POST /api/v1/processes/{id}/{start,stop,restart}` | Process control |
| `GET /api/v1/processes/{id}/logs?tail=500` | Log tail |
| `GET /api/v1/events/stream` | SSE: live domain events |
| `GET /api/v1/processes/{id}/logs/stream` | SSE: live logs |

- OpenAPI spec generated with `utoipa` and published in `docs/`.

**Done when:** every CLI command has an HTTP equivalent, integration tests cover each endpoint, and the gRPC and HTTP transports share one implementation.

### Phase 2 — Security and VPS hardening *(~1–2 weeks; must ship before any public bind)*

A web page that can deploy code is remote code execution by design, so this phase is not optional.

- **Bind safely by default:** `web.host = "127.0.0.1"`. Binding `0.0.0.0` requires TLS or an explicit `web.behind_proxy = true`. The daemon refuses to start in an unsafe combination.
- **Authentication:** implement `core/auth`. On first start, generate an admin password, print it once, and store it Argon2id-hashed in SQLite. Also offer `aegis admin reset-password`.
- **Sessions:** `HttpOnly`, `Secure`, `SameSite=Strict` cookies; CSRF tokens on every mutating request; idle and absolute timeouts; login rate limiting and lockout.
- **API tokens** for automation (scoped, revocable, hashed at rest).
- **TLS:** built-in `rustls` with a certificate path, *or* automatic Let's Encrypt via ACME (`rustls-acme`) when a domain is configured. Document the Caddy/nginx reverse-proxy option.
- **Audit:** every web action is a domain event that includes the actor, so the existing event store becomes the audit log shown in the UI.
- **Security headers:** strict CSP (the UI loads nothing from third-party origins), HSTS, `X-Frame-Options: DENY`.
- **Service install:** `install.sh --server` installs a hardened systemd unit (dedicated `aegis` user, `ProtectSystem=strict`, cgroup delegation via `Delegate=yes`), opens the port only when requested, and prints the dashboard URL and first-login password.
- **Webhooks:** `POST /hooks/github/{project}` with HMAC-SHA256 signature verification (`git/webhook` finally gets a real endpoint).

**Done when:** a security review passes, unauthenticated requests get `401` on every route except `/login` and `/hooks/*` (signature-checked), and a CI test proves the daemon refuses a public bind without TLS or proxy mode.

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
| **M-Core** | 0 | Deploy/stop/restart/rollback do real work |
| **M-API** | 1 + 2 | Secure HTTP API reachable on the VPS |
| **v0.5.0 — Web MVP** | 3 | Manage all apps from the browser |
| **v0.6.0 — Resource Control** | 4 | Live CPU/memory monitoring and slider limits |
| **v0.7.0 — Self-serve Ops** | 5 | Env/secrets, domains + TLS, webhooks, schedules in the UI |
