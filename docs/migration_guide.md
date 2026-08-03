# Aegis PM2 & Systemd Migration Guide

This guide describes how to migrate legacy process management configurations from **PM2** and **systemd** to Aegis using automated CLI tools.

---

## 1. Migrating from PM2

Aegis provides automated PM2 ecosystem configuration parsing:

```bash
# Automatically parse ecosystem.config.js or ecosystem.config.json and generate aegis.toml
aegis migrate pm2
```

### Manual PM2 to Aegis Field Mapping

| PM2 Field | Aegis TOML Equivalent | Notes |
| :--- | :--- | :--- |
| `name` | `project.name` | Unique project identifier |
| `script` | `build.start_command` | Execution command |
| `cwd` | `project.directory` | Project working directory |
| `env` | `[environment]` | Environment variables |
| `autorestart: true` | `deploy.restart_policy = "Always"` | Supervision restart policy |
| `max_restarts` | `deploy.max_restarts = 5` | Maximum restart threshold |

---

## 2. Migrating from Systemd

Aegis provides automated systemd service unit parsing:

```bash
# Convert a systemd service file (e.g. my-app.service) to aegis.toml
aegis migrate systemd --service /etc/systemd/system/my-app.service
```

### Manual Systemd to Aegis Field Mapping

| Systemd Unit Directive | Aegis TOML Equivalent | Notes |
| :--- | :--- | :--- |
| `ExecStart=` | `build.start_command` | Main process startup command |
| `WorkingDirectory=` | `project.directory` | Working directory path |
| `Environment=` | `[environment]` | Environment variable assignments |
| `Restart=always` | `deploy.restart_policy = "Always"` | Auto-restart policy |
| `User=` | `project.user` | Process execution user |
