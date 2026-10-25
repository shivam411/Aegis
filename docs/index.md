# Aegis

> Single-binary, event-sourced deployment daemon and process manager for a Linux server.

Aegis deploys and supervises apps on a single VPS. Written in Rust, it provides health-checked deployments with automatic rollback, version retention, scheduled deploys, runtime detection, a web dashboard with live CPU/memory limits, and an authenticated HTTP API — all from one daemon with embedded SQLite.

Aegis is pre-1.0 and early. What is tested in CI, and what isn't measured yet, is listed in the [scorecard](scorecard.md) and [feature maturity](feature_maturity.md) pages.

## Key Features

- **Safe Deployments** — Build in isolation, health-check the new release, and roll back automatically if it fails. (Cutover has a short gap until the reverse proxy on the [roadmap](roadmap_web_control_plane.md) lands.)
- **Version Retention** — Keeps the last N releases on disk (default: 2). Older versions are pruned after each deployment.
- **Daily Scheduled Deployments** — Set target hours for automated builds (e.g. `aegis schedule --hour 2` for 02:00 daily).
- **Runtime Detection** — Detects Node.js, Rust, Go, Python, Bun and Deno projects from their files. (CI deploys Node.js apps; other stacks are detected but not yet deploy-tested.)
- **Event-Sourced Audit Trail** — Every change is an immutable event stored in SQLite and replayed on startup.
- **Rollbacks** — Roll back to any retained release without rebuilding.
- **Web Dashboard & HTTP API** — Deploy, roll back, stream logs and audit from the browser; sign-in, API tokens and TLS built in.
- **Live Resource Limits** — Per-app CPU and memory limits with Linux cgroups, changed without a restart.
- **Users, Roles & App Access** — Viewer, deployer and admin accounts; limit an account to some apps (or all), and hide an app behind a password.
- **Notifications & Webhooks** — Slack alerts and signed GitHub push webhooks.

## Quick Links

- [Installation Guide](install.md) · [Upgrading](upgrading.md) · [Migrating from PM2/systemd](migration_guide.md)
- [`agent.md`](../agent.md) — copy-paste prompts that let an AI coding agent install, migrate and manage Aegis
- [Quickstart Tutorial](quickstart.md)
- [Dashboard Guide](dashboard/README.md) — every page of the web dashboard, with screenshots
- [Architecture Deep Dive](architecture.md)
- [Feature Maturity](feature_maturity.md) · [Scorecard](scorecard.md) · [Benchmarks & Soak Tests](benchmarks.md) · [Roadmap](public_roadmap.md)
- [GitHub Repository](https://github.com/shivam411/Aegis)

## Quick Install

```bash
curl -fsSL https://raw.githubusercontent.com/shivam411/Aegis/main/install.sh | bash
```

## License

[MIT License](https://github.com/shivam411/Aegis/blob/main/LICENSE)
