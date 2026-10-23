# Aegis Configuration

Aegis reads two kinds of settings from `aegis.toml` files:

- **Project settings** (`[project]`, `[build]`, `[deploy]`, `[run]`, `[env]`) live in your application's directory. They are copied into every release, so a release always runs with the settings it was built with, and a rollback uses the old release's settings.
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
strategy = "GracefulSwitch"                 # "Immediate", "Rolling" or "BlueGreen"
port = 3000                                 # exported to the app as PORT
health_check_url = "http://127.0.0.1:3000/health"
health_check_timeout_secs = 30
drain_timeout_secs = 10
max_retained_versions = 2
restart_policy = "always"                   # "always", "on-failure" or "never"
rolling_batch = 1                           # Rolling: new instances per step
blue_green_keep_secs = 300                  # BlueGreen: how long the old set stays up

[run]
instances = 1                               # 1-16; more than 1 needs a domain on the proxy

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
| `deploy.strategy` | `GracefulSwitch` | `GracefulSwitch` sends SIGTERM and waits up to `drain_timeout_secs`. `Immediate` doesn't wait. For apps with a domain on the built-in proxy, `GracefulSwitch` is zero-downtime (see `[proxy]` below). `Rolling` and `BlueGreen` need a domain on the built-in proxy; see [Deployment strategies](#deployment-strategies-and-instances). `aegis deploy` sends this value; deployments started elsewhere (dashboard, webhook, schedule) use the dashboard's *Default strategy*, else `GracefulSwitch`. Rollbacks use the target release's value. |
| `deploy.rolling_batch` | `1` | `Rolling`: how many new instances start (and old ones stop) per step, 1-16. |
| `deploy.blue_green_keep_secs` | `300` | `BlueGreen`: how long the previous release's instances keep running after the switch, as an instant rollback target. `0` stops them right away. At most 86400. |
| `run.instances` | `1` | Copies of the app to run, 1-16, each with its own `PORT`, log file, restart policy and resource limits. More than 1 needs a domain on the built-in proxy, which spreads requests over them. |
| `deploy.port` | Detected | Exported as `PORT` unless `[env]` sets `PORT`. Behind the built-in proxy each release gets its own `PORT` instead. |
| `deploy.health_check_url` | `http://127.0.0.1:<port>/health` | Any HTTP response below 500 counts as healthy. Use `tcp://host:port` for a TCP-only check. With an empty string, the release counts as healthy if its process stays up for 3 seconds. |
| `deploy.health_check_timeout_secs` | `30` | How long a new release may take to become healthy before it is rolled back. |
| `deploy.drain_timeout_secs` | `10` | Grace period between SIGTERM and SIGKILL when stopping the app. |
| `deploy.max_retained_versions` | `2` | Release directories kept on disk for rollback, including the live one. |
| `deploy.restart_policy` | `always` | What the supervisor does when the app exits. A crash loop (more than 5 restarts within 2 minutes) stops restarting and marks the process `Failed`. |
| `[env]` | none | Environment variables for build commands and the app. |

### Containers (`[run]`)

An app runs as a plain process unless `[run] runtime = "docker"` is set. Then it runs in a container built from its Dockerfile (or from a prebuilt `image`), with the same health checks, cutover, rollback, restart policy, logs and resource sliders. See the [Docker runtime guide](docker_runtime.md) for how it works.

```toml
[run]
runtime = "docker"              # "process" (default) or "docker"
dockerfile = "Dockerfile"       # default; relative to the app's root. Or: image = "nginx:1.27"
container_port = 8080           # default: deploy.port; exported to the container as PORT
# command = ["node", "server.js"]
# user = "1000:1000"
# read_only = true
# tmpfs = ["/tmp"]
# volumes = ["data:/var/lib/app"]   # named volumes only, scoped to this app
# init = true
# pull = "missing"                  # with image: "missing" or "always"
```

| Key | Default | Meaning |
| :--- | :--- | :--- |
| `run.runtime` | `process` | `process` or `docker`. The other `[run]` keys are only accepted with `docker`. |
| `run.dockerfile` | `Dockerfile` | Built with the release directory as context. Must stay inside the release. |
| `run.image` | none | A prebuilt image instead of a build (exclusive with `dockerfile`). It is tagged per release, so rollbacks run exactly the same image. |
| `run.pull` | `missing` | With `image`: pull only when missing, or `always`. |
| `run.container_port` | `deploy.port` | The port inside the container. It is published on `127.0.0.1:<deploy.port>` (or the release's own port behind the proxy). |
| `run.command` | image `CMD` | An argument list that replaces the image's command. |
| `run.user` | image user | `name`, `uid` or `uid:gid`. |
| `run.read_only` | `false` | Read-only root filesystem. |
| `run.init` | `true` | `--init` (signal forwarding, zombie reaping). |
| `run.tmpfs` | none | In-memory directories (absolute paths, no options). |
| `run.volumes` | none | `name:/path[:ro]`. These are named volumes `aegis-<project id>-<name>` that keep data across releases. Host paths are refused. |

There is no setting for privileged mode, host networking, extra capabilities, devices or bind mounts, and unknown `[run]` keys are rejected. For container apps, only explicitly set `[build]` commands run on the host (before the image build), `build.start_command` is ignored, and the default health check is `http://127.0.0.1:<port>/`.

### Variables and secrets from the dashboard

- **Variables** set in an app's Settings tab (or `PUT /api/v1/projects/{p}/settings` with `env`) are merged over `[env]` and written into each new release's `aegis.toml`, like the other dashboard settings. Every role can read them (they are part of the app's settings and its `aegis.toml`), and they are available to build commands.
- **Secrets** (Settings → Secrets, or `PUT /api/v1/projects/{p}/secrets/{NAME}`) are stored exactly as given, line breaks included (a PEM private key can be pasted as it is; the dashboard's value field is a masked multi-line box). They are stored encrypted in the database with a key kept in `<data_dir>/secrets.key` (created on first start, mode 0600). They are passed to the app when its process starts and override variables of the same name; they are never written into a release, an event or a log. After changing one, restart the app to apply it. Secrets are **not** available to build commands: anything the build reads (for example `NEXT_PUBLIC_*` variables, which Next.js inlines at build time) must be a Variable. Container apps (`[run] runtime = "docker"`) can't receive values with line breaks, because Docker's env file can't hold them; the start fails with a message saying so (store such a value base64-encoded). Only signed-in users can manage secrets; API tokens cannot. `PORT` is reserved.
- **Back up `secrets.key` together with `aegis.db`.** Without it the secrets can't be decrypted, and the daemon refuses to start rather than run apps without them; delete the `secrets` table rows (or restore the key) to recover.
- **An env file on the server** (Settings → Environment → *Env file path*) can supply the values underneath all of these; see the next section.
- **Import .env** (Settings → Environment) turns an existing `.env` into Variables and Secrets instead of retyping them; see [the dashboard's Settings tab](dashboard/settings.md#environment).

Build commands also receive `AEGIS_RELEASE_VERSION`.

### Env file on the server

An app can read a `.env` file that already exists on the server, for example the one PM2 used, instead of (or underneath) dashboard Variables and Secrets. Set its absolute path in the dashboard (**Settings → Environment → Env file path**) or with `PUT /api/v1/projects/{p}/settings` and `"env_file": "/opt/apps/my-app/.env"` (like every field there, a settings request without it clears it). It is a dashboard/API setting only, not an `aegis.toml` key: what is in the repository can't choose which server file an app reads.

- **When it is read:** at every build (so values such as `NEXT_PUBLIC_*` reach `next build`) and every time the app's process starts or restarts. Edit the file, then restart the app (or deploy) to apply it. Its values are never stored: not in the database, a release's `aegis.toml`, an event, the build log (which lists only the names it read) or an API response.
- **Where it may be:** only inside a directory listed in the daemon configuration's `[apps] env_file_dirs` (below; empty by default, which turns env files off). Symbolic links are resolved first, `..` is refused, and the file must be a regular file of at most 1 MiB, readable by the daemon's user. Otherwise the setting is refused when saved (outside the allowed directories) or the build or start fails with a message saying what is wrong, for example `aegis can't read /opt/apps/my-app/.env: permission denied. Give the daemon's user read access, e.g. sudo chgrp aegis FILE && sudo chmod 640 FILE`. A missing file can be saved (create it afterwards); until then builds and starts fail rather than run without its values.
- **Syntax:** the usual dotenv format. `NAME=value` per line; `export NAME=value`; `#` comments and blank lines; unquoted values are trimmed and end at a ` #` comment (a `#` without a space before it is part of the value, as in URLs); `'single quotes'` are literal; `"double quotes"` understand `\n`, `\r`, `\t`, `\"`, `\\` and `\$`; both kinds of quotes may span several lines (a PEM key pasted as it is). There is no `${VAR}` expansion. Later lines win over earlier ones. An invalid line stops the read with an error naming the file and line number, never the value.
- **`PORT` in the file is ignored**: Aegis sets `PORT` (from `deploy.port`, `[env]`, or the proxy's per-release port).
- **Container apps** get the file's values like secrets, through the 0600 env file Aegis hands to `docker run`; Docker can't pass values with line breaks, so such a value makes the start fail with a message naming it (store it base64-encoded). The values reach host-side build commands, not `docker build`.
- The dashboard shows the path, whether it is readable right now (and why not), the names it defines and which of them Variables or Secrets override, never the values (`GET /api/v1/projects/{p}/env`; every role can read names). Changing the path is recorded as `ProjectSettingsUpdated` with the path only.

**Precedence**, lowest first (a later source overrides an earlier one with the same name):

| # | Source | Build commands | App process | Stored |
| :--- | :--- | :--- | :--- | :--- |
| 1 | Env file on the server | yes | yes | no, read each time |
| 2 | `[env]` in the release's `aegis.toml` | yes | yes | in the release |
| 3 | Dashboard Variables | yes | yes | in the settings and each new release's `aegis.toml` |
| 4 | Dashboard Secrets | **no** | yes | encrypted in the database |
| 5 | `PORT` | no | yes (always Aegis's) | |

Builds also get `AEGIS_RELEASE_VERSION`. A name that is both a Secret and in the env file has the env file's value during the build and the secret's value when the app runs: anything the build needs must come from 1-3. Secrets are never given to builds, because build output (`<data_dir>/projects/<id>/logs/deploy-<id>.log`) is shown in the dashboard and a build command can print its environment; that applies to env file values too, so don't put values in an env file that a build script echoes and that deployers shouldn't read.

### What a deployment copies

- With `aegis deploy` in a project directory, Aegis copies that directory. In a git work tree it copies tracked files plus untracked files that aren't ignored, and always includes `aegis.toml`. Outside git, it skips `.git`, `node_modules`, `target`, virtualenvs, `__pycache__` and `*.db` files.
- With `aegis deploy --repo <url> [--branch b | --commit sha]`, Aegis clones the repository instead.
- An app with an **app folder** (`source_subdir`, e.g. `apps/web`; set when the app is created in the wizard, by a folder scan, or with `"source_subdir"` in `POST /api/v1/projects`) builds and runs only that folder of its repository or directory: the clone (or copy) is made in full, then only the folder becomes the release. Files outside it (a monorepo's root lockfile, shared workspace packages) are not part of the release, so the folder must build on its own. The path is relative, without `..`; a folder that doesn't exist (or a symbolic link leading out of the checkout) fails the Clone stage. A deployment that copies an explicitly given directory (`aegis deploy` in a project directory) is not narrowed.
- **Private repositories** are cloned with the app's SSH deploy key (Settings → Repository; see [`security.md`](security.md#deploy-keys)). With a key, a GitHub HTTPS URL is cloned over SSH (`git@github.com:owner/repo.git`), because deploy keys only work there; SSH URLs (`ssh://git@host/owner/repo.git`, `git@host:owner/repo.git`) are used as they are. Webhook and scheduled deployments use the key too.
- A `.env` in the project directory is part of the release only if it would be copied by these rules (for example, not git-ignored). Use an [env file on the server](#env-file-on-the-server) to keep it out of releases while the app still gets its values.

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

- **cgroup v2 under systemd** with `Delegate=yes`, as `install.sh --server` sets up: the daemon moves itself into `<service cgroup>/daemon` and app cgroups go under `<service cgroup>/apps`. A service cgroup that also holds other processes (the daemon was started by hand, e.g. with `sudo`, from inside some other service) is left alone.
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

### Domains and the built-in reverse proxy (`[proxy]`)

```toml
[proxy]
enabled = false            # off by default
host = "0.0.0.0"           # address both listeners bind to
http_port = 80             # HTTP, ACME challenges, redirects to HTTPS
https_port = 443           # HTTPS; certificates chosen by SNI
trusted_proxies = []       # front proxies whose X-Forwarded-Proto/-Host are kept, e.g. ["127.0.0.1", "::1"]
response_timeout_secs = 120    # how long an app may take to send response headers (1-86400)
connect_timeout_secs = 5       # how long to wait for a connection to the app (1-300)

[proxy.acme]               # automatic certificates
enabled = false
email = "ops@example.com"  # optional contact for the CA
directory_url = "production"   # "production" or "staging" (Let's Encrypt), or an ACME directory URL

[proxy.dashboard]          # optional: the dashboard and API on their own domain
domain = "aegis.example.com"
tls = "acme"               # or "files" with cert_path / key_path
```

With the proxy on, the daemon serves each app on the domains mapped to it (dashboard Settings tab, or `POST /api/v1/projects/{p}/domains`). Requests are routed by `Host` to the app's live process on `127.0.0.1`. Unknown hosts get a 404 page, and an app that isn't running gets a 502. The proxy:

- forwards `X-Forwarded-For` (the address of the connecting peer is appended, so apps should trust only the entries added by proxies they trust), `X-Forwarded-Proto` and `X-Forwarded-Host`, and keeps the original `Host`. `X-Forwarded-Proto` and `X-Forwarded-Host` are set by the proxy and anything the client sent is replaced, unless the request comes from a [trusted front proxy](#running-behind-another-reverse-proxy-nginx-caddy);
- removes hop-by-hop headers;
- passes WebSocket and other `Upgrade` connections through;
- streams request and response bodies;
- gives up on connecting to the app after `connect_timeout_secs` (default 5 s, then 502) and on waiting for its response headers after `response_timeout_secs` (default 120 s, then 504). Both apply to every route. Once the headers have arrived the body streams for as long as it takes, and an upgraded (WebSocket) connection stays open for as long as both sides keep it open: the response timeout only covers the upgrade handshake.

Each domain has its own TLS mode:

| Mode | Certificate |
| :--- | :--- |
| `acme` | Requested from the ACME CA with the HTTP-01 challenge, which the proxy answers on `http_port` (so port 80 must be reachable from the internet and DNS must point here). Stored under `<data_dir>/certs/<domain>/`: `key.pem` is mode 0600 and the directory 0700. Renewed in the background when fewer than 30 days remain. A failed request is retried after an hour. |
| `files` | PEM chain and key at absolute paths you manage (e.g. certbot). They are checked when the domain is mapped and reloaded within a minute of changing. |
| `off` | Plain HTTP only. |

Once a domain has a certificate, HTTP requests to it are redirected (308) to HTTPS. ACME challenges (`/.well-known/acme-challenge/...`) are always answered on the plain HTTP listener and never redirected.

#### Running behind another reverse proxy (nginx, Caddy)

If another web server already owns ports 80 and 443 on the machine, run the Aegis proxy on loopback ports and let the front proxy forward to it:

```toml
[proxy]
enabled = true
host = "127.0.0.1"
http_port = 8081
https_port = 8444          # must be set and free, but nothing needs to reach it
trusted_proxies = ["127.0.0.1", "::1"]
```

Map the app's domains with TLS `off`: the front proxy terminates HTTPS, so Aegis needs no certificate. In nginx:

```nginx
location / {
    proxy_pass http://127.0.0.1:8081;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_set_header X-Forwarded-Host $host;
    # WebSockets
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection $connection_upgrade;   # map $http_upgrade $connection_upgrade { default upgrade; '' close; }
    proxy_read_timeout 300s;   # keep at least response_timeout_secs if apps are slow
}
```

`trusted_proxies` is a list of IP addresses and CIDR ranges (`"10.0.0.0/8"`, `"fd00::/8"`). It is empty by default. When a request's TCP peer is in the list:

- `X-Forwarded-Proto` and `X-Forwarded-Host` are passed to the app as the front proxy sent them, if they are well formed: the protocol must be exactly `http` or `https`, the host a host name or IP address with an optional port, each sent once. Anything else is ignored and the proxy's own values are used.
- `X-Forwarded-For` is kept and the peer's address is appended, as for every request.
- `X-Forwarded-Proto: https` counts as HTTPS, so a domain with a certificate (`acme` or `files`) is not redirected again (no redirect loop). A request the front proxy marks as `http` is redirected to `https://<domain>/...` without a port, because the public HTTPS port is the front proxy's.

From any other peer, both headers are replaced, so clients can't spoof them, and the HTTPS redirect is decided by the listener the request arrived on. With `host = "127.0.0.1"` only processes on the machine can connect at all. Host names are refused (list addresses), and so are `0.0.0.0/0` and `::/0`, which would trust everyone. Entries outside loopback are accepted with a warning at startup: anything that can connect from those addresses can claim to be HTTPS. See [Security](security.md#forwarded-headers-and-trusted-proxies).

**Zero-downtime deployments.** For an app with at least one domain, each release gets its own free local port, passed to the app as `PORT`. This overrides `deploy.port` and any `PORT` in `[env]`, so the app must listen on `$PORT`. If the health check URL names the app's configured port, it is probed on the release's port instead. A URL naming another port is left alone.

A deployment (or rollback):

1. starts the new release while the old one keeps serving;
2. waits until the new release is healthy;
3. switches the proxy to it in one step;
4. lets the old process finish its in-flight requests (up to `drain_timeout_secs`), then stops it.

If the new release never becomes healthy, it is stopped and the old one goes on serving untouched. Apps without a domain keep the previous behaviour: they use their configured port, and the old process stops before the new one starts. The port of every process is recorded in its events, so routes come back on a daemon restart.

**Ports and health checks.** A health check only proves something if the new release is the one answering it. So, for every strategy and runtime (including a container's published port):

- Before a release's process starts, its port must be free: nothing bound to it on 127.0.0.1, 0.0.0.0 or `[::]`, and nothing accepting connections on 127.0.0.1 (a just-stopped release, or its container, gets 3 seconds to let go of it). Otherwise the deployment fails at the Promote stage, or the Start/Restart is refused, with for example `port 4000 is already in use by another process (pid 812 (node)); map a domain so each release gets its own port, or stop the other process`. The process is named when the daemon's user can see it (not for another user's processes; `sudo ss -ltnp 'sport = :4000'` shows those). Nothing is promoted. This is what happens when an app's previous manager (PM2, a systemd unit) still runs it on the same port.
- After the health check passes, the release must keep running, without exiting or being restarted, for the liveness window (3 seconds, or the health check timeout if shorter). A release that crashes in that window fails the deployment, and the previous release is started again.
- On Linux, for process apps, the socket listening on the probed port must belong to the release's process tree (matched by socket inode in `/proc/net/tcp*` and `/proc/<pid>/fd`). A health check answered by another process fails the deployment. Container apps are reached through Docker's port publishing, so this last check doesn't apply to them; the first two do.

### Deployment strategies and instances

*Experimental.* An app can run several **instances** (`[run] instances = N`, or *Instances* in the dashboard's Settings tab), up to 16. Each instance is a supervised process of the live release with its own `PORT`, its own log file (`<data_dir>/projects/<id>/logs/<process-id>.log`), the app's restart policy, and its own cgroup. **Resource limits apply to each instance on its own**: a 1-core, 512 MiB limit lets three instances use up to 3 cores and 1.5 GiB in total. The app's usage figures are the sum over its instances, and the dashboard lists each instance with its own CPU and memory. Limit changes apply to every running instance at once.

Several instances need the built-in proxy: `[proxy] enabled = true` and a domain mapped to the app. The proxy sends requests to the running instances in turn (round robin). An instance that refuses a connection is skipped for 5 seconds; a request without a body that hit it is retried on another instance, so it doesn't fail.

| Strategy | What a deployment does | Needs the proxy |
| :--- | :--- | :---: |
| `GracefulSwitch` (default) | Starts the full new set next to the old one, switches the proxy to it in one step once every new instance is healthy, then drains and stops the old set. | only for more than 1 instance |
| `Immediate` | Like `GracefulSwitch` behind the proxy, but without draining the old set. Without the proxy, stops the old process before starting the new one. | only for more than 1 instance |
| `Rolling` | Replaces instances `rolling_batch` at a time: start new ones, wait until they are healthy, add them to the proxy, then take old ones out of the proxy, let them finish their requests (up to `drain_timeout_secs`) and stop them. Each step is recorded (`RolloutStepCompleted`). If a new instance doesn't become healthy, the rollout stops, the old instances already stopped are started again (same ports), the proxy goes back to the old set, and the new instances are drained and stopped. | yes |
| `BlueGreen` | Starts the full new set, switches the proxy in one step, and keeps the old set running for `blue_green_keep_secs`. A rollback to that release in the window is an instant switch back to the still-running processes; no build or start. The old set is stopped when the window ends, when another release goes live, or when the app is stopped. | yes |

Rolling and BlueGreen (and more than one instance) are refused, with a message saying what is missing, when the dashboard settings are saved, when a deployment is queued with that strategy, and when a release asking for more than one instance is about to go live. A refused deployment leaves the running app alone. Instance counts and strategies take effect with the next deployment; each release keeps the values it was built with (so a rollback restores the instance count of that release).

Other operations on an app with several instances:

- **Restart** restarts one instance at a time: out of the proxy, drain, restart, wait until healthy, back in. The others keep serving. A restart of one process id (`/processes/<id>/restart`) restarts only that instance.
- **Stop** and **Start** act on every instance.
- **Daemon restart**: the recorded live set comes back, each instance on its previous port when it is free. If fewer instances are running than the live release asks for (for example after a rollout the daemon didn't finish), new ones are started and recorded (`InstanceSetChanged`). A deployment interrupted by a daemon stop is marked failed, and the instances of the previous release, including any the rollout had already replaced, are started again. A BlueGreen standby does not survive a daemon restart.
- Scheduled deployments and webhooks use the app's default strategy like any other deployment.

**Ports 80 and 443** need root or `CAP_NET_BIND_SERVICE`; the systemd unit installed by `install.sh --server` grants the capability.

**`[proxy.dashboard]`** puts the dashboard behind the proxy on its own domain, always over HTTPS. It requires `[web] enabled = true` without `[web.tls]` (keep `web.host` on loopback). The domain is added to `web.allowed_hosts`, and the proxy is trusted for `X-Forwarded-For` and Secure cookies.

The daemon refuses a proxy configuration that:

- uses the same port twice, or a port of the HTTP or gRPC API;
- has an invalid `host`, ACME email or directory URL;
- sets up a dashboard domain without HTTPS;
- has a `trusted_proxies` entry that is not an IP address or CIDR range, or that covers every address;
- has `response_timeout_secs` outside 1-86400 or `connect_timeout_secs` outside 1-300.

The last two are checked even while the proxy is disabled.

### Backups (`[backup]`)

```toml
[backup]
enabled = false            # scheduled backups; manual ones work either way
interval_hours = 24        # time between scheduled backups (1-8784), counted from the newest backup
daily_at = "03:30"         # optional: once a day at this UTC time instead of interval_hours
keep = 7                   # backups kept in <data_dir>/backups (1-1000); older ones are deleted
include_releases = true    # false: database, secrets key and certificates only
```

A backup is one `.tar.gz` in `<data_dir>/backups` (directory 0700, archives 0600) with a consistent snapshot of the database (SQLite `VACUUM INTO`, safe while the daemon runs), `secrets.key`, `certs/` and, unless excluded, every release directory with its `current.json` and the release metadata in `artifacts/` (rollbacks need it). Logs are not included. Retention counts every archive in the directory, whether it was scheduled, made in the dashboard or with `aegis-daemon admin backup`. A scheduled backup that was missed while the daemon was down runs about a minute after it starts. Restoring is offline only: see [`disaster_recovery.md`](disaster_recovery.md).

The daemon holds `<data_dir>/aegis.lock` (containing its pid) while it runs, so two daemons can't share a data directory and a restore can't run under a live daemon.

### Update checks (`[update]`)

```toml
[update]
check = true               # ask GitHub for the latest stable release; false = no update requests at all
interval_hours = 12        # time between checks (1-744); the first is about a minute after start
repo = "shivam411/Aegis"   # owner/name of the repository releases come from (for forks)
```

The result is shown to every signed-in user (`GET /api/v1/system/update`, the dashboard banner); admins can check now and upgrade. With `check = false`, checking and upgrading from the dashboard are refused. How an upgrade is installed, and what happens to running apps, is in [`upgrading.md`](upgrading.md).

`AEGIS_UPDATE_API_URL` points checks at a fake GitHub API for tests. Only `http(s)://` URLs on a loopback address are accepted (anything else stops the daemon at startup), and release downloads must then come from that same address.

### App settings (`[apps]`)

```toml
[apps]
env_file_dirs = ["/opt/apps", "/etc/aegis/env"]   # where apps' env files may be; default [] = env files off
import_dirs = ["/opt/github"]                     # where "Scan a folder on the server" may look; default []
```

`import_dirs` lists the folders the new app wizard's **Scan a folder on the server** (`POST /api/v1/apps/scan`) may look in for apps to import. The folders in `env_file_dirs` may be scanned too, so a server that already lists `/opt/github` there needs nothing more. With both empty (the default) scanning is off. The same rules apply as for `env_file_dirs`: absolute paths without `..`, `/` refused, symbolic links resolved before the check (a link inside an allowed folder that leads outside it is not followed), and only what the daemon's user can read is seen. A scan goes at most two levels below the folder it is given and looks at no more than 5000 directory entries; it skips `node_modules`, `.git`, `target`, `dist`, `build`, `.next`, `.nuxt`, virtualenvs and hidden folders, and doesn't search inside an app it found (except `apps/*`, `packages/*` and `services/*` of a monorepo). What it reads is listed in [`security.md`](security.md#scanning-folders-on-the-server). Scanning and importing are for signed-in admins only.

`env_file_dirs` lists the directories an app's env file ([Env file on the server](#env-file-on-the-server)) may be in, at any depth, after symbolic links are resolved. Each must be an absolute path without `..`, and `/` is refused. Only admins can point an app at a file, but any file the daemon's user can read in these directories can be chosen, so list only directories that hold env files for apps on this server. A directory that doesn't exist is logged at startup and allows nothing.

With `install.sh --server` the daemon runs as `aegis` under `ProtectSystem=strict` and `ProtectHome=read-only`: files outside `/var/lib/aegis` are readable (subject to their permissions) but not writable, so env files in `/opt` or `/etc` work without changing the unit. The `aegis` user needs read permission on the file and execute (search) permission on every directory above it:

```bash
sudo chgrp aegis /opt/apps/my-app/.env
sudo chmod 640 /opt/apps/my-app/.env
namei -l /opt/apps/my-app/.env      # every directory needs x for aegis (via group or other)
```

Alternatively keep env files in a directory only root and `aegis` can read: `sudo install -d -m 750 -o root -g aegis /etc/aegis/env`, then `sudo install -m 640 -o root -g aegis /opt/apps/my-app/.env /etc/aegis/env/my-app.env`.

### Git servers (`[git]`)

```toml
[git]
known_hosts_file = "/etc/aegis/known_hosts"   # SSH host keys of git servers other than GitHub; default: none
```

Deploy keys are used over SSH with `StrictHostKeyChecking=yes`: a server whose host key Aegis doesn't know is refused, never trusted on first use. GitHub's published host keys (Ed25519, ECDSA and RSA, from [GitHub's SSH key fingerprints](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/githubs-ssh-key-fingerprints)) are built in. For another server (GitLab, Gitea, a self-hosted git), put its keys in a file in `known_hosts` format and name it here; it is read at every clone, so it can be edited without a restart. Get the keys from the server's administrator, or with `ssh-keyscan -t ed25519 git.example.com` and compare the fingerprint (`ssh-keygen -lf`) with one from a trusted source. The path must be absolute and readable by the daemon's user. The system-wide `/etc/ssh/ssh_known_hosts` is consulted too, as ssh always does.

### Data directory layout

```
<data_dir>/
├── aegis.lock                   # held by the running daemon
├── secrets.key                  # encryption key for app secrets and deploy keys (0600); back it up with the database
├── run/ssh/key-<random>/        # a deploy key (0600) and known_hosts while one git command runs (0700); removed after it
├── backups/                     # aegis-backup-<timestamp>.tar.gz (0700 / 0600)
├── update/                      # state.json (last check, pending update); request.json/result.json for the root helper
├── pre-restore-<timestamp>/     # what a restore replaced; delete when no longer needed
├── projects/<project-id>/
│   ├── releases/<version>/      # one directory per release; the app runs from here
│   ├── current.json             # pointer to the live release
│   └── logs/
│       ├── <process-id>.log     # app stdout/stderr, rotated at 10 MiB
│       └── deploy-<id>.log      # output of each deployment's build commands
├── artifacts/releases/<release-id>/metadata.json   # content checksums
└── certs/                       # [proxy] ACME account(s) and certificates (0700)
    └── <domain>/cert.pem, key.pem   # key.pem is 0600
```

---

## 3. Planned, not yet implemented

- `AEGIS_*` environment variable overrides for individual settings (only `AEGIS_CONFIG` exists today).
- A global `~/.aegis/config.toml` merged underneath project settings.
- Least-connections balancing and active health checks of running instances (the proxy balances round robin and only skips instances that refuse connections).
- Wildcard domains and the DNS-01 challenge.
