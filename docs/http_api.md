# HTTP API

The daemon can serve a JSON API alongside gRPC. It exposes everything the CLI does: projects, deployments, rollbacks, process control, logs, schedules and events, plus live **Server-Sent Events** streams. The web dashboard (roadmap Phase 3) will be built on it.

The full schema is in [`openapi.json`](openapi.json). The running daemon also serves it at `/api/v1/openapi.json`.

## Enabling it

```toml
# aegis.toml read by the daemon
[web]
enabled = true
host = "127.0.0.1"   # loopback only, see below
port = 8420
```

`aegis status` shows the URL once it's running.

## Security model (until Phase 2)

The API can deploy and run code, and it has **no authentication yet**. Until roadmap Phase 2 adds authentication and TLS:

- **It only listens on loopback.** The daemon refuses to start if `web.host` is anything other than `127.0.0.1`, `::1` or `localhost`. To use it from your laptop, tunnel over SSH:

  ```bash
  ssh -N -L 8420:127.0.0.1:8420 you@your-server
  curl http://localhost:8420/api/v1/status
  ```

- **Browsers can't be used against it.** Any web page you open could otherwise send requests to `127.0.0.1`, so the API rejects:
  - a `Host` header that isn't a loopback name. This blocks DNS-rebinding attacks. Any port is allowed, so tunnels to other local ports work.
  - an `Origin` that doesn't match the `Host`, and `Sec-Fetch-Site: cross-site`.
  - state-changing requests (`POST`/`PUT`) without `Content-Type: application/json`. Such requests trigger a CORS preflight, which is never granted. Send `{}` when there's nothing else to send.

- **Any local user can reach it.** Other accounts on the same server can connect to `127.0.0.1:8420`, exactly as with the gRPC port. On shared machines, wait for Phase 2 before enabling the API.

## Endpoints

All paths are under `/api/v1`. Wherever a path takes `{project}`, you can use the project id or its name. Wherever it takes `{target}`, you can use a process id or a project (meaning the project's live process).

| Method & path | Does |
| :--- | :--- |
| `GET /status` | Version, project count, running apps, event count |
| `GET /projects` | Projects with their live release, schedule and process |
| `POST /projects` | Register a project: `{"name", "source_dir"?, "repository_url"?, "branch"?, "project_id"?}` |
| `GET /projects/{project}` | One project |
| `POST /projects/{project}/deploy` | Queue a deployment (**202**): `{"version"?, "strategy"?, "branch"?, "commit"?, "source_dir"?, "repository_url"?}` |
| `POST /projects/{project}/rollback` | Switch to the previous (or `{"version"}`) release; returns once it's live |
| `PUT /projects/{project}/schedule` | Daily auto-deploy: `{"hour", "minute"?, "branch"?}` (UTC) |
| `GET /projects/{project}/releases` | Release history |
| `GET /deployments?project=` | Deployment history |
| `GET /deployments/{id}` | One deployment: status, current stage, error |
| `GET /processes?project=` | Processes |
| `POST /processes/{target}/{start\|stop\|restart}` | Process control |
| `GET /processes/{target}/logs?lines=` | Recent log lines |
| `GET /processes/{target}/logs/stream?lines=` | **SSE**: history, then new lines |
| `GET /events?project=&limit=` | Recent events |
| `GET /events/stream?project=` | **SSE**: each new event as it is recorded |
| `GET /openapi.json` | This API's OpenAPI 3 description |

Raw event injection (`aegis emit-event`) is deliberately not offered over HTTP.

### Errors

Errors use HTTP status codes and a JSON body:

```json
{"error": {"code": "not_found", "message": "Unknown project 'shop'"}}
```

| Code | Status | Meaning |
| :--- | :--- | :--- |
| `invalid_argument` | 400 / 415 | Bad input, unknown field, or missing JSON content type |
| `forbidden` | 403 | Rejected by the Host/Origin checks above |
| `not_found` | 404 | Unknown project, process, deployment or endpoint |
| `failed_precondition` | 409 | E.g. nothing to roll back to, already running, or the rollback target didn't become healthy |
| `internal` | 500 | Unexpected failure; see the daemon log |

## Examples

Deploy and follow progress:

```bash
API=http://127.0.0.1:8420/api/v1

curl -N "$API/events/stream?project=shop" &    # live events
curl -X POST -H 'Content-Type: application/json' -d '{}' "$API/projects/shop/deploy"
# {"deployment_id":"…","project_id":"…","project_name":"shop"}
curl "$API/deployments/<deployment_id>"        # Queued -> InProgress -> Success / Failed / RolledBack
```

The event stream looks like this:

```
event: BuildStageInstall
id: 01a0e6cf-…
data: {"id":"01a0e6cf-…","event_type":"BuildStageInstall","created_at":"…","payload":{"stage":"Install","status":"Success",…}}
```

Follow an app's logs:

```bash
curl -N "$API/processes/shop/logs/stream?lines=50"
```

In a browser, `new EventSource('/api/v1/events/stream')` works from a page served by the same origin (the future dashboard).
