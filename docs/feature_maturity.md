# Aegis Feature Maturity

Every capability in Aegis is in one of four maturity tiers. A feature moves up a tier only when the evidence for that tier exists and is linked here.

---

## 1. Maturity Tiers

| Tier | Icon | Requirement |
| :--- | :---: | :--- |
| **Experimental** | 🧪 | Exists, but has little or no automated testing, or prints placeholder output. Expect changes. |
| **Validated** | 🟡 | Covered by automated tests that run in CI ([`ci.yml`](../.github/workflows/ci.yml)). |
| **Dogfooded** | 🟢 | Used by the Aegis project itself on a real host, with a published report. |
| **Production Proven** | 🔵 | Used in production by external users, with a published report. |

**Today no feature is Dogfooded or Production Proven.** Aegis doesn't deploy itself yet and has no recorded external production users. See the [scorecard](scorecard.md) for what is and isn't measured.

"Validated" means CI on Ubuntu (Linux). macOS and Windows are built in CI but not tested.

---

## 2. Feature Maturity Matrix

### Core and deployment

| Feature | Status | Evidence |
| :--- | :---: | :--- |
| **Event store & event bus** (ordered `seq`, replay, migration) | 🟡 Validated | Unit tests in [`core/event_store`](../core/event_store/src/lib.rs), [`core/projection`](../core/projection/src/lib.rs) |
| **Deploy pipeline** (clone/copy, install/build/test, artifacts, health check, automatic rollback) | 🟡 Validated | [`core/control/tests/lifecycle.rs`](../core/control/tests/lifecycle.rs), [`scripts/e2e-smoke.sh`](../scripts/e2e-smoke.sh), unit tests in `deployment/builder`, `deployment/health`, `deployment/artifact_store` |
| **Release switcher, retention and manual rollback** | 🟡 Validated | [`deployment/release/src/switcher.rs`](../deployment/release/src/switcher.rs) tests, `lifecycle.rs`, `e2e-smoke.sh` |
| **Process supervisor** (process groups, SIGTERM → SIGKILL, backoff, crash-loop detection, rotating logs) | 🟡 Validated | Unit tests in [`runtime/process`](../runtime/process/src/lib.rs), `e2e-smoke.sh` |
| **Daemon restart recovery** (apps restarted, stopped apps stay stopped) | 🟡 Validated | `lifecycle.rs`, `e2e-smoke.sh` |
| **Runtime detection** | 🟡 Validated (detection only) | [`runtime/engine/src/detector_pipeline.rs`](../runtime/engine/src/detector_pipeline.rs) tests. Only Node.js apps are actually deployed in CI. |
| **Scheduled daily deploys** | 🟡 Validated (unit) | [`core/scheduler`](../core/scheduler/src/lib.rs) tests; schedule API in [`core/web/tests/api.rs`](../core/web/tests/api.rs) |
| **Configuration loading and unsafe-config refusal** | 🟡 Validated | [`core/config`](../core/config/src/lib.rs) tests, `e2e-smoke.sh` |

### Web control plane (roadmap Phases 1–4)

| Feature | Status | Evidence |
| :--- | :---: | :--- |
| **HTTP/JSON API and SSE streams** (Phase 1) | 🟡 Validated | [`core/web/tests/api.rs`](../core/web/tests/api.rs), OpenAPI drift test, HTTP steps in `e2e-smoke.sh` |
| **Authentication, sessions, CSRF, API tokens, login throttling** (Phase 2) | 🟡 Validated | [`core/web/tests/auth.rs`](../core/web/tests/auth.rs), [`core/auth`](../core/auth/src/lib.rs) tests, `e2e-smoke.sh` |
| **Built-in TLS with certificate reload, HTTP/2** (Phase 2) | 🟡 Validated | `auth.rs` (HTTPS over HTTP/1.1 and HTTP/2, renewed certificates) |
| **GitHub webhooks** (HMAC, replay protection, push → deploy) (Phase 2) | 🟡 Validated | `auth.rs`, [`git/webhook`](../git/webhook/src/lib.rs) tests (including GitHub's published test vector) |
| **Hardened server install** (`install.sh --server`) (Phase 2) | 🧪 Experimental | Exercised by hand in a container; not run in CI |
| **Web dashboard** (Phase 3) | 🟡 Validated | Playwright tests in [`ui-tests/tests/`](../ui-tests/tests/) (desktop and phone), [`core/web/tests/dashboard.rs`](../core/web/tests/dashboard.rs) |
| **Resource monitoring and live CPU/memory limits** (Phase 4, cgroup v2) | 🟡 Validated | [`runtime/resources/tests/kernel.rs`](../runtime/resources/tests/kernel.rs) and [`core/web/tests/resources.rs`](../core/web/tests/resources.rs) run as root in CI; [`ui-tests/tests/resources.spec.js`](../ui-tests/tests/resources.spec.js) |
| **Resource limits on cgroup v1** (Phase 4) | 🧪 Experimental | Passed locally on a v1 host; not run in CI |
| **Slack notifications** | 🟡 Validated (unit) | [`integrations/slack`](../integrations/slack/src/lib.rs) tests post to a local test server, not to Slack |

### App configuration (roadmap Phase 5)

| Feature | Status | Evidence |
| :--- | :---: | :--- |
| **Built-in reverse proxy, domains and HTTPS** (`[proxy]`) | 🟡 Validated | [`runtime/ingress`](../runtime/ingress/) unit and integration tests (routing, headers, streaming, upgrades, HTTPS from files with SNI, ACME challenge), [`core/web/tests/domains.rs`](../core/web/tests/domains.rs). Certificate issuance against a real ACME CA is not tested. |
| **Secret and plain environment variables** | 🟡 Validated | [`core/secrets`](../core/secrets/src/lib.rs) tests (encryption at rest, key mismatch, slot binding), [`core/web/tests/appconfig.rs`](../core/web/tests/appconfig.rs) (values reach the app, restart applies changes, never returned by the API or written to events/releases) |
| **aegis.toml diff before saving, webhook status and test delivery** | 🟡 Validated | `appconfig.rs`, Playwright settings step in [`ui-tests/tests/dashboard.spec.js`](../ui-tests/tests/dashboard.spec.js) |
| **Zero-downtime cutover behind the proxy** | 🟡 Validated | [`core/control/tests/proxy_cutover.rs`](../core/control/tests/proxy_cutover.rs): no failed requests through deploys, a failed deploy, a rollback and a daemon restart |

### Roadmap Phase 6

| Feature | Status | Evidence |
| :--- | :---: | :--- |
| **Docker runtime** (`[run] runtime = "docker"`, [guide](docker_runtime.md)) | 🧪 Experimental | Unit tests for `[run]` validation, `docker` argument building (including that secrets never reach a command line) and limit mapping in [`core/config`](../core/config/src/lib.rs) and [`integrations/docker`](../integrations/docker/src/); supervisor tests with a fake launcher in [`runtime/process`](../runtime/process/src/lib.rs). [`core/control/tests/docker.rs`](../core/control/tests/docker.rs) runs against the real Docker daemon on CI's Ubuntu runner (`AEGIS_REQUIRE_DOCKER=1` makes a missing Docker a failure there). It covers the image build, the health check, a secret reaching the container, a redeploy through the proxy with no failed requests, live `docker update` limits, a killed container being restarted, an unhealthy release rolled back, a rollback that reuses the old image, image pruning, a daemon restart and a stop. [`docker_missing.rs`](../core/control/tests/docker_missing.rs) covers a host without Docker. Not used in production; dashboard display of the runtime is not browser-tested. |

### CLI and tools

| Feature | Status | Evidence |
| :--- | :---: | :--- |
| **Core CLI** (`init`, `validate`, `deploy`, `rollback`, `start`/`stop`/`restart`, `logs`, `list`, `releases`, `deployments`, `timeline`) | 🟡 Validated | `e2e-smoke.sh` |
| **Deployment replay (`aegis replay`)** | 🧪 Experimental | Unit tests over synthetic event histories in [`ui/cli/src/`](../ui/cli/src/); not yet run against a live daemon in CI |
| **Time travel inspection (`aegis inspect --at`)** | 🧪 Experimental | Unit tests over synthetic event histories in [`ui/cli/src/`](../ui/cli/src/); not yet run against a live daemon in CI |
| **Incident report (`aegis incident`)** | 🧪 Experimental | Unit tests over synthetic event histories in [`ui/cli/src/`](../ui/cli/src/); not yet run against a live daemon in CI |
| **Outage investigation (`aegis investigate`)** | 🧪 Experimental | Unit tests over synthetic event histories in [`ui/cli/src/`](../ui/cli/src/); not yet run against a live daemon in CI |
| **Self-update (`aegis upgrade`)** | 🧪 Experimental | Version comparison unit-tested in [`ui/cli/src/upgrade.rs`](../ui/cli/src/upgrade.rs); the install step is not run in CI |
| **PM2 & systemd migration (`aegis migrate`)** | 🧪 Experimental | Parser unit tests in [`ui/cli/src/migrate.rs`](../ui/cli/src/migrate.rs); not checked against real PM2/systemd setups |
| **Terminal UI (`aegis-tui`)** | 🧪 Experimental | `ratatui` dashboard over the local gRPC API: apps table, app view (releases, deployments, live logs), live event stream, and confirmed start/stop/restart/deploy/rollback actions ([roadmap](roadmap_web_control_plane.md) Phase 6). The state reducer, filters and screen rendering (`TestBackend`) have unit tests in CI. Two opt-in tests (`cargo test -p aegis-tui -- --ignored` with `AEGIS_TUI_LIVE_ADDR` set) drive its gRPC worker against a real daemon, including a reconnect after a daemon restart; they are not in CI, and no test drives the interactive terminal, so it stays Experimental. |

### Not implemented yet

`core/ssh`, `core/state`, `deployment/analytics`, `git/action` and `integrations/github` are stubs. Fleet support comes later ([roadmap](roadmap_web_control_plane.md) Phase 6).
