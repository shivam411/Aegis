# Quickstart Guide

Deploy your first app in under 5 minutes.

---

## 1. Start the Daemon

Starts the gRPC service on port `50051` and creates the SQLite database:

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

In your project directory (the runtime is auto-detected):

```bash
aegis init --name my-api --repo https://github.com/user/my-api
```

Detects `package.json` (Node.js), `Cargo.toml` (Rust), `go.mod` (Go), `requirements.txt` (Python) or `bun.lockb` (Bun).

---

## 3. Deploy

Health-checked deployment:

```bash
aegis deploy my-api --branch main
```

```
1. Clone     → Git clone into isolated build directory
2. Install   → Install dependencies (npm install / cargo fetch / go mod download)
3. Build     → Compile project (npm run build / cargo build --release)
4. Test      → Run test suite (npm test / cargo test)
5. Package   → Archive build artifacts with SHA-256 checksums
6. Verify    → Health check the new instance before switching traffic
7. Promote   → Atomically switch the active version pointer
```

A failed build never touches the live version. With a domain on the built-in proxy, the new release starts beside the old one and traffic switches once it is healthy (no downtime). Without a domain, the old process stops first, so there is a short gap. If the health check fails, the previous release is started again.

---

## 4. Check Status

Daemon status and projects:

```bash
aegis status
```

Running processes:

```bash
aegis list
```

---

## 5. Configure Scheduled Deployments

Daily deployment at 02:00:

```bash
aegis schedule my-api --hour 2 --branch main
```

---

## 6. Roll Back

Roll back to a previous version:

```bash
aegis rollback my-api --version v1.0.0
```

No rebuild: previous releases stay on disk (`max_retained_versions`).

---

## 7. Stream Logs

Tail process logs:

```bash
aegis logs my-api -f
```

Stream all events:

```bash
aegis stream-events
```

---

## 8. Launch the Terminal UI

Interactive terminal dashboard:

```bash
aegis-tui                         # daemon address from ./aegis.toml [daemon]
aegis-tui --addr 127.0.0.1:50051  # or point it at a daemon explicitly
```

The TUI (experimental) lists apps with status, release, uptime, restarts, CPU/memory. `Enter` opens an app (releases, deployments, log), `Tab` switches panes or the event stream, `/` filters, `f` follows or pauses, `?` lists all keys. `s`/`x`/`R` start, stop, restart; `d` deploys; `b` rolls back; each asks first. It reconnects by itself if the daemon stops.

---

## Configuration

Read from `aegis.toml` in the working directory:

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
