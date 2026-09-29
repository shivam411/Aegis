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

The first admin password is in `<data_dir>/initial-admin-password`. Scopes, expiry, throttling and everything else are described in [security.md](security.md).

Requests that change state must also send `Content-Type: application/json` (an empty `{}` body is fine), and the `Host` header must be a loopback name or one listed in `web.allowed_hosts`.

## Endpoints

All paths are under `/api/v1`. Wherever a path takes `{project}`, you can use the project id or its name. Wherever it takes `{target}`, you can use a process id or a project (meaning the project's live process).

| Method & path | Does |
| :--- | :--- |
| `GET /status` | Version, project count, running apps, event count |
| `GET /host` | Host CPU %, memory, swap, disk (the data directory's filesystem) and load average, sampled every 5 s |
| `POST /detect` | Detect runtime and default commands: `{"source_dir"}` or `{"repository_url", "branch"?}` (shallow clone into scratch space) |
| `GET /projects` | Projects with their live release, schedule and process |
| `POST /projects` | Register a project: `{"name", "source_dir"?, "repository_url"?, "branch"?, "project_id"?}` |
| `GET /projects/{project}` | One project |
| `POST /projects/{project}/deploy` | Queue a deployment (**202**): `{"version"?, "strategy"?, "branch"?, "commit"?, "source_dir"?, "repository_url"?}` |
| `POST /projects/{project}/rollback` | Switch to the previous (or `{"version"}`) release; returns once it's live |
| `PUT /projects/{project}/schedule` | Daily auto-deploy: `{"hour", "minute"?, "branch"?}` (UTC) |
| `PUT /projects/{project}/settings` | Override aegis.toml for future deployments: `{"install_command"?, "build_command"?, "test_command"?, "start_command"?, "port"?, "health_check_url"?}`. Omitted fields follow aegis.toml; an empty command skips that stage; an empty `health_check_url` means "healthy while the process stays up". Replaces all previous overrides |
| `GET /projects/{project}/releases` | Release history |
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
| `GET /tokens` · `POST /tokens` · `DELETE /tokens/{id}` | List, create (`{"name", "scope": "read"\|"deploy"}`; shown once) and revoke API tokens (users only) |
| `POST /projects/{project}/webhook` | Create or replace the GitHub webhook secret (users only; shown once) |

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
