# Monitoring (Prometheus and Grafana)

**Account → Monitoring (Prometheus & Grafana)**, for admins. It is **off until you turn it on**, and Aegis never sends data anywhere: Prometheus fetches `/metrics` from Aegis when it wants it.

## Turn it on

1. Click **Turn metrics on**. The card then shows the scrape URL and a ready `prometheus.yml`.
2. Under **API tokens** create a token (**read** scope is enough) and put it in `prometheus.yml` as the bearer token. A signed-in session can open `/metrics` too, but sees only the apps that user may see; tokens see every app.
3. If Prometheus reaches the daemon under another host name, add that name to `[web] allowed_hosts` in the daemon configuration.
4. **Turn metrics off** any time: `/metrics` then answers "not found".

The same switch over the API: `GET`/`PUT /api/v1/system/prometheus` (`{"enabled": true}`; admins, signed in).

## Grafana

1. Add your Prometheus as a data source in Grafana.
2. **Download Grafana dashboard** (or `GET /api/v1/system/grafana-dashboard`), then in Grafana: **Dashboards → New → Import**, upload the file and choose the Prometheus data source.
3. The **App** variable at the top limits the charts to some apps. Panels: apps and apps running, host CPU, memory and disk, per-app CPU against its limit, memory against its limit, up/down, restarts, CPU throttling, threads and open files, deployments by status, host load.

## Metrics

| Metric | Labels | Meaning |
| :--- | :--- | :--- |
| `aegis_build_info` | `version` | Always 1. |
| `aegis_apps` | | Registered apps. |
| `aegis_events_total` | | Events in the audit log. |
| `aegis_app_up` | `app` | 1 while the app's process runs. |
| `aegis_app_restarts` | `app` | Restarts of its current process. |
| `aegis_app_instances` | `app` | Instances serving the live release. |
| `aegis_app_cpu_cores`, `aegis_app_cpu_limit_cores` | `app` | CPU in use and its limit. |
| `aegis_app_memory_bytes`, `aegis_app_memory_limit_bytes` | `app` | Memory in use and its limit. |
| `aegis_app_cpu_throttled_ratio` | `app` | Share of CPU periods held back by the limit (0-1). |
| `aegis_app_threads`, `aegis_app_open_fds` | `app` | Threads and open files. |
| `aegis_deployments` | `app`, `status` | Deployments by outcome. |
| `aegis_host_cpu_percent`, `aegis_host_load` (`window`), `aegis_host_memory_*_bytes`, `aegis_host_disk_*_bytes`, `aegis_host_uptime_seconds` | | The server. |

Request counts and latency per app are not exported yet (the built-in proxy does not count them).

Sample files: [`examples/grafana/prometheus.yml`](../../examples/grafana/prometheus.yml).
