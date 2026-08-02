# Aegis 🛡️

[![CI](https://img.shields.io/github/actions/workflow/status/shivam411/Aegis/ci.yml?branch=main&style=flat-square&logo=github&label=CI)](https://github.com/shivam411/Aegis/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/shivam411/Aegis?style=flat-square&logo=github&label=Release)](https://github.com/shivam411/Aegis/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg?style=flat-square)](LICENSE)
[![Docs](https://img.shields.io/badge/Docs-GitHub%20Pages-blue?style=flat-square)](https://shivam411.github.io/Aegis/)

**Aegis** is a zero-dependency, event-sourced deployment platform and process manager written in Rust. A modern, self-hosted alternative to PM2, Heroku, and Vercel for single-server VPS environments.

```bash
curl -fsSL https://shivam411.github.io/Aegis/install.sh | bash
```

---

## ✨ Features

| Feature | Description |
| :--- | :--- |
| ⚡ **Zero-Dependency** | Single daemon binary with embedded SQLite. No PostgreSQL, Redis, or Docker required. |
| 🚀 **Zero-Downtime Deployments** | Build in isolation, health-check, atomically switch. Live traffic is never interrupted. |
| 🧹 **Automated Version Retention** | Keeps the last N releases on disk (default: 2). Older versions auto-pruned after each deploy. |
| ⏰ **Scheduled Auto-Deployments** | Background scheduler triggers builds at specific daily hours (e.g. `02:00 AM`). |
| 🛠️ **Polyglot Runtime Detection** | Auto-detects Node.js, Rust, Go, Python, Bun, Deno from project files. |
| 🖥️ **Terminal UI (TUI)** | Real-time interactive dashboard powered by `ratatui` + `crossterm`. |
| 🔒 **Event-Sourced Audit Trail** | Every mutation is an immutable domain event. Full replay on startup. |
| 🔄 **Instant Rollbacks** | Roll back to any previous release without rebuilding from source. |
| 🔌 **Plugin System** | Extensible event bus with Slack, GitHub, and webhook integrations. |

---

## 📦 Installation

### One-Line Installer (Linux & macOS)

```bash
curl -fsSL https://shivam411.github.io/Aegis/install.sh | bash
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
# 1. Start the daemon
aegis-daemon

# 2. Initialize a project (auto-detects runtime)
aegis-cli init --name my-api --repo https://github.com/user/my-api

# 3. Deploy with zero downtime
aegis-cli deploy --project <PROJECT_ID> --branch main

# 4. Schedule daily auto-deployments at 2 AM
aegis-cli schedule --project <PROJECT_ID> --hour 2

# 5. Instant rollback
aegis-cli rollback --project <PROJECT_ID> --version v1.0.0

# 6. Launch the terminal UI
aegis-tui
```

For a detailed walkthrough, see the [**Quickstart Guide**](https://shivam411.github.io/Aegis/quickstart.md).

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
| `aegis-cli status` | Query daemon health and version |
| `aegis-cli init` | Initialize a project with runtime auto-detection |
| `aegis-cli deploy` | Trigger a zero-downtime deployment |
| `aegis-cli schedule` | Configure daily auto-deployment (`--hour 0-23`) |
| `aegis-cli rollback` | Roll back to a previous release version |
| `aegis-cli list` | List active projects and running processes |
| `aegis-cli logs` | Tail real-time process logs |
| `aegis-cli stop` | Gracefully stop a process |
| `aegis-cli restart` | Restart a monitored process |
| `aegis-cli emit-event` | Emit a custom event to the event store |
| `aegis-cli stream-events` | Stream live event bus notifications |

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

## 🗺️ Roadmap

| Milestone | Scope |
| :--- | :--- |
| **v0.1** | GitHub Releases + GitHub Pages + installer script |
| **v0.2** | `aegis self-update`, shell completions, `aegis doctor` |
| **v0.3** | macOS and Windows binary improvements |
| **v1.0** | Homebrew, AUR, `.deb`, `.rpm` packages |

---

## 🛠️ Development

```bash
git clone https://github.com/shivam411/Aegis.git
cd Aegis

cargo check --workspace          # Compile check
cargo test --workspace           # Run all tests
cargo clippy --workspace         # Lint
cargo fmt --all                  # Format
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
├── scripts/        # Install/uninstall helpers
├── install.sh      # One-line installer
└── .github/        # CI, release, and pages workflows
```

---

## 📄 License

Licensed under the [MIT License](LICENSE).

Built with ❤️ in Rust.