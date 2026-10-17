# Docker runtime (container apps)

> **Status: 🧪 Experimental** ([feature maturity](feature_maturity.md)). It is covered by unit tests and by one end-to-end test against a real Docker daemon on Linux CI. It has not been used in production.

By default Aegis runs an app as a plain process (`build.start_command`). With `[run] runtime = "docker"` in `aegis.toml`, the app runs in a container instead. Deployments, health checks, the graceful switch, zero-downtime cutover behind the built-in proxy, automatic and manual rollback, restarts, crash-loop detection, logs and the CPU/memory/process sliders all work the same way as for process apps.

## Quick start

```toml
# aegis.toml
[project]
name = "web"

[deploy]
port = 3400                                  # where Aegis reaches the app (127.0.0.1)
health_check_url = "http://127.0.0.1:3400/"

[run]
runtime = "docker"
dockerfile = "Dockerfile"                    # the default; relative to the app's root
container_port = 8080                        # the app listens here inside the container
```

`aegis init` writes `[run] runtime = "docker"` for a directory that only has a `Dockerfile`. A complete example is in [`examples/docker-app`](../examples/docker-app/), a BusyBox web server of about 2 MB.

Docker must be installed on the server and usable by the user the daemon runs as (for example, a member of the `docker` group). `aegis doctor` and the dashboard's Resources tab show whether it is. Process apps never need Docker.

## Configuration: `[run]`

| Key | Default | Meaning |
| :--- | :--- | :--- |
| `runtime` | `"process"` | `"process"` or `"docker"`. Any other value is rejected. |
| `dockerfile` | `"Dockerfile"` | The Dockerfile to build, relative to the release directory, which is the build context. Absolute paths and `..` are rejected. |
| `image` | none | Run a prebuilt image (for example `"nginx:1.27"` or `"ghcr.io/org/app@sha256:…"`) instead of building one. Set `image` or `dockerfile`, not both. |
| `pull` | `"missing"` | With `image`: `"missing"` pulls only if the image isn't on the server yet; `"always"` pulls on every deployment. |
| `container_port` | `deploy.port` | The port the app listens on inside the container. The container gets it as `PORT`. |
| `command` | the image's `CMD` | Replaces the image's command. It is a list of arguments, such as `["node", "server.js"]`, not a shell line. |
| `user` | the image's user | `"name"`, `"uid"` or `"uid:gid"`. |
| `read_only` | `false` | Mount the container's root filesystem read-only. Combine it with `tmpfs` for scratch space. |
| `init` | `true` | Run a small init as PID 1 (`docker run --init`) that forwards signals and reaps zombie processes. |
| `tmpfs` | none | In-memory directories, given as absolute paths in the container (for example `["/tmp"]`). Mount options are not accepted. |
| `volumes` | none | Named volumes that keep data across releases: `"name:/path"` or `"name:/path:ro"`. Each one is a Docker volume named `aegis-<project id>-<name>`, so one app can never mount another app's data. Host paths (bind mounts) are not accepted. |

Other `[build]` and `[deploy]` keys keep their meaning:

- `deploy.port` is the host port. Aegis publishes the container port on `127.0.0.1:<port>`, never on a public interface. Behind the built-in proxy, each release gets its own free port instead, as for process apps.
- `deploy.health_check_url` defaults to `http://127.0.0.1:<port>/` for container apps. Any HTTP answer below 500 counts as healthy. Don't use `tcp://` checks with containers: Docker's port forwarder accepts connections before the app listens, so a TCP check passes too early.
- `build.install_command`, `build.build_command` and `build.test_command` run on the host before the image is built, but only when they are set explicitly. Commands detected from the source (such as `npm install`) are not run for container apps. The Dockerfile does the build.
- `build.start_command` is ignored.
- `deploy.drain_timeout_secs` is the grace period for `docker stop`.
- `deploy.restart_policy` works as for process apps. Docker's own restart policy is always `no`, because Aegis does the restarting.

Unknown keys in `[run]` are errors. A typo can't silently drop a setting, and there is no way to pass arbitrary `docker run` flags. In particular, Aegis cannot be asked for `--privileged`, host networking or PID namespace, extra capabilities, devices or bind mounts. Every container also gets `--security-opt no-new-privileges`.

## How it works

**Build.** In the deployment's Build stage, Aegis first checks that Docker is reachable. If it isn't, the deployment fails with the reason, for example *"Docker is not installed"*, *"The Docker daemon is not running or not reachable"* or *"No permission to use the Docker daemon"*. It then runs `docker build` with the release directory as context, or `docker pull` (if needed) and `docker tag` for `image`. The result is tagged `aegis/<project id>:<release>`, and the build output goes to the deployment log. Release versions that contain characters a Docker tag can't hold (such as `+`) get a sanitized tag with a short hash.

**Run.** Each start, including every automatic restart, creates a fresh container named `aegis-<process id>` from the release's image. It runs `docker create --pull never`, so a release only ever runs the image built for it. The supervisor then runs `docker start --attach` for that container. That command streams the container's stdout and stderr into the process's log file, so `aegis logs`, the dashboard and the TUI work unchanged. It also exits with the container's exit code, so crash detection, restart backoff and crash-loop handling are the supervisor's own. Docker keeps no second copy of the output (`--log-driver none`).

**Environment and secrets.** `[env]`, dashboard variables, secrets and `PORT` reach the container through an `--env-file`. The file is written with mode 0600 in `<data_dir>/run/docker` (mode 0700) and deleted as soon as `docker create` returns. Values never appear on a command line, so `ps` can't show them. The `docker start` command itself runs with the daemon's environment, not the app's. Values containing a line break can't be put in an env file; such a deployment fails with a message saying so. Anyone who can run `docker inspect` can read a container's environment, but access to the Docker socket amounts to root anyway.

**PID.** For a container app, the PID shown by `aegis list`, the API, the dashboard and the TUI is the container's main process, as seen from the host (`docker inspect --format '{{.State.Pid}}'`). The API's process objects also carry `runtime` (`"docker"`), `container` and `image`.

**Stop, switch and cleanup.** Stopping runs `docker stop --time <drain>` (SIGTERM, then SIGKILL) and then removes the container, whatever the reason for the stop: an operator stop, a restart, a newer release taking over, or a daemon shutdown. A container that crashed is removed before it is recreated. When retention removes a release, its image tag is removed too (`docker image rm`; an image still used by a container stays).

**Daemon restarts.** On shutdown, containers are stopped and removed like processes. On boot, the apps that should be running are started again from their release images. Aegis does not re-adopt a running container. Containers it finds from an earlier, unclean exit are removed first; they are found by the `aegis.instance` label, which is derived from the data directory. Every container also carries `aegis.managed`, `aegis.project`, `aegis.process` and `aegis.release` labels.

**Resource limits.** The CPU, memory and process sliders apply to container apps through Docker, whether or not the host's cgroups are delegated to Aegis:

| Slider | At container start | Live change (`docker update`) | Removing the limit while running |
| :--- | :--- | :--- | :--- |
| CPU (cores) | `--cpus` | `--cpus` | set to the host's core count |
| Memory | `--memory` and `--memory-swap` (the same value, so there is no swap on top) | same | `--memory <host RAM> --memory-swap -1` |
| Processes | `--pids-limit` | `--pids-limit` | `--pids-limit -1` |

`docker update` can't fully clear a CPU or memory limit, so "no limit" on a running container means "the host's size", which has the same effect. The next start creates the container without the flag. `GET /api/v1/projects/{p}/resources` reports `backend: "docker"` with what Docker can enforce. `GET /api/v1/resources` includes `docker` (`available`, `version`, `reason`).

**Metrics.** CPU, memory, throttling and OOM kills are read from the container's own cgroup, found through `/proc/<pid>/cgroup` of its main process (cgroup v2 or v1). OOM-kill and pressure alerts work as for process apps.

## Diagnostics

- `aegis doctor` checks `[run]` and reports whether Docker is available. A container app on a server without Docker is marked as a failure.
- `GET /api/v1/resources` returns `docker: { available, version, reason }`. Docker's status is checked every minute.
- `AEGIS_DOCKER_BIN` points Aegis at a `docker` binary outside `PATH`. The CLI honours `DOCKER_HOST` and Docker contexts as usual.

## Events

Container apps use the existing events (see the [event catalog](event_catalog.md)). `ProcessStarted` gains `runtime` (`"process"` or `"docker"`), `container` and `image`. The Build stage's `BuildStageBuild` success detail is the image tag. Limit changes are `ResourceLimitsChanged`, as for process apps.

## Known gaps

- Only the Docker CLI is used, not the Engine API. Each operation starts a short-lived `docker` process.
- Containers are restarted, not re-adopted, when the daemon restarts, so container apps restart along with the daemon.
- The resource sliders can't promise more than the host has. Memory limits below current use need confirmation, as for process apps. Docker refuses CPU values above the host's core count.
- Multi-line environment values are not supported (env-file format).
- No image registry credentials management: `docker pull` uses whatever `docker login` the daemon's user has.
- The settings form in the dashboard doesn't edit `[run]`; change `aegis.toml`.
- Tested on Linux with cgroup v2 (GitHub's Ubuntu runners) only. Rootless Docker and Podman's Docker-compatible CLI are untested.
