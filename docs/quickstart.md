# Quickstart Guide

This guide walks you through deploying your first application with Aegis in under 5 minutes.

---

## 1. Start the Daemon

Launch the Aegis daemon. It starts the gRPC service on port `50051` and auto-creates the SQLite database:

```bash
aegis-daemon
```

To run as a background systemd service:

```bash
# Create the systemd unit (one-time)
sudo tee /etc/systemd/system/aegis.service > /dev/null <<EOF
[Unit]
Description=Aegis Deployment Daemon
After=network.target

[Service]
Type=simple
User=$USER
ExecStart=$HOME/.local/bin/aegis-daemon
Restart=on-failure
RestartSec=5
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now aegis
```

---

## 2. Initialize a Project

Inside your project directory, initialize Aegis. It auto-detects the runtime:

```bash
aegis-cli init --name my-api --repo https://github.com/user/my-api
```

Aegis scans for `package.json` (Node.js), `Cargo.toml` (Rust), `go.mod` (Go), `requirements.txt` (Python), or `bun.lockb` (Bun) and configures the build pipeline automatically.

---

## 3. Deploy (Zero-Downtime)

Trigger a zero-downtime deployment:

```bash
aegis-cli deploy --project <PROJECT_ID> --branch main
```

What happens behind the scenes:

```
1. Clone     → Git clone into isolated build directory
2. Install   → Install dependencies (npm install / cargo fetch / go mod download)
3. Build     → Compile project (npm run build / cargo build --release)
4. Test      → Run test suite (npm test / cargo test)
5. Package   → Archive build artifacts with SHA-256 checksums
6. Verify    → Health check the new instance before switching traffic
7. Promote   → Atomically switch the active version pointer
```

The previous version keeps running until the new version passes health checks. If anything fails, the live version remains untouched.

---

## 4. Check Status

Query daemon status and active projects:

```bash
aegis-cli status
```

List running processes:

```bash
aegis-cli list
```

---

## 5. Configure Scheduled Deployments

Set a daily automated deployment at 02:00 AM:

```bash
aegis-cli schedule --project <PROJECT_ID> --hour 2 --branch main
```

---

## 6. Roll Back

If something goes wrong, instantly roll back to a previous version:

```bash
aegis-cli rollback --project <PROJECT_ID> --version v1.0.0
```

No rebuild required — Aegis keeps previous release artifacts on disk (configurable via `max_retained_versions`).

---

## 7. Stream Logs

Tail real-time process logs:

```bash
aegis-cli logs --process-id <PROCESS_ID>
```

Stream all operational events:

```bash
aegis-cli stream-events
```

---

## 8. Launch the Terminal UI

For a real-time interactive dashboard:

```bash
aegis-tui
```

The TUI provides live views of projects, deployments, processes, and event streams.

---

## Configuration

Aegis reads configuration from `aegis.toml` in the working directory:

```toml
[daemon]
host = "127.0.0.1"
port = 50051
database_path = "aegis.db"
log_level = "info"
max_retained_versions = 2
```

| Field | Default | Description |
| :--- | :--- | :--- |
| `host` | `127.0.0.1` | gRPC server bind address |
| `port` | `50051` | gRPC server port |
| `database_path` | `aegis.db` | SQLite database file location |
| `log_level` | `info` | Tracing log level (`trace`, `debug`, `info`, `warn`, `error`) |
| `max_retained_versions` | `2` | Maximum release versions kept on disk per project |

---

## Next Steps

- Read the [Architecture Deep Dive](architecture.md) to understand how Aegis works under the hood.
- Browse the [CLI Command Reference](https://github.com/shivam411/Aegis#-cli-command-reference) in the README.
