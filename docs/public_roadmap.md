# Aegis Public Product Roadmap (Now / Next / Later)

An operator-focused summary of the Aegis roadmap. The detailed, phased plan is [`roadmap_web_control_plane.md`](roadmap_web_control_plane.md); this page follows it. For what is tested and what isn't, see [`feature_maturity.md`](feature_maturity.md) and the [scorecard](scorecard.md).

---

## ✅ SHIPPED (Phases 0–6)
- **Real deploy engine (Phase 0):** isolated builds, health checks, automatic rollback of unhealthy releases, manual rollback, release retention, supervised processes with backoff and per-app logs, and apps restored after a daemon restart.
- **HTTP/JSON API (Phase 1):** every CLI operation over HTTP, with live event and log streams and a checked-in OpenAPI spec.
- **Security and VPS hardening (Phase 2):** admin sign-in, API tokens, CSRF, login throttling, built-in TLS with certificate reload, refusal of unsafe configs, and a hardened systemd install (`install.sh --server`).
- **GitHub webhooks (Phase 2):** signed push webhooks deploy the pushed commit.
- **Web dashboard (Phase 3):** overview, deploy with live pipeline progress, logs, process control, rollback, deployments timeline, audit log and a new-app wizard; works on a phone.
- **Resource monitoring and control (Phase 4):** per-app CPU and memory charts, live CPU/memory limits from sliders without a restart (Linux cgroups), and OOM/throttling alerts.
- **Slack notifications:** failed deploys, crash loops and resource alerts.
- **Built-in reverse proxy, domains and HTTPS (Phase 5):** map a domain to an app with automatic Let's Encrypt certificates (or your own); deploys behind the proxy switch over with no dropped requests.
- **App configuration in the dashboard (Phase 5):** encrypted secrets and plain environment variables, an aegis.toml diff before saving settings, daily deploy schedules, and webhook setup with recent deliveries and a test delivery.
- **Operations polish (Phase 6):**
  - **Multiple users and roles:** viewer, deployer and admin, with every action in the audit log under the user who did it.
  - **Backup and restore** of the database, secrets key, certificates and releases, from the dashboard or on a schedule; restore on the server.
  - **Terminal UI** *(experimental)*: a `ratatui` dashboard over the local API.
  - **Self-update** *(experimental)*: "a new version is available → Upgrade now" in the dashboard ([`upgrading.md`](upgrading.md)).
  - **Docker runtime** *(experimental)*: container apps with the same health checks, proxy switch-over, rollback and CPU/memory sliders ([`docker_runtime.md`](docker_runtime.md)).
  - **Several instances per app, Rolling and blue/green deploys** *(experimental)*, behind the built-in proxy.
  - **Benchmarks and soak tests in CI** ([`benchmarks.md`](benchmarks.md)).
- **Experimental CLI tools:** `inspect --at` (state at a point in time), `replay`, `incident`, `investigate`, `upgrade`, and real `migrate pm2`/`migrate systemd` parsers.
- **Open source governance:** `CONTRIBUTING.md`, `SECURITY.md`, `SUPPORT.md`, issue and PR templates.

---

## 🟢 NOW (Hardening)
- **Signed releases**, verified by self-update and `install.sh`.
- **Resource limits verified against a real kernel in CI**, not skipped.
- **A first-run tour and an illustrated dashboard guide.**
- **Proof work:** blocking benchmark thresholds (benchmarks and 5/60-minute soaks already run in CI, see [`benchmarks.md`](benchmarks.md)), a published 24 h+ soak test, Aegis deploying itself, tested PM2/systemd migration, and tests on macOS and Windows ([`proof_phase.md`](proof_phase.md)).

---

## 🟣 NEXT (proposed)
The detailed plan for the next phase (access control, canary deploys, observability, cloud integrations, add-ons and a multi-node fleet) is in [`plan_phase7.md`](plan_phase7.md).

After that, [`plan_phase8.md`](plan_phase8.md) (proposed):
- **Home server mode:** turn an ordinary laptop, desktop or mini PC into a server with `install.sh --server --home` / `aegis host prepare`. It stays awake with the lid closed, starts at boot, cares for the battery, and is reachable over HTTPS even behind CGNAT (Cloudflare Tunnel, Tailscale, a self-hosted relay, or direct with dynamic DNS and DNS-01 certificates). It alerts on power loss, low battery, heat and disk health.
- **Virtual machines from the dashboard:** create, start, snapshot, roll back and open (browser console) full VMs on that machine with QEMU/KVM, without reinstalling it; macOS/Windows hosts, microVMs and importing an old machine as a VM come later.
- **Next-generation hosting:** every machine you own joins one pool over an encrypted mesh, so IP addresses stop mattering; workloads move off a laptop that goes on battery or overheats, and fail over between home and a small VPS. Research: [`research_home_hosting.md`](research_home_hosting.md).
- **Always-on proof first:** a published 24 h soak, a week on a real home machine, `install.sh --server` in CI, and macOS/Windows supervisor tests in CI.
- **Easier deploys:** Docker Compose apps, buildpacks, one-click templates, GitLab/Gitea/Bitbucket webhooks, `git push` deploys, remote builds.

---

## 🔵 LATER (Future Horizons)
- **Multi-node fleet:** a remote node agent reusing the same control-plane API, with a server selector in the dashboard and rolling fleet updates.
- **Plugin ecosystem:** more integrations on the event bus.
- **Optional Prometheus `/metrics` endpoint.**
- **AI operations assistant:** natural-language incident response.
