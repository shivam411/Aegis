# Aegis

> Zero-dependency, event-sourced deployment platform & process manager for Linux servers.

Aegis is a modern, high-performance alternative to PM2, Heroku, and Vercel for single-server VPS environments. Written in Rust, it provides health-checked deployments with automatic rollback, automated version retention, scheduled auto-deployments, polyglot runtime detection, and a high-quality terminal UI — all powered by a single self-contained daemon with embedded SQLite.

## Key Features

- **Safe Deployments** — Build in isolation, health-check the new release, and roll back automatically if it fails. (A reverse proxy for fully zero-downtime cutover is on the [roadmap](roadmap_web_control_plane.md).)
- **Automated Version Retention** — Keeps the last N releases on disk (default: 2). Older versions are auto-pruned after each deployment.
- **Daily Scheduled Deployments** — Set target hours for automated builds (e.g. `aegis schedule --hour 2` for 02:00 AM daily).
- **Polyglot Runtime Detection** — Auto-detects Node.js, Rust, Go, Python, Bun, Deno from project files.
- **Event-Sourced Audit Trail** — Every mutation is an immutable domain event stored in SQLite. Full replay capability.
- **Instant 1-Step Rollbacks** — Roll back to any previous healthy release without rebuilding.
- **Interactive Terminal UI** — Real-time dashboard powered by `ratatui` + `crossterm`.
- **Plugin System** — Extensible event bus with Slack, GitHub, and webhook integrations.

## Quick Links

- [Installation Guide](install.md)
- [Quickstart Tutorial](quickstart.md)
- [Architecture Deep Dive](architecture.md)
- [GitHub Repository](https://github.com/shivam411/Aegis)

## Quick Install

```bash
curl -fsSL https://raw.githubusercontent.com/shivam411/Aegis/main/install.sh | bash
```

## License

[MIT License](https://github.com/shivam411/Aegis/blob/main/LICENSE)
