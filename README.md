# Aegis 🛡️

[![CI Status](https://img.shields.io/github/actions/workflow/status/shivam411/Aegis/ci.yml?branch=main&style=flat-square&logo=github)](https://github.com/shivam411/Aegis)
[![Crates.io](https://img.shields.io/crates/v/aegis-cli.svg?style=flat-square&logo=rust)](https://crates.io/crates/aegis-cli)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg?style=flat-square)](LICENSE)
[![APT Package](https://img.shields.io/badge/apt-v0.1.0-orange.svg?style=flat-square&logo=ubuntu)](https://aegis.dev/install)
[![RPM Package](https://img.shields.io/badge/rpm-v0.1.0-red.svg?style=flat-square&logo=redhat)](https://aegis.dev/install)
[![Homebrew](https://img.shields.io/badge/homebrew-v0.1.0-yellow.svg?style=flat-square&logo=homebrew)](https://aegis.dev/install)

**Aegis** is a zero-dependency, event-sourced operational terminal platform and process manager written in Rust. Designed as a modern, high-performance alternative to PM2, Heroku, and Vercel for single-server VPS environments and Linux server clusters.

---

## ✨ Features

- ⚡ **Zero-Dependency & SQLite-First**: Runs as a single self-contained daemon (`aegis-daemon`) with embedded, auto-migrating SQLite storage (`aegis.db`). No PostgreSQL, Redis, or external service overhead required.
- 🚀 **Zero-Downtime Deployments & Atomic Versioning**: Build and test releases in isolated staging environments (`~/.aegis/builds/{release_id}/`). Live application traffic is never touched until pre-switch health checks pass 100%. Atomic version pointer switches (`current.json`) guarantee zero downtime.
- 🧹 **Automated Version Retention (Default: 2 Versions)**: Auto-prunes older release directories and artifact archives while preserving the active version + newest rollback version, keeping server disk usage minimal.
- ⏰ **Daily Scheduled Auto-Deployments**: Background cron/hour scheduler (`aegis schedule`) to trigger automated zero-downtime builds at specific target hours (e.g., `02:00 AM`).
- 🛠️ **Polyglot Runtime Auto-Detection**: Native auto-detection and execution for Node.js (`package.json`), Rust (`Cargo.toml`), Go (`go.mod`), Python (`requirements.txt`), Bun, Deno, and custom commands.
- 🖥️ **High-Quality Terminal UI (TUI) & gRPC CLI**: Interactive dashboard (`aegis-tui`) powered by `ratatui` + `crossterm` alongside a full gRPC CLI client (`aegis-cli`).
- 🔒 **Event-Sourced Audit Trail & Pub/Sub Event Bus**: Every mutation is logged as an immutable domain event. Embedded pub/sub event bus dispatches events to plugins (Slack alerts, GitHub status, Webhook triggers).
- 🔄 **Instant 1-Step Rollbacks**: Roll back to any previous healthy release without rebuilding from source.

---

## 📦 Installation

### One-Line Shell Installer (Linux & macOS)

```bash
curl -fsSL https://get.aegis.dev | sh
```

### Linux Package Managers

#### Debian / Ubuntu (`.deb`)
```bash
curl -fsSL https://apt.aegis.dev/gpg.key | sudo gpg --dearmor -o /etc/apt/trusted.gpg.d/aegis.gpg
echo "deb [arch=amd64,arm64] https://apt.aegis.dev stable main" | sudo tee /etc/apt/sources.list.d/aegis.list
sudo apt update && sudo apt install aegis
```

#### RHEL / Fedora / CentOS (`.rpm`)
```bash
sudo dnf config-manager --add-repo https://rpm.aegis.dev/aegis.repo
sudo dnf install aegis
```

#### Arch Linux (`AUR`)
```bash
yay -S aegis-bin
```

### macOS (Homebrew)

```bash
brew tap aegis-dev/tap
brew install aegis
```

### Rust Cargo

```bash
cargo install --locked aegis-daemon aegis-cli aegis-tui
```

---

## 🚀 Quickstart Guide

### 1. Start the Aegis Daemon

Launch the daemon process on your VPS or server (runs gRPC service on port `50051` and auto-runs database migrations):

```bash
aegis-daemon
```

*To run as a systemd background service:*
```bash
sudo systemctl enable --now aegis
```

### 2. Initialize a Project

Run `aegis init` inside your project repository directory. Aegis automatically detects your runtime (Node.js, Rust, Go, Python):

```bash
aegis init --name my-api --repo https://github.com/user/my-api
```

### 3. Deploy an Application (Zero-Downtime)

Trigger a zero-downtime build and release:

```bash
aegis deploy --project <PROJECT_ID> --branch main
```

Aegis builds your code in isolation, runs health checks, atomically updates the active version pointer, and gracefully drains the previous process instance.

### 4. Configure Daily Scheduled Auto-Deployments

Set up daily automated deployments at specific hours (e.g. `02:00 AM` daily):

```bash
aegis schedule --project <PROJECT_ID> --hour 2 --branch main
```

### 5. Perform Instant Rollbacks

Instantly switch back to a previous healthy version without rebuilding:

```bash
aegis rollback --project <PROJECT_ID> --version v1.0.0
```

### 6. Monitor via Interactive Terminal UI (TUI)

Launch the real-time terminal dashboard:

```bash
aegis-tui
```

---

## 🏗️ Architecture & CQRS Event Flow

Aegis follows CQRS-Lite principles. Commands mutate state by appending immutable events to SQLite (`aegis.db`). The `EventBus` broadcasts events to the `ProjectionEngine` for 60fps in-memory TUI rendering.

```
┌─────────────────┐       Command       ┌──────────────┐
│  CLI / TUI /    │ ──────────────────► │ Aegis Daemon │
│  Git Webhooks   │                     └──────┬───────┘
└─────────────────┘                            │
                                        (Emits Event)
                                               ▼
                                      ┌─────────────────┐
                                      │    Event Bus    │ (tokio::broadcast)
                                      └────────┬────────┘
                    ┌──────────────────────────┼──────────────────────────┐
                    ▼                          ▼                          ▼
          ┌───────────────────┐      ┌───────────────────┐      ┌───────────────────┐
          │    Event Store    │      │ Projection Engine │      │  Plugin Manager   │
          │    (SQLite DB)    │      │(In-Memory Caches) │      │  (Slack / Alerts) │
          └───────────────────┘      └───────────────────┘      └───────────────────┘
```

### 7-Stage Build Pipeline

Deployments execute a strict, composable 7-stage sequence:

```
┌─────────┐   ┌─────────┐   ┌─────────┐   ┌─────────┐   ┌───────────┐   ┌──────────┐   ┌───────────┐
│ 1.Clone │──►│2.Install│──►│ 3.Build │──►│ 4.Test  │──►│ 5.Package │──►│ 6.Verify │──►│ 7.Promote │
└─────────┘   └─────────┘   └─────────┘   └─────────┘   └───────────┘   └──────────┘   └───────────┘
```

---

## 📜 Architecture Decision Records (ADRs)

Aegis architectural choices are documented under [`docs/adr`](docs/adr):

| ADR | Title | Summary |
| :--- | :--- | :--- |
| [ADR 0001](docs/adr/0001-event-sourcing.md) | Event Sourcing | Events are the immutable source of truth. |
| [ADR 0002](docs/adr/0002-sqlite-first-persistence.md) | SQLite First Persistence | Single-file DB (`aegis.db`) with zero external service dependencies. |
| [ADR 0003](docs/adr/0003-domain-driven-crates.md) | Domain-Driven Crates | Multi-crate workspace topology (`core/`, `deployment/`, `runtime/`, `git/`, `ui/`, `integrations/`). |
| [ADR 0004](docs/adr/0004-event-bus.md) | Event Bus Architecture | Decoupled pub/sub event distribution. |
| [ADR 0005](docs/adr/0005-projection-engine.md) | Projection Engine | In-memory projected state models for fast TUI/CLI reads. |
| [ADR 0006](docs/adr/0006-domain-type-ids.md) | Domain Type IDs | Strongly-typed, time-sortable `UUIDv7` identifiers. |
| [ADR 0007](docs/adr/0007-immutable-releases.md) | Immutable Releases | Releases tied to Git commit SHAs, artifacts, and build metadata. |
| [ADR 0008](docs/adr/0008-artifact-store.md) | Artifact Store | Disk-backed storage (`~/.aegis/artifacts/`) with SHA-256 integrity checks. |
| [ADR 0009](docs/adr/0009-deployment-strategies.md) | Deployment Strategies | Strategy abstraction (`Immediate`, `Rolling`, `BlueGreen`, `GracefulSwitch`). |
| [ADR 0010](docs/adr/0010-runtime-interface.md) | Runtime Interface | Polyglot runtime trait for Node.js, Rust, Go, Python, Deno, Bun. |

---

## ⚙️ Configuration (`aegis.toml`)

Create an `aegis.toml` file in your root working directory to customize daemon parameters:

```toml
[daemon]
host = "127.0.0.1"
port = 50051
database_path = "aegis.db"
log_level = "info"
max_retained_versions = 2  # Maximum releases kept on disk (default: 2)
```

---

## 💻 CLI Command Reference

| Command | Description |
| :--- | :--- |
| `aegis status` | Query daemon health status, version, and loaded plugins. |
| `aegis init` | Initialize a project and auto-detect runtime environment. |
| `aegis deploy` | Trigger a zero-downtime build and deployment pipeline. |
| `aegis schedule` | Configure daily automated deployment at specific target hours (`--hour <0-23>`). |
| `aegis rollback` | Instantly roll back a project to a previous release version. |
| `aegis list` | List active projects and running processes. |
| `aegis logs` | Tail real-time process stdout/stderr and operational logs. |
| `aegis stop` | Gracefully stop a running process PID. |
| `aegis restart` | Restart a monitored process. |
| `aegis emit-event` | Emit custom operational events to the event store. |
| `aegis stream-events` | Stream live event bus notifications from the daemon. |

---

## 🛠️ Development & Contributing

We welcome open-source contributions!

### Prerequisites
- Rust 1.75+
- SQLite3
- Protocol Buffers compiler (`protoc`)

### Building & Testing

```bash
# Clone the repository
git clone https://github.com/shivam411/Aegis.git
cd Aegis

# Check workspace compilation
cargo check --workspace

# Run all workspace unit and integration tests
cargo test --workspace

# Run Clippy linter
cargo clippy --workspace --all-targets
```

---

## 📄 License

Licensed under the [MIT License](LICENSE). Built with ❤️ by the Aegis core team.