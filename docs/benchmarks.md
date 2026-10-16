# Benchmarks and soak tests

What Aegis measures about its own performance and stability, how to run it,
and how to read the results. Everything here runs in
[`.github/workflows/perf.yml`](../.github/workflows/perf.yml) on
GitHub-hosted Ubuntu runners (2–4 vCPUs, shared). Those machines are noisy,
so treat the numbers as rough and look at trends, not single runs.

> **Status:** benchmarks run on every PR and push to `main`, and are compared
> with the latest `main` run, but a regression only **warns**. A 5-minute soak
> runs on every push to `main`, a 60-minute soak nightly. The numbers in the
> tables below are a snapshot from one CI run each, not targets or
> guarantees. They say nothing about production hosts. See
> [What is not measured](#what-is-not-measured).

---

## 1. Benchmarks (`benchmarks/`)

| Benchmark | What it measures | Kind |
| :--- | :--- | :--- |
| `event_bus/publish` | Publishing one event on the in-memory bus with one subscriber | Criterion |
| `projection/apply_event` | Folding one event into the in-memory projection | Criterion |
| `event_store/append` | Appending one event to a file-backed SQLite store (opened like the daemon: WAL, five connections, a sync listener updating a projection) | Criterion |
| `event_store/replay_12020` | Reading back a realistic 12,020-event history (20 apps × 50 deployments) in order | Criterion |
| `projection/rebuild_12020` | Rebuilding the projection from those events in memory | Criterion |
| `projection/load_and_rebuild_12020` | Both, as the daemon does on every start | Criterion |
| `proxy/direct/sequential`, `proxy/ingress/sequential` | One small GET over a kept-alive HTTP/1.1 connection, straight to an app and through the built-in proxy (`runtime/ingress`) | Criterion |
| `proxy/direct/concurrent_64`, `proxy/ingress/concurrent_64` | 64 GETs over 8 kept-alive connections at once, both ways | Criterion |
| `deploy_saga/first_deploy`, `deploy_saga/direct_port` | Whole deployments of a tiny app (`python3 -m http.server`) through the real deploy saga: copy, build, package, start, health check, stop the previous release | Timing harness |
| `deploy_saga/behind_proxy` | The same with a domain on the proxy: side-by-side start, health check, switch, drain, stop | Timing harness |
| `deploy_saga/rollback_behind_proxy` | Rollbacks behind the proxy | Timing harness |

The proxy overhead is the difference between the `ingress` and `direct` rows.
Deploy times are dominated by waiting: the health check polls every 500 ms
(the daemon's default), and the previous release gets a SIGTERM and up to its
drain timeout to exit.

The deploy saga harness is not a Criterion benchmark (one deployment takes
seconds). It runs each scenario `AEGIS_BENCH_DEPLOYS` times (default 8),
prints min/median/p95/max and writes `target/bench-results/deploy_saga.json`.
It runs on Unix only and does nothing under `cargo test`.

### Running locally

```bash
cargo bench -p aegis-benchmarks                              # everything
cargo bench -p aegis-benchmarks --bench event_throughput     # one file
cargo bench -p aegis-benchmarks --bench proxy_overhead -- ingress   # a filter
AEGIS_BENCH_DEPLOYS=20 cargo bench -p aegis-benchmarks --bench deploy_saga

# The report CI produces, compared with an earlier bench-results.json:
python3 scripts/bench_report.py --criterion-dir target/criterion \
  --extra target/bench-results/deploy_saga.json \
  --baseline old/bench-results.json --out-json bench-results.json --out-md bench-report.md
```

Criterion keeps its own history in `target/criterion`, so a second local run
also prints the change against the first.

### How CI compares

1. The `Benchmarks` job runs on every PR to `main` and every push to `main`
   (about 10 minutes with a warm cache, most of it compiling).
2. Criterion runs with `--warm-up-time 1 --measurement-time 4` (8 s for the
   replay benchmarks), then the deploy saga harness.
3. [`scripts/bench_report.py`](../scripts/bench_report.py) collects the
   medians into `bench-results.json` and compares each one with the latest
   `main` run's file, restored from the Actions cache
   (`bench-baseline-main-*`, saved by every push to `main`).
4. A benchmark more than **30%** slower than `main` is flagged
   `REGRESSION` in the job summary and as a warning annotation on the run.
   The job still passes.
5. The summary table, `bench-results.json`, `bench-report.md` and the raw
   Criterion output are uploaded as the `bench-results-<run id>` artifact
   (kept 30 days).

The threshold is generous on purpose: on shared runners the same commit can
vary by 10–20% between runs, more for the proxy and deploy timings. A real
regression shows up again on the next run; a single flag usually doesn't.

**Making it blocking.** Set the repository variable
`BENCH_FAIL_ON_REGRESSION` to `true` (Settings → Secrets and variables →
Actions → Variables) and the job fails on any flagged regression; then make
the `Benchmarks` check required in the branch protection rules. Change the
threshold with `BENCH_REGRESSION_THRESHOLD` (e.g. `0.5` for 50%). Before
doing either, a dedicated runner would make the numbers far more stable.

If the cache entry has expired (GitHub evicts entries unused for 7 days) the
summary says there is no baseline and shows absolute numbers; the next push
to `main` creates a new one.

---

## 2. Soak test (`scripts/soak.sh`)

The soak test runs the real release build of `aegis-daemon` on Linux for a
set time and checks that it stays correct and doesn't leak:

1. Starts the daemon with the web API and the built-in proxy on local ports.
2. Registers a copy of `examples/node-app` (its response carries a version
   tag), maps the domain `soak.test` to it on the proxy (TLS off) and
   deploys it.
3. Sends steady HTTP load through the proxy: by default 100 requests/s over
   8 kept-alive connections, from [`scripts/soak_load.py`](../scripts/soak_load.py)
   (Python standard library, nothing installed). A request fails on a
   connection error, a 5 s timeout or any status but 200.
4. Every 20 s runs the next operation of a cycle over the HTTP API:
   deploy, resource-limit change, deploy, rollback, app restart, deploy,
   limit change. Limit changes alternate between two CPU/memory/pid limits
   and need a cgroup backend (CI runs the soak as root; without one they
   are recorded as skipped).
5. Restarts the daemon once at 75% of short runs, every 20 minutes in long
   ones.
6. Every 5 s samples the daemon's RSS, open fds, threads and zombie
   children, the database and WAL size, the app log directory, the whole
   data directory and the daemon log, plus the live app's pid, RSS and
   restart count.

At the end [`scripts/soak_report.py`](../scripts/soak_report.py) writes
`report.md` and `summary.json`.

### When it fails

| Check | Default threshold | Variable |
| :--- | :--- | :--- |
| Failed requests during deploys, rollbacks, limit changes and steady state | more than max(5, 0.05% of those requests) | `SOAK_MAX_FAILED_REQUESTS`, `SOAK_MAX_FAILED_RATIO` |
| Any operation fails (deploy not `Success`, rollback or restart error, app not serving again within 60 s of an app or daemon restart) | any | — |
| The daemon exits on its own, or isn't running at the end | any | — |
| App crashes (`ProcessCrashed` events) | more than 0 | `SOAK_MAX_APP_CRASHES` |
| RSS leak: over one daemon lifetime of ≥ 2.5 min, after a 20% warm-up, the least-squares trend grows by more than 16 MiB **and** faster than 20 MiB/h **and** the last third's median is above the first third's | — | `SOAK_RSS_MAX_GROWTH_MB`, `SOAK_RSS_MAX_SLOPE_MB_PER_H`, `SOAK_MIN_SEGMENT_SECS` |
| fd leak: the same trend for open fds grows by more than 20 | — | `SOAK_FD_MAX_GROWTH` |
| `ERROR` lines in the daemon log | any | `SOAK_ALLOWED_ERRORS` (a regex of lines to ignore) |

Failed requests during an **app restart** or a **daemon restart** (plus 2 s,
`SOAK_RECOVERY_GRACE_MS`) are counted and reported but don't fail the run:
neither is zero-downtime today. A restart stops the process before starting
it again, and the proxy runs inside the daemon, so a daemon restart takes the
app's traffic down until the daemon is back and has restarted the app.

A 5-minute run can only catch fast leaks; the nightly 60-minute run has
three 20-minute daemon lifetimes to judge.

### Running it

```bash
cargo build --release -p aegis-daemon
DURATION_MIN=5 OUT_DIR=soak-report scripts/soak.sh            # as yourself: no limits
sudo -E env "PATH=$PATH" DURATION_MIN=60 scripts/soak.sh      # with cgroup limits
```

It needs Linux, `curl`, `python3` and `node`, and free local ports 18080,
18443, 18520 and 50251 (`PROXY_HTTP_PORT`, `PROXY_HTTPS_PORT`, `WEB_PORT`,
`GRPC_PORT`). Other knobs: `LOAD_RATE`, `LOAD_WORKERS`, `OP_INTERVAL_SECS`,
`SAMPLE_SECS`, `DAEMON_RESTART_EVERY_SECS`.

### In CI

| Trigger | Duration |
| :--- | :--- |
| Push to `main` | 5 min |
| PR labelled `soak` (and later pushes to it) | 5 min |
| Nightly (`schedule`, 03:23 UTC) | 60 min |
| `workflow_dispatch` | `soak_minutes` input (default 60, at most 300) |

For example `gh workflow run perf.yml --ref main -f soak_minutes=180`.
Runs of the same kind on the same branch don't pile up: a newer PR push
cancels the older run.

### Reading the report

The job summary shows `report.md`; the `soak-report-<minutes>min-<run id>`
artifact has it with the raw data:

- `samples.csv`: one row per sample (columns as listed above;
  `daemon_alive` is 1, 0 = dead, 2 = stopped on purpose).
- `load.csv`: one row per second: successful and failed requests, p50, p99
  and max latency.
- `failures.csv`: every failed request with its time and error.
- `ops.csv`: every operation with start and end time, result and detail.
- `events.json`: the app's events at the end; `daemon.log`: the daemon's
  output.

In `report.md`:

- **Requests:** totals, latency, and seconds in which nothing succeeded
  (downtime). Failures are split by what was going on at the time, so a
  failure during a deploy is distinguishable from one during a restart.
- **Operations:** how long each kind took; `deploy` and `rollback` are
  measured from the API call until the new release is live behind the proxy.
- **Daemon resources per process lifetime:** RSS, fds and threads at the
  start and end of each daemon lifetime, the fitted growth and the verdict.
  Some growth in the first minutes is normal (allocator pools, caches,
  connection pools); what matters is a steady climb late in a long run.
- **Disk:** database, app logs and data directory, with growth per hour. The
  database grows with every event and keeps per-minute metric rollups; the
  data directory is bounded by release retention.
- **Daemon log:** `ERROR` lines (shown) and the number of `WARN` lines.

---

## 3. Measured numbers

Snapshot from CI on GitHub-hosted `ubuntu-latest`; see the linked runs for
the full reports.

_To be filled in from the first green runs._

---

## What is not measured

- Production hardware or real traffic: everything above runs on a shared
  CI VM with a toy app.
- The SLO targets in [`slo_targets.md`](slo_targets.md) are mostly not
  measured yet: CLI latency, `aegis doctor`/`validate`, runtime detection,
  crash-restart time and event propagation latency have no benchmark. Daemon
  start (launch until `/healthz` answers) is timed by the soak but not
  enforced.
- Soaks longer than 5 hours (GitHub's job limit is 6); the 24 h / 72 h /
  168 h runs in [`soak_testing.md`](soak_testing.md) need a dedicated host.
- HTTPS through the proxy (the soak and the proxy benchmark use plain HTTP),
  WebSockets, large bodies, many apps or many domains.
- macOS and Windows.
