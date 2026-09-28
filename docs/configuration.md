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

The daemon reads `aegis.toml` from its working directory, or the file named by the `AEGIS_CONFIG` environment variable. The CLI reads `./aegis.toml`, or the file given with `aegis -c <file>`, to find the daemon.

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
- `[web]` settings for the web control plane (see [roadmap_web_control_plane.md](roadmap_web_control_plane.md)).
