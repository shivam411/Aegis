# Aegis 🛡️

[![CI](https://img.shields.io/github/actions/workflow/status/shivam411/Aegis/ci.yml?branch=main&style=flat-square&logo=github&label=CI)](https://github.com/shivam411/Aegis/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/shivam411/Aegis?style=flat-square&logo=github&label=Release)](https://github.com/shivam411/Aegis/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg?style=flat-square)](LICENSE)
[![Docs](https://img.shields.io/badge/Docs-GitHub%20Pages-blue?style=flat-square)](https://shivam411.github.io/Aegis/)

**Aegis** is production operations from your terminal. Zero-dependency, event-sourced process supervision and zero-downtime deployment platform written in Rust.

```bash
curl -fsSL https://raw.githubusercontent.com/shivam411/Aegis/main/install.sh | bash
```

---

## ✨ Features

| Feature | Description |
| :--- | :--- |
| ⚡ **Zero-Dependency** | Single daemon binary with embedded SQLite. No PostgreSQL, Redis, or Docker required. |
| 🚀 **Safe Deployments** | Build in isolation (a failed build never touches the live app), health-check the new release, and roll back automatically if it doesn't come up. Fully zero-downtime cutover needs the planned reverse proxy ([roadmap](docs/roadmap_web_control_plane.md)). |
| 🧹 **Automated Version Retention** | Keeps the last N releases on disk (default: 2). Older versions auto-pruned after each deploy. |
| ⏰ **Scheduled Auto-Deployments** | Background scheduler triggers builds at specific daily hours (e.g. `02:00 AM`). |
| 🛠️ **Polyglot Runtime Detection** | Auto-detects Node.js, Rust, Go, Python, Bun, Deno from project files. |
| 🖥️ **Terminal UI (TUI)** | Real-time interactive dashboard powered by `ratatui` + `crossterm`. |
| 🔒 **Event-Sourced Audit Trail** | Every mutation is an immutable domain event. Full replay on startup. |
| 🔄 **Instant Rollbacks** | Roll back to any retained release without rebuilding from source. |
| 🔌 **Plugin System** | Extensible event bus with Slack, GitHub, and webhook integrations. |

---

## 📦 Installation

### One-Line Installer (Linux & macOS)

```bash
curl -fsSL https://raw.githubusercontent.com/shivam411/Aegis/main/install.sh | bash
```

The installer automatically detects your OS and CPU architecture, downloads the latest release from GitHub, and installs to `~/.local/bin/`.

### Manual Download

Download the latest binary for your platform from [**GitHub Releases**](https://github.com/shivam411/Aegis/releases/latest):

| Platform | Archive |
| :--- | :--- |
| Linux x86_64 | `aegis-linux-amd64.tar.gz` |
| Linux ARM64 | `aegis-linux-arm64.tar.gz` |
| macOS x86_64 | `aegis-darwin-amd64.tar.gz` |
| macOS ARM64 (Apple Silicon) | `aegis-darwin-arm64.tar.gz` |
| Windows x86_64 | `aegis-windows-amd64.zip` |

```bash
tar -xzf aegis-linux-amd64.tar.gz
mv aegis-daemon aegis-cli aegis-tui ~/.local/bin/
```

### Build from Source

```bash
git clone https://github.com/shivam411/Aegis.git
cd Aegis
cargo build --release
cp target/release/aegis-daemon target/release/aegis-cli target/release/aegis-tui ~/.local/bin/
```

Requires Rust 1.75+, SQLite3, and `protoc`.

---

## 🚀 Quickstart

```bash
# 1. Initialize project in your repository
aegis init

# 2. Verify project configuration
aegis validate

# 3. Build, health-check and deploy (rolls back automatically on failure)
aegis deploy

# 4. Investigate outages & root cause
aegis investigate

# 5. Execute instant rollback if needed
aegis rollback
```

For an interactive 2-minute tour, run: `aegis demo`.
See the public operational metrics on the [**Public Scorecard**](docs/scorecard.md).

---

## 📊 Feature Maturity & Empirical Evidence

Every capability in Aegis is assigned a Feature Maturity Level:

| Feature / Capability | Maturity Level | Empirical Validation Report |
| :--- | :---: | :--- |
| **Event Store & Event Bus** | 🟢 **Dogfooded** | [`validation/dogfood/dogfood-report.md`](validation/dogfood/dogfood-report.md) |
| **Detector Pipeline** | 🟡 **Validated** | [`validation/compatibility/compatibility-report.md`](validation/compatibility/compatibility-report.md) |
| **Runtime Supervisor Engine** | 🟡 **Validated** | [`validation/soak/72h-report.md`](validation/soak/72h-report.md) |
| **Atomic Release Switcher** | 🟡 **Validated** | [`validation/chaos/chaos-report.md`](validation/chaos/chaos-report.md) |
| **PM2 & Systemd Migration Engine** | 🟡 **Validated** | [`validation/migration/migration-report.md`](validation/migration/migration-report.md) |
| **Deployment Replay (`aegis replay`)** | 🧪 **Experimental** | [`docs/proof_phase.md`](docs/proof_phase.md) |
| **Time Travel Inspection (`aegis inspect --at`)** | 🧪 **Experimental** | [`docs/proof_phase.md`](docs/proof_phase.md) |
| **Outage Investigation Engine (`aegis investigate`)** | 🧪 **Experimental** | [`docs/proof_phase.md`](docs/proof_phase.md) |

For complete empirical evidence, see the [`validation/`](validation/) evidence repository.

---

## 🏗️ Architecture

Aegis follows CQRS-Lite event-sourcing principles. Commands mutate state by appending immutable events to SQLite. The `EventBus` broadcasts events to the `ProjectionEngine` for fast in-memory TUI rendering.

```
┌─────────────────┐       Command       ┌──────────────┐
│  CLI / TUI /    │ ──────────────────► │ Aegis Daemon │
│  Git Webhooks   │                     └──────┬───────┘
└─────────────────┘                            │
                                        (Emits Event)
                                               ▼
                                      ┌─────────────────┐
                                      │    Event Bus    │
                                      └────────┬────────┘
                    ┌──────────────────────────┼──────────────────────────┐
                    ▼                          ▼                          ▼
          ┌───────────────────┐      ┌───────────────────┐      ┌───────────────────┐
          │    Event Store    │      │ Projection Engine │      │  Plugin Manager   │
          │    (SQLite DB)    │      │(In-Memory Caches) │      │  (Slack / GitHub) │
          └───────────────────┘      └───────────────────┘      └───────────────────┘
```

### 7-Stage Build Pipeline

```
Clone → Install → Build → Test → Package → Verify → Promote
```

Each stage runs in an isolated build directory. If any stage fails, the live version remains untouched.

For a deep dive, see the [**Architecture Documentation**](https://shivam411.github.io/Aegis/architecture.md).

---

## 💻 CLI Reference

| Command | Description |
| :--- | :--- |
| `aegis-cli status` | Daemon version, project count, running apps |
| `aegis-cli init` | Detect the runtime, write `aegis.toml`, register the project |
| `aegis-cli validate` | Check `aegis.toml` and show the effective settings |
| `aegis-cli deploy [project]` | Build and deploy, showing each stage; `--release`, `--repo`, `--commit`, `--detach` |
| `aegis-cli rollback [project]` | Switch back to the previous (or `--version`) release without rebuilding |
| `aegis-cli list` | Projects with status, live release, PID and restart count |
| `aegis-cli logs [target] [-n N] [-f]` | App output (stdout, stderr and supervisor messages) |
| `aegis-cli start` / `stop` / `restart [target]` | Control an app's process; a stopped app stays stopped across daemon restarts |
| `aegis-cli releases [project]` | Release history with status and commit |
| `aegis-cli deployments [project]` | Deployment history with status, failing stage and reason |
| `aegis-cli timeline [project]` | The project's recent events |
| `aegis-cli schedule` | Configure daily auto-deployment (`--hour 0-23`) |
| `aegis-cli emit-event` | Emit a custom event to the event store |
| `aegis-cli stream-events` | Stream live event bus notifications |

`target` is a process id or a project id/name; without it, commands use the project in `./aegis.toml`.

### Web dashboard

With `[web] enabled = true`, open `http://127.0.0.1:8420/` (through an SSH tunnel, a reverse proxy, or built-in TLS) and sign in as `admin`. From the browser you can:

- **Overview:** host CPU, memory, disk and load, plus every app's status, version, uptime and restarts.
- **Deploy:** choose a branch, commit and strategy, and watch the 7-stage pipeline and build log live.
- **Operate:** start, stop and restart apps, and roll back from the release history.
- **Logs:** follow, pause, search and download live logs.
- **Audit:** browse the deployments timeline and a filterable event log showing who did what.
- **Add apps:** a wizard goes from repo URL to auto-detected settings to a first deploy.
- **Manage:** edit app settings, set daily schedules, create webhook secrets and API tokens.

The dashboard has light and dark themes, works on a phone, and has keyboard shortcuts (press `?`).

### HTTP API

Set `[web] enabled = true` and the daemon also serves an authenticated JSON API, with live Server-Sent Events, on `127.0.0.1:8420`. Sign in as `admin` (the first password is in `<data_dir>/initial-admin-password`) or use API tokens. Reach it through an SSH tunnel, a reverse proxy with HTTPS, or built-in TLS. See [docs/http_api.md](docs/http_api.md), [docs/security.md](docs/security.md) and [docs/openapi.json](docs/openapi.json).

### Server install

```bash
curl -fsSL https://raw.githubusercontent.com/shivam411/Aegis/main/install.sh | sudo bash -s -- --server
```

This installs Aegis as a hardened systemd service running as a dedicated `aegis` user, with the web API enabled on loopback. Add `--domain aegis.example.com` to prepare it for a reverse proxy such as Caddy.

---

## ⚙️ Configuration

Aegis reads `aegis.toml` from the working directory:

```toml
[daemon]
host = "127.0.0.1"
port = 50051
database_path = "aegis.db"
log_level = "info"
max_retained_versions = 2
```

---

## 🗺️ Product Roadmap & Production Themes

> [!NOTE]
> Aegis is currently in **Beta Readiness (`v0.4.0-beta`)**. New architectural changes follow the [RFC Process](docs/rfcs/0001-rfc-process.md).

| Production Theme | Key Focus & Customer Outcome | Status |
| :--- | :--- | :--- |
| **Theme 1: Production Reliability** | Crash recovery, health verification, restart backoff, rollback safety ([`slo_targets.md`](docs/slo_targets.md)) | ✅ Complete |
| **Theme 2: Production Visibility** | Live dashboard, deployment timeline, event explorer, incident diagnosis (`aegis incident`) | ▶ **In Progress** |
| **Theme 3: Production Automation** | GitHub Actions CI/CD, auto-deploy, scheduled auto-deploys, environment promotion | 📅 Planned |
| **Theme 4: Production Fleet** | Multi-node rolling deployments, node agent coordination, fleet management | 📅 Planned |


---

## 🛠️ Development

```bash
git clone https://github.com/shivam411/Aegis.git
cd Aegis

cargo check --workspace          # Compile check
cargo test --workspace           # Run all tests
cargo clippy --workspace         # Lint
cargo fmt --all                  # Format

# Dashboard browser tests (needs Node 18+; uses a debug build of the daemon)
cargo build -p aegis-daemon
cd ui-tests && npm ci && npx playwright install chromium && npx playwright test
```

### Repository Layout

```
Aegis/
├── core/           # Daemon, config, event store, projection, scheduler
├── deployment/     # Release, builder, strategy, health, rollback
├── runtime/        # Process supervisor, polyglot engine, logs
├── git/            # Repository operations, webhooks
├── ui/             # CLI (clap + tonic), TUI (ratatui + crossterm)
├── integrations/   # Slack, GitHub, Docker plugins
├── docs/           # Documentation (GitHub Pages)
├── ui-tests/       # Playwright tests for the web dashboard
├── scripts/        # Install/uninstall helpers
├── install.sh      # One-line installer
└── .github/        # CI, release, and pages workflows
```

---

## 📄 License

Licensed under the [MIT License](LICENSE).

Built with ❤️ in Rust.