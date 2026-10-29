# HTTP API

The daemon can serve a JSON API alongside gRPC. It exposes everything the CLI does: projects, deployments, rollbacks, process control, logs, schedules and events, plus live **Server-Sent Events** streams. The web dashboard, served at `/` on the same port, is built entirely on it.

The full schema is in [`openapi.json`](openapi.json). The running daemon also serves it at `/api/v1/openapi.json`.

## Enabling it

```toml
# aegis.toml read by the daemon
[web]
enabled = true
host = "127.0.0.1"
port = 8420
```

`aegis status` shows the URL once it's running. To reach it from outside the server, see [security.md](security.md#exposing-the-web-api) (SSH tunnel, reverse proxy, or TLS).

## Authentication

Every `/api/v1` route except sign-in requires one of:

- **A session.** Sign in, keep the cookie, and send the CSRF token on changes:

  ```bash
  curl -c jar -H 'Content-Type: application/json' \
       -d '{"username":"admin","password":"…"}' "$API/auth/login"
  # {"kind":"user","name":"admin","csrf_token":"…","expires_at":…}
  curl -b jar -H 'X-CSRF-Token: …' -H 'Content-Type: application/json' -d '{}' \
       -X POST "$API/processes/shop/restart"
  ```

- **An API token**, for scripts and CI:

  ```bash
  curl -H "Authorization: Bearer aegis_…" "$API/projects"
  ```

The first admin password is in `<data_dir>/initial-admin-password`. Users have a role (`viewer`, `deployer` or `admin`; returned as `role` by login and `GET /auth/session`): viewers read, deployers also deploy, roll back and start/stop/restart, and admins change everything else. A request beyond the user's role gets **403**. Roles, scopes, expiry, throttling and everything else are described in [security.md](security.md).

Requests that change state must also send `Content-Type: application/json` (an empty `{}` body is fine), and the `Host` header must be a loopback name or one listed in `web.allowed_hosts`.

## Endpoints

All paths are under `/api/v1`. Wherever a path takes `{project}`, you can use the project id or its name. Wherever it takes `{target}`, you can use a process id or a project (meaning the project's live process; for start, stop and restart of an app with several instances, all of them, restarted one at a time).

| Method & path | Does |
| :--- | :--- |
| `GET /status` | Version, project count, running apps, event count |
| `GET /resources` | Whether this host can enforce limits (`backend`: cgroup2, cgroup1 or none, with a `reason`), host size, and the limits already promised to apps |
| `GET /projects/{project}/resources` | The app's limits and its latest usage sample |
| `PUT /projects/{project}/resources` | Set limits, applied at once without a restart: `{"cpu_cores"?, "memory_bytes"?, "pids_max"?, "force"?}` (omitted = unlimited). A memory limit below current use, or more memory promised than the host has, returns 409 `confirmation_required` unless `force` is true |
| `GET /projects/{project}/metrics?range=&points=` | CPU and memory history with the limits in effect (`range`: e.g. `5m`, `1h`, `24h`, `7d`; up to an hour at 2 s resolution, longer from per-minute rollups) |
| `GET /system/prometheus` · `PUT /system/prometheus` | Whether `GET /metrics` answers / turn it on or off: `{"enabled"}` (admins, signed in). Off by default |
| `GET /system/grafana-dashboard` | A Grafana dashboard (JSON file) for the metrics. See [monitoring](dashboard/monitoring.md) |
| `GET /host/metrics?range=&points=` | The same for the whole host |
| `GET /metrics/stream?project=&host=` | **SSE**: a `sample` message per app (and the host) every sample interval |
| `GET /host` | Host CPU %, memory, swap, disk (the data directory's filesystem) and load average, sampled every 5 s |
| `GET /system/update` | Whether a newer Aegis release is available (cached result of the periodic check), how it would be installed here (`mode`), the release signing keys this build trusts (`release_keys`), and update progress (`phase`) |
| `POST /system/update/check` | Admins, signed in (not API tokens): ask GitHub now. Refused (409) with `[update] check = false` |
| `POST /system/update/apply` | Admins, signed in: back up, download, verify (release signature, then SHA-256) and install the latest release, then restart (202; follow `phase`). `{"version"?}` must match the latest known release. 409 with the command to run when the daemon can't replace its own binary, or when this build has no release signing key. See [upgrading.md](upgrading.md) |
| `POST /detect` | Detect runtime and default commands: `{"source_dir"}` or `{"repository_url", "branch"?}` (shallow clone into scratch space), plus `"source_subdir"?` (look inside that folder) and `"project"?` (an existing app: clone with its deploy key, default to its folder). `start_hint` is set when the package.json start script hard-codes its port: `{"port", "script", "suggested_start"}` (e.g. `npx next start -p $PORT`) |
| `GET /projects` | Projects (only those the caller may see; `protected` is true for apps hidden behind a password) with their live release, schedule and process, plus `instances` (every instance of the live release with its process, CPU and memory; one entry for single-instance apps) and `standby` (a BlueGreen previous release kept running, or `null`) |
| `POST /projects` | Register a project: `{"name", "source_dir"?, "repository_url"?, "branch"?, "project_id"?, "source_subdir"?}`. `source_subdir` (e.g. `apps/web`, relative, no `..`) builds and runs only that folder of the source |
| `GET /projects/{project}` | One project |
| `DELETE /projects/{project}?confirm={name}&keep_releases=` | Delete the app. `confirm` must repeat its name (or id), else **400**. Stops all its processes (instances and a BlueGreen standby; taken out of the proxy first, then given the drain timeout to finish requests), stops serving its domains, and deletes its secrets, deploy key, webhook secret, env-file setting, daily deploy, resource limits, Docker images and (unless `keep_releases=true`) its release directories, logs and artifact metadata. ACME certificates already issued are kept on disk. Records `ProjectDeleted`; its earlier events stay. Its name and domains can be used again at once. Returns `{"project_id", "name", "stopped_processes", "domains", "secrets_removed", "releases_kept", "kept_dir"}`. **409** while a deployment of it is queued or running. Admins, signed in (API tokens get 403) |
| `PUT /projects/{project}/source` | Change where the app's code comes from: `{"repository_url", "branch"?, "source_subdir"?}` or `{"source_dir", "source_subdir"?}` (exactly one of the two). Validated like `POST /projects` (the folder must exist on the server; URL, branch and subfolder must be safe). A missing `branch` keeps the app's branch. Takes effect at the next deployment; the deploy key is kept. Records `ProjectSourceChanged`. Admins |
| `POST /projects/{project}/deploy` | Queue a deployment (**202**): `{"version"?, "strategy"?, "branch"?, "commit"?, "source_dir"?, "repository_url"?}`. `strategy` is `GracefulSwitch`, `Immediate`, `Rolling` or `BlueGreen`; the last two need a domain on the built-in proxy (else **400**). |
| `POST /projects/{project}/rollback` | Switch to the previous (or `{"version"}`) release; returns once it's live |
| `POST /projects/{project}/verify` | Request a URL once and say whether it is up: `{"url"?, "rollback"?}` (the app's `verify_url` when `url` is omitted). Returns `{"url", "ok", "status", "error", "error_code", "elapsed_ms", "expected", "rolled_back_to", "rollback_error"}`; with `rollback: true` a URL that is not up puts the previous release back. Deployers. The same check runs automatically after each deployment when `verify_url` is set (see [configuration](configuration.md)) |
| `PUT /projects/{project}/schedule` | Daily auto-deploy: `{"hour", "minute"?, "branch"?}` (UTC) |
| `PUT /projects/{project}/settings` | Override aegis.toml for future deployments: `{"install_command"?, "build_command"?, "test_command"?, "start_command"?, "port"?, "health_check_url"?, "health_check_timeout_secs"?, "drain_timeout_secs"?, "strategy"?, "instances"?, "env"?, "env_file"?}`. `verify_url`, `verify_timeout_secs` and `verify_expect` configure the live URL check that rolls a release back when its URL isn't up. `health_check_timeout_secs` (1-3600) is how long a new release may take to become healthy, `drain_timeout_secs` (0-3600) how long a stopping release gets to finish requests (`[deploy]` keys of the same names; default 30 and 10); else **400**. `strategy` is the default for deployments that don't name one; `Rolling`, `BlueGreen` and `instances` above 1 need a domain on the built-in proxy (else **409**). Omitted fields follow aegis.toml; an empty command skips that stage; an empty `health_check_url` means "healthy while the process stays up". Replaces all previous overrides |
| `GET /projects/{project}/releases` | Release history |
| `GET /projects/{project}/domains` | Domains the built-in proxy serves the app on, each with its TLS mode and certificate state (`cert.state`: valid, expiring, expired, pending, error, disabled or off) |
| `POST /projects/{project}/domains` | Map a domain (**201**): `{"domain", "tls"?, "cert_path"?, "key_path"?}`. `tls` is `acme` (default; certificate requested in the background), `files` (absolute PEM paths on the server, checked now) or `off`. A domain belongs to one app (409 otherwise). **Users only**: API tokens get 403 |
| `DELETE /projects/{project}/domains/{domain}` | Stop serving the app on the domain (**204**; users only) |
| `GET /groups` · `GET /groups/{group}` | App groups (see [groups.md](groups.md)): name, repository, branch, `rollback_on_failure`, `members` in deploy order (with `watch_paths`, live release and commit) and `last_deployment` (per-app outcome). `{group}` is an id or name. Every role |
| `POST /groups` | Create a group (**201**): `{"name", "repository_url", "branch"?, "rollback_on_failure"?, "members"?: [{"project", "watch_paths"?}]}`. Members must be deployed from that repository and branch, and in no other group (else **400**). Admins |
| `PUT /groups/{group}` · `DELETE /groups/{group}` | Change name, repository, branch or `rollback_on_failure` (**409** if a member wouldn't match); delete (**204**; the apps stay). Admins |
| `PUT /groups/{group}/members` | Replace the members: `{"members": [{"project", "watch_paths"?}]}` in deploy order. Admins |
| `POST /groups/{group}/members` · `PUT`/`DELETE /groups/{group}/members/{project}` | Add one (`{"project", "watch_paths"?, "position"?}`), change one's `watch_paths` and/or `position` (1-based), or remove one. Admins |
| `POST /groups/{group}/deploy` | Deploy the group (**202**, `{"group_deployment_id"}`): `{"commit"?, "only_changed"?}` (default `true`: only apps whose watch paths changed since their live commit). One commit, one clone, apps in order; a failing app stops the rest. **409** while the group or one of its apps is being deployed. Deployers |
| `POST /groups/{group}/rollback` | Put every app the latest group deployment switched back on its previous release: `{"members": [{"name", "version", "error"}]}`. Deployers |
| `GET /groups/{group}/deployments` | The group's deployments, oldest first |
| `GET /projects/{project}/updates` · `POST /projects/{project}/updates/check` | New changes since the live release: `{"kind": "git"\|"folder", "branch", "head", "live_commit", "deployed", "behind", "commits": [{"sha", "author", "date", "message"}], "changed_files", "relevant", "dirty", "up_to_date", "error"}`. GET returns the latest check (running one if there is none); POST checks now (deployers). `changed_files` is filtered by the app's watch paths in a group; `dirty` counts uncommitted files of a folder app. `GET /projects` includes the summary as `updates` |
| `GET /groups/{group}/updates` · `POST /groups/{group}/updates/check` | The same for every app of a group (one fetch), plus `changed_apps` |
| `GET`/`POST /groups/{group}/webhook` · `POST /groups/{group}/webhook/test` | The group's GitHub webhook: status and recent deliveries, a new secret (shown once), a signed test ping. Admins, signed in |
| `GET /projects/{project}/jobs` · `POST /projects/{project}/jobs` | App jobs ([jobs.md](dashboard/jobs.md)): list with `next_run` and `last_run`; create (**201**): `{"name", "schedule", "command", "timeout_secs"?, "enabled"?}` (cron in UTC). Admins, signed in (tokens get 403) |
| `PUT`/`DELETE /projects/{project}/jobs/{job}` | Change `name`, `schedule`, `command`, `timeout_secs`, `enabled`; delete (**204**). Admins, signed in |
| `POST /projects/{project}/jobs/{job}/run` | Start it now (**202**, the run; **409** if it is running or the app has no live release). Deployers, API tokens too |
| `GET /projects/{project}/jobs/{job}/runs` | The last 50 runs, newest first, each with `status`, `exit_code` and `output_tail` |
| `GET /proxy` | The built-in proxy: `enabled`, ports, ACME settings, and every route with its upstream port and certificate state |
| `GET /deployments?project=` | Deployment history |
| `GET /deployments/{id}` | One deployment: status, current stage, error |
| `GET /deployments/{id}/log?lines=` | The deployment's build log (default 500 lines, max 5000) |
| `GET /deployments/{id}/events` | The deployment's events (queued, each pipeline stage, outcome), oldest first |
| `GET /processes?project=` | Processes |
| `POST /processes/{target}/{start\|stop\|restart}` | Process control |
| `GET /processes/{target}/logs?lines=` | Recent log lines |
| `GET /processes/{target}/logs/stream?lines=` | **SSE**: history, then new lines |
| `GET /events?project=&limit=` | Recent events |
| `GET /events/stream?project=` | **SSE**: each new event as it is recorded |
| `GET /openapi.json` | This API's OpenAPI 3 description |
| `POST /auth/login` | Sign in (**public**): `{"username", "password"}`; sets the session cookie |
| `POST /auth/logout` | End the session |
| `GET /auth/session` | Who is calling; the CSRF token for sessions |
| `POST /auth/password` | `{"current_password", "new_password"}`; ends all sessions (users only) |
| `GET /tokens` · `POST /tokens` · `DELETE /tokens/{id}` | List, create (`{"name", "scope": "read"\|"deploy"}`; shown once) and revoke API tokens (admins only) |
| `GET /users` · `POST /users` | List users (each with `all_apps` and, when false, the `apps` they may see); create one (**201**): `{"username", "password", "role", "all_apps"?, "apps"?}`. `all_apps: true` (the default) lets the user see every app; `apps` is a list of app names or ids they are limited to (not for admins; **400** if both are given, **404** for an unknown app). Admins only |
| `PUT /users/{name}` · `DELETE /users/{name}` | Change `role`, set a new `password` (signs them out), and/or change app access with `all_apps: true` or `apps: [...]` (an empty list: no apps); applies to open sessions at once. Delete (**204**). The last admin can't be demoted or deleted; you can't delete yourself (**409**). Admins only |
| `PUT /projects/{project}/hide` · `DELETE /projects/{project}/hide` | Hide the app behind a password: `{"password"}` (at least 12 characters; **204**; your own session stays unlocked) / stop hiding it (**204**). Admins, signed in (API tokens get 403). See [Hidden apps](#hidden-apps-and-app-access) |
| `POST /projects/{project}/unlock` · `POST /projects/{project}/lock` | Enter a hidden app's password (`{"password"}`, **204**; **403** for a wrong password *or* an app that does not exist, hidden or not allowed to you; **429** after repeated failures) so this session sees it until it ends / hide it again for this session. Signed-in users, any role |
| `POST /projects/{project}/webhook` | Create or replace the GitHub webhook secret (users only; shown once) |
| `GET /projects/{project}/deploy-key` | The app's SSH deploy key: `{"public_key", "fingerprint", "created_at", "clone_url", "add_key_url"}` (**404** without one). The private key is never returned. Admins, signed in (API tokens get 403) |
| `POST /projects/{project}/deploy-key` | Generate an Ed25519 deploy key (**201**, same body). `{"replace": true}` replaces an existing key (else **409**). Admins, signed in |
| `DELETE /projects/{project}/deploy-key` | Delete it (**204**). Admins, signed in |
| `POST /projects/{project}/deploy-key/test` | `git ls-remote` with the key: `{"ok", "url", "with_deploy_key", "branches", "branch_found", "error", "hint"}`; `error` is git's message (never key material). **409** without a key. Admins, signed in |
| `POST /apps/check-access` | Whether a GitHub repository can be read without credentials: `{"repository_url", "branch"?}` returns the same body as the test above (`ok: false`: private or missing). Only GitHub URLs (**400** otherwise). Admins, signed in |
| `GET /apps/scan` | The folders scans may look in (`allowed_dirs`: `[apps] import_dirs` plus `env_file_dirs`) and whether env files are on. Admins, signed in |
| `POST /apps/scan` | Look for apps in a server folder: `{"path"}` (inside an allowed folder, else **400**; **409** when scanning is off). Returns `{"root", "candidates", "truncated", "skipped"}`; each candidate has the proposed name, runtime, commands, port, health check, `repository_url`/`branch` (from `.git`, credentials removed), `source_subdir` (monorepo apps), `deploy_key` (`required`, `check`, `unknown` or `no`), the `.env` path and its variable **names**, `start_hint`, `port_in_use`, `registered_as` and `warnings` (`{"code", "message"}`). Read-only. Admins, signed in |
| `POST /apps/import` | Register apps without deploying them: `{"apps": [{"name", "repository_url"? or "source_dir"?, "branch"?, "source_subdir"?, "runtime"?, "settings"?}]}` (`settings` as for `PUT /projects/{p}/settings`; at most 100 apps). Returns `{"apps": [{"name", "project_id", "error", "next_steps"}]}`, one entry per app: one failing doesn't stop the others. Admins, signed in |

Outside `/api/v1`:

| Method & path | Does |
| :--- | :--- |
| `GET /healthz` | `ok` (public; for load balancers and proxies) |
| `GET /metrics` | Prometheus text metrics (apps, resources, deployments, host). **404 until an admin turns it on**; then needs an API token (any scope) or a session. See [monitoring](dashboard/monitoring.md) |
| `GET /` · `GET /assets/{file}` | The web dashboard's static files (public; they contain no data, and every API call they make needs a session) |
| `POST /hooks/github/{project-id}` | GitHub push webhook, authenticated by `X-Hub-Signature-256`. Pushes are ignored while the app is in a group |
| `POST /hooks/github/groups/{group-id}` | A group's GitHub push webhook: a push to its branch deploys the group's changed apps at the pushed commit |

Raw event injection (`aegis emit-event`) is deliberately not offered over HTTP.

### Hidden apps and app access

Two features limit which apps a signed-in user sees. Both answer **404** `not_found` for an app the caller can't see, exactly as for an app that does not exist, on every path that names it (`/projects/{project}/…`, `/processes/{target}/…`, `/deployments/{id}/…`, `?project=`), and both leave such apps out of `/projects`, `/status` counts, `/processes`, `/deployments`, `/events` (and its stream), `/metrics/stream`, `/proxy` routes and `/groups` (a group with an app you can't see is hidden whole).

- **Per-user app lists.** `PUT /users/{name}` with `apps` limits a non-admin account to those apps. Apps created later are not included until added. Admins are never limited.
- **Hidden apps.** An admin hides an app with a password; each session must unlock it (`POST /projects/{project}/unlock`) after signing in. Unlocks end with the session, and setting a new password or removing the hiding ends them for everyone.

API tokens are not limited by either: an admin issued them, and automation (CI deploys) must keep working. Hidden or not, GitHub webhooks and schedules keep deploying the app.

### Errors

Errors use HTTP status codes and a JSON body:

```json
{"error": {"code": "not_found", "message": "Unknown project 'shop'"}}
```

| Code | Status | Meaning |
| :--- | :--- | :--- |
| `invalid_argument` | 400 / 415 | Bad input, unknown field, or missing JSON content type |
| `unauthenticated` | 401 | No or invalid session/token, wrong password, or bad webhook signature |
| `forbidden` | 403 | Host/Origin checks, read-only token, token on a users-only endpoint |
| `csrf` | 403 | Session request without the right `X-CSRF-Token` |
| `rate_limited` | 429 | Too many failed sign-ins; see `Retry-After` |
| `not_found` | 404 | Unknown project, process, deployment or endpoint |
| `confirmation_required` | 409 | A risky resource limit; repeat with `"force": true` |
| `failed_precondition` | 409 | E.g. nothing to roll back to, already running, or the rollback target didn't become healthy |
| `internal` | 500 | Unexpected failure; see the daemon log |

## Examples

Deploy and follow progress:

```bash
API=http://127.0.0.1:8420/api/v1
AUTH="Authorization: Bearer aegis_…"

curl -N -H "$AUTH" "$API/events/stream?project=shop" &    # live events
curl -X POST -H "$AUTH" -H 'Content-Type: application/json' -d '{}' "$API/projects/shop/deploy"
# {"deployment_id":"…","project_id":"…","project_name":"shop"}
curl -H "$AUTH" "$API/deployments/<deployment_id>"   # Queued -> InProgress -> Success / Failed / RolledBack
```

The event stream looks like this:

```
event: BuildStageInstall
id: 01a0e6cf-…
data: {"id":"01a0e6cf-…","event_type":"BuildStageInstall","created_at":"…","payload":{"stage":"Install","status":"Success",…}}
```

Follow an app's logs:

```bash
curl -N -H "$AUTH" "$API/processes/shop/logs/stream?lines=50"
```

In a browser, `new EventSource('/api/v1/events/stream')` works from a same-origin page such as the dashboard; the session cookie authenticates it.
