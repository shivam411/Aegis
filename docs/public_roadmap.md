# Aegis Public Product Roadmap (Now / Next / Later)

An operator-focused summary of the Aegis roadmap. The detailed, phased plan is [`roadmap_web_control_plane.md`](roadmap_web_control_plane.md); this page follows it. For what is tested and what isn't, see [`feature_maturity.md`](feature_maturity.md) and the [scorecard](scorecard.md).

---

## ✅ SHIPPED (Phases 0–5)
- **Real deploy engine (Phase 0):** isolated builds, health checks, automatic rollback of unhealthy releases, manual rollback, release retention, supervised processes with backoff and per-app logs, and apps restored after a daemon restart.
- **HTTP/JSON API (Phase 1):** every CLI operation over HTTP, with live event and log streams and a checked-in OpenAPI spec.
- **Security and VPS hardening (Phase 2):** admin sign-in, API tokens, CSRF, login throttling, built-in TLS with certificate reload, refusal of unsafe configs, and a hardened systemd install (`install.sh --server`).
- **GitHub webhooks (Phase 2):** signed push webhooks deploy the pushed commit.
- **Web dashboard (Phase 3):** overview, deploy with live pipeline progress, logs, process control, rollback, deployments timeline, audit log and a new-app wizard; works on a phone.
- **Resource monitoring and control (Phase 4):** per-app CPU and memory charts, live CPU/memory limits from sliders without a restart (Linux cgroups), and OOM/throttling alerts.
- **Slack notifications:** failed deploys, crash loops and resource alerts.
- **Built-in reverse proxy, domains and HTTPS (Phase 5):** map a domain to an app with automatic Let's Encrypt certificates (or your own); deploys behind the proxy switch over with no dropped requests.
- **App configuration in the dashboard (Phase 5):** encrypted secrets and plain environment variables, an aegis.toml diff before saving settings, daily deploy schedules, and webhook setup with recent deliveries and a test delivery.
- **Experimental CLI tools:** `inspect --at` (state at a point in time), `replay`, `incident`, `investigate`, `upgrade`, and real `migrate pm2`/`migrate systemd` parsers.
- **Open source governance:** `CONTRIBUTING.md`, `SECURITY.md`, `SUPPORT.md`, issue and PR templates.

---

## 🟢 NOW (Phase 6 — Operations polish)
- **Multiple users and roles:** viewer, deployer, admin.
- **Backup and restore** of the database and releases from the UI, with scheduled backups.
- **Self-update** from the dashboard.
- **Terminal UI:** a real `ratatui` dashboard on the same API as the web UI.
- **Docker runtime:** container apps with the same CPU/memory sliders.
- **Rolling and blue/green deploys** *(experimental, done)*: several instances per app behind the proxy.
- **Proof work:** run benchmarks in CI with regression thresholds, a published soak test, Aegis deploying itself, tested PM2/systemd migration, and tests on macOS and Windows ([`proof_phase.md`](proof_phase.md)).

---

## 🔵 LATER (Future Horizons)
- **Multi-node fleet:** a remote node agent reusing the same control-plane API, with a server selector in the dashboard and rolling fleet updates.
- **Plugin ecosystem:** more integrations on the event bus.
- **Optional Prometheus `/metrics` endpoint.**
- **AI operations assistant:** natural-language incident response.
