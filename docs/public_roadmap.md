# Aegis Public Product Roadmap (Now / Next / Later)

This document provides a clean, operator-focused overview of the Aegis product roadmap using the **Now / Next / Later** framework.

---

## 🟢 NOW (Active Focus — Milestone M2 & Proof Phase)
- **Zero-Boilerplate 5-Minute Onboarding**: One-command installer and `DetectorPipeline` auto-generation (`aegis.toml`).
- **Signature Feature — Deployment Replay**: Step-by-step operational replay (`aegis replay <release_id>`).
- **Signature Feature — Time Travel Inspection**: Historical state inspection (`aegis inspect --at <timestamp>`).
- **Operational Incident Response**: Root-cause diagnostic advisor (`aegis incident`).
- **PM2 & Systemd Migration Engine**: Automated migration tooling (`aegis migrate pm2`, `aegis migrate systemd`).
- **Open Source Governance Suite**: Full community setup (`CONTRIBUTING.md`, `SECURITY.md`, `SUPPORT.md`, issue/PR templates).

---

## 🟡 NEXT (Upcoming — Production Engine & TUI)
- **Atomic Deployment Engine**: Isolated build execution & zero-downtime symlink directory handover.
- **Interactive Terminal Dashboard (`aegis-tui`)**: Real-time Ratatui status dashboard and process controls.
- **Automated Webhooks & Git Triggers**: GitHub webhook receiver for instant automated deployments on push.
- **24h/72h/168h Soak Testing Certification**: Production stability validation under continuous load.

---

## 🔵 LATER (Future Horizons)
- **Multi-Node Fleet Supervision**: Multi-server node agent coordination and rolling fleet updates.
- **Plugin Capabilities & Ingress Engine**: Extensible plugin ecosystem and reverse proxy route management.
- **AI Operational Copilot**: Natural language incident response and auto-remediation.
