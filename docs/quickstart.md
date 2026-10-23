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
aegis init --name my-api --repo https://github.com/user/my-api
```

Aegis scans for `package.json` (Node.js), `Cargo.toml` (Rust), `go.mod` (Go), `requirements.txt` (Python), or `bun.lockb` (Bun) and configures the build pipeline automatically.

---

## 3. Deploy

Trigger a health-checked deployment:

```bash
aegis deploy my-api --branch main
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

A failed build never touches the live version. For an app with a domain on the built-in proxy, the new release starts next to the old one and the proxy switches over once it is healthy (no downtime). An app without a domain has its old process stopped before the new one starts, so there is a short gap while it starts listening. If the new version fails its health check, the previous release is started again.

---

## 4. Check Status

Query daemon status and active projects:

```bash
aegis status
```

List running processes:

```bash
aegis list
```

---

## 5. Configure Scheduled Deployments

Set a daily automated deployment at 02:00 AM:

```bash
aegis schedule my-api --hour 2 --branch main
```

---

## 6. Roll Back

If something goes wrong, instantly roll back to a previous version:

```bash
aegis rollback my-api --version v1.0.0
```

No rebuild required — Aegis keeps previous release artifacts on disk (configurable via `max_retained_versions`).

---

## 7. Stream Logs

Tail real-time process logs:

```bash
aegis logs my-api -f
```

Stream all operational events:

```bash
aegis stream-events
```

---

## 8. Launch the Terminal UI

For a real-time interactive dashboard:

```bash
aegis-tui                         # daemon address from ./aegis.toml [daemon]
aegis-tui --addr 127.0.0.1:50051  # or point it at a daemon explicitly
```

The TUI (experimental) shows your apps with status, live release, uptime, restarts and CPU/memory. Press `Enter` on an app for its releases, recent deployments and live log, `Tab` to switch panes or to the event stream, `/` to filter, `f` to follow or pause, and `?` for every key. `s`/`x`/`R` start, stop and restart, `d` deploys and `b` rolls back; each asks for confirmation. If the daemon stops, the TUI shows a banner and reconnects on its own.

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
