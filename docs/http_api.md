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
| `GET /host/metrics?range=&points=` | The same for the whole host |
| `GET /metrics/stream?project=&host=` | **SSE**: a `sample` message per app (and the host) every sample interval |
| `GET /host` | Host CPU %, memory, swap, disk (the data directory's filesystem) and load average, sampled every 5 s |
| `GET /system/update` | Whether a newer Aegis release is available (cached result of the periodic check), how it would be installed here (`mode`), the release signing keys this build trusts (`release_keys`), and update progress (`phase`) |
| `POST /system/update/check` | Admins, signed in (not API tokens): ask GitHub now. Refused (409) with `[update] check = false` |
| `POST /system/update/apply` | Admins, signed in: back up, download, verify (release signature, then SHA-256) and install the latest release, then restart (202; follow `phase`). `{"version"?}` must match the latest known release. 409 with the command to run when the daemon can't replace its own binary, or when this build has no release signing key. See [upgrading.md](upgrading.md) |
| `POST /detect` | Detect runtime and default commands: `{"source_dir"}` or `{"repository_url", "branch"?}` (shallow clone into scratch space), plus `"source_subdir"?` (look inside that folder) and `"project"?` (an existing app: clone with its deploy key, default to its folder). `start_hint` is set when the package.json start script hard-codes its port: `{"port", "script", "suggested_start"}` (e.g. `npx next start -p $PORT`) |
| `GET /projects` | Projects with their live release, schedule and process, plus `instances` (every instance of the live release with its process, CPU and memory; one entry for single-instance apps) and `standby` (a BlueGreen previous release kept running, or `null`) |
| `POST /projects` | Register a project: `{"name", "source_dir"?, "repository_url"?, "branch"?, "project_id"?, "source_subdir"?}`. `source_subdir` (e.g. `apps/web`, relative, no `..`) builds and runs only that folder of the source |
| `GET /projects/{project}` | One project |
| `POST /projects/{project}/deploy` | Queue a deployment (**202**): `{"version"?, "strategy"?, "branch"?, "commit"?, "source_dir"?, "repository_url"?}`. `strategy` is `GracefulSwitch`, `Immediate`, `Rolling` or `BlueGreen`; the last two need a domain on the built-in proxy (else **400**). |
| `POST /projects/{project}/rollback` | Switch to the previous (or `{"version"}`) release; returns once it's live |
| `PUT /projects/{project}/schedule` | Daily auto-deploy: `{"hour", "minute"?, "branch"?}` (UTC) |
| `PUT /projects/{project}/settings` | Override aegis.toml for future deployments: `{"install_command"?, "build_command"?, "test_command"?, "start_command"?, "port"?, "health_check_url"?, "strategy"?, "instances"?}`. `strategy` is the default for deployments that don't name one; `Rolling`, `BlueGreen` and `instances` above 1 need a domain on the built-in proxy (else **409**). Omitted fields follow aegis.toml; an empty command skips that stage; an empty `health_check_url` means "healthy while the process stays up". Replaces all previous overrides |
| `GET /projects/{project}/releases` | Release history |
| `GET /projects/{project}/domains` | Domains the built-in proxy serves the app on, each with its TLS mode and certificate state (`cert.state`: valid, expiring, expired, pending, error, disabled or off) |
| `POST /projects/{project}/domains` | Map a domain (**201**): `{"domain", "tls"?, "cert_path"?, "key_path"?}`. `tls` is `acme` (default; certificate requested in the background), `files` (absolute PEM paths on the server, checked now) or `off`. A domain belongs to one app (409 otherwise). **Users only**: API tokens get 403 |
| `DELETE /projects/{project}/domains/{domain}` | Stop serving the app on the domain (**204**; users only) |
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
| `GET /users` · `POST /users` | List users; create one (**201**): `{"username", "password", "role"}` (admins only) |
| `PUT /users/{name}` · `DELETE /users/{name}` | Change `role` and/or set a new `password` (signs them out); delete (**204**). The last admin can't be demoted or deleted; you can't delete yourself (**409**). Admins only |
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
| `GET /` · `GET /assets/{file}` | The web dashboard's static files (public; they contain no data, and every API call they make needs a session) |
| `POST /hooks/github/{project-id}` | GitHub push webhook, authenticated by `X-Hub-Signature-256` |

Raw event injection (`aegis emit-event`) is deliberately not offered over HTTP.

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
