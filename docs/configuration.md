# Aegis Configuration

Aegis reads two kinds of settings from `aegis.toml` files:

- **Project settings** (`[project]`, `[build]`, `[deploy]`, `[env]`) live in your application's directory. They are copied into every release, so a release always runs with the settings it was built with, and a rollback uses the old release's settings.
- **Daemon settings** (`[daemon]`) tell the daemon where to listen and store data, and tell the CLI where to find the daemon.

`aegis init` generates the project sections, and `aegis validate` shows the effective settings after defaults are applied.

---

## 1. Project configuration (`./aegis.toml`)

```toml
[project]
id = "01910a3b-7f12-7890-8b01-123456789abc"   # written by `aegis init`; don't change it
name = "my-api"
runtime = "Node.js"

[build]
install_command = "npm ci --no-audit --no-fund"
build_command = "npm run build --if-present"
test_command = "npm test"                   # optional; skipped when empty
start_command = "npm start"
timeout_secs = 1800                         # per install/build/test command

[deploy]
strategy = "GracefulSwitch"                 # or "Immediate"
port = 3000                                 # exported to the app as PORT
health_check_url = "http://127.0.0.1:3000/health"
health_check_timeout_secs = 30
drain_timeout_secs = 10
max_retained_versions = 2
restart_policy = "always"                   # "always", "on-failure" or "never"

[env]
NODE_ENV = "production"
```

| Key | Default | Meaning |
| :--- | :--- | :--- |
| `build.install_command` | Detected (Node: `npm ci` with a lockfile, else `npm install`; Go: `go mod download`) | Runs first, in the release directory. An empty string skips it. |
| `build.build_command` | Detected | Runs after install. An empty string skips it. |
| `build.test_command` | None | Runs after build. A non-zero exit code fails the deployment. |
| `build.start_command` | Detected | Run with `sh -c` in the release directory, in its own process group. |
| `build.timeout_secs` | `1800` | Each install/build/test command is killed after this long. |
| `deploy.strategy` | `GracefulSwitch` | `GracefulSwitch` sends SIGTERM and waits up to `drain_timeout_secs`. `Immediate` doesn't wait. `Rolling` and `BlueGreen` need multiple instances behind a proxy, which is planned for roadmap Phase 5. |
| `deploy.port` | Detected | Exported as `PORT` unless `[env]` sets `PORT`. |
| `deploy.health_check_url` | `http://127.0.0.1:<port>/health` | Any HTTP response below 500 counts as healthy. Use `tcp://host:port` for a TCP-only check. With an empty string, the release counts as healthy if its process stays up for 3 seconds. |
| `deploy.health_check_timeout_secs` | `30` | How long a new release may take to become healthy before it is rolled back. |
| `deploy.drain_timeout_secs` | `10` | Grace period between SIGTERM and SIGKILL when stopping the app. |
| `deploy.max_retained_versions` | `2` | Release directories kept on disk for rollback, including the live one. |
| `deploy.restart_policy` | `always` | What the supervisor does when the app exits. A crash loop (more than 5 restarts within 2 minutes) stops restarting and marks the process `Failed`. |
| `[env]` | none | Environment variables for build commands and the app. |

Build commands also receive `AEGIS_RELEASE_VERSION`.

### What a deployment copies

- With `aegis deploy` in a project directory, Aegis copies that directory. In a git work tree it copies tracked files plus untracked files that aren't ignored, and always includes `aegis.toml`. Outside git, it skips `.git`, `node_modules`, `target`, virtualenvs, `__pycache__` and `*.db` files.
- With `aegis deploy --repo <url> [--branch b | --commit sha]`, Aegis clones the repository instead.

---

## 2. Daemon configuration

The daemon reads `aegis.toml` from its working directory, or the file named by the `AEGIS_CONFIG` environment variable. `install.sh --server` puts it in `/etc/aegis/aegis.toml`. The CLI reads `./aegis.toml`, or the file given with `aegis -c <file>`, to find the daemon.

```toml
[daemon]
host = "127.0.0.1"
port = 50051
database_path = "/var/lib/aegis/aegis.db"
log_level = "info"
data_dir = "/var/lib/aegis"      # releases, logs and artifact metadata
max_retained_versions = 2        # used when a project doesn't set its own
```

| Setting | Default |
| :--- | :--- |
| `daemon.host` | `"127.0.0.1"` |
| `daemon.port` | `50051` |
| `daemon.database_path` | `"aegis.db"` (relative to the daemon's working directory) |
| `daemon.log_level` | `"info"` |
| `daemon.data_dir` | `~/.aegis` |
| `daemon.max_retained_versions` | `2` |

### HTTP API (`[web]`)

```toml
[web]
enabled = false            # off by default
host = "127.0.0.1"
port = 8420
allowed_hosts = []         # public hostnames, e.g. ["aegis.example.com"]
behind_proxy = false       # HTTPS is terminated by a reverse proxy on this machine
session_idle_minutes = 120
session_max_hours = 24

[web.tls]                  # serve HTTPS directly (PEM files; renewals picked up automatically)
cert_path = "/etc/letsencrypt/live/aegis.example.com/fullchain.pem"
key_path = "/etc/letsencrypt/live/aegis.example.com/privkey.pem"
```

The daemon refuses to start if the API would be exposed unsafely: a non-loopback `host` without `[web.tls]`, public access without `allowed_hosts`, or half-configured TLS. `daemon.host` (gRPC) must be loopback. `aegis-daemon check-config` validates without starting. A configuration file that doesn't parse is an error, not silently ignored. See [security.md](security.md) and [http_api.md](http_api.md).

### Resource limits and metrics (`[resources]`)

```toml
[resources]
cgroups = "auto"            # "auto" (use cgroups when possible) or "off"
# cgroup_root = "/sys/fs/cgroup/my.slice/aegis"   # cgroup v2: use this directory instead of detecting
sample_interval_secs = 2    # how often apps and the host are measured
retention_days = 7          # per-minute metric rollups kept in the database
```

With `cgroups = "auto"`, the daemon picks the first of these that works:

- **cgroup v2 under systemd** with `Delegate=yes`, as `install.sh --server` sets up: app cgroups go under `<service cgroup>/apps`.
- **cgroup v2 as root:** `/sys/fs/cgroup/aegis`.
- **cgroup v1 as root:** `aegis/` under the daemon's cgroup in each controller.

Otherwise limits are unavailable (the dashboard says why), but CPU, memory, threads and open files are still measured from `/proc`. The daemon's startup log and `GET /api/v1/resources` show which backend is in use. Limits are set per app from the dashboard or `PUT /api/v1/projects/{p}/resources`, not in this file; they are stored as events, so they survive restarts.

### Notifications (`[notifications]`)

```toml
[notifications]
slack_webhook_url = "https://hooks.slack.com/services/T000/B000/XXXX"
```

When set, Aegis posts to Slack when:

- a deployment succeeds, fails or is rolled back;
- a rollback fails;
- an app crash-loops;
- a resource alert fires: out-of-memory kill, sustained CPU throttling, memory near its limit, or low disk.

### Data directory layout

```
<data_dir>/
├── projects/<project-id>/
│   ├── releases/<version>/      # one directory per release; the app runs from here
│   ├── current.json             # pointer to the live release
│   └── logs/
│       ├── <process-id>.log     # app stdout/stderr, rotated at 10 MiB
│       └── deploy-<id>.log      # output of each deployment's build commands
└── artifacts/releases/<release-id>/metadata.json   # content checksums
```

---

## 3. Planned, not yet implemented

- `AEGIS_*` environment variable overrides for individual settings (only `AEGIS_CONFIG` exists today).
- A global `~/.aegis/config.toml` merged underneath project settings.
- Built-in ACME (automatic Let's Encrypt). Use certbot with `[web.tls]`, or a reverse proxy.
