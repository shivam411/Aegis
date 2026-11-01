# Monitoring (Prometheus and Grafana)

**Account → Monitoring (Prometheus & Grafana)**, admins only. **Off until you turn it on.** Aegis sends nothing; Prometheus fetches `/metrics`.

## Turn it on

1. Click **Turn metrics on**. The card shows the scrape URL and a ready `prometheus.yml`.
2. Under **API tokens** create a token (**read** scope is enough) and use it as the bearer token in `prometheus.yml`. A signed-in session can open `/metrics` but sees only its apps; tokens see all.
3. If Prometheus uses another host name, add it to `[web] allowed_hosts` in the daemon configuration.
4. **Turn metrics off** any time: `/metrics` then answers "not found".

API: `GET`/`PUT /api/v1/system/prometheus` (`{"enabled": true}`; signed-in admins).

## Grafana

1. Add Prometheus as a Grafana data source.
2. **Download Grafana dashboard** (or `GET /api/v1/system/grafana-dashboard`), then **Dashboards → New → Import** and pick the data source.
3. The **App** variable filters the charts. Panels: apps, host CPU/memory/disk/load, per-app CPU and memory against limits, up/down, restarts, throttling, threads, open files, deployments by status.

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

Per-app request counts and latency are not exported.

Sample files: [`examples/grafana/prometheus.yml`](../../examples/grafana/prometheus.yml).
