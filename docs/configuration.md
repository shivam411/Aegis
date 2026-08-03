# Aegis Configuration Hierarchy & Precedence Specification

This document establishes the 5-tier configuration hierarchy for Aegis. Settings are merged deterministically, with higher-numbered tiers overriding lower tiers.

---

## 1. Precedence Hierarchy

```
[Tier 5] CLI Flags & Arguments (Highest Precedence)
    ↓
[Tier 4] Environment Variables (AEGIS_*)
    ↓
[Tier 3] Project Configuration (./aegis.toml)
    ↓
[Tier 2] Global Workspace Configuration (~/.aegis/config.toml)
    ↓
[Tier 1] Hardcoded Platform Defaults (Lowest Precedence)
```

---

## 2. Tier Details

### Tier 5 — CLI Flags & Arguments
Explicit command-line flags pass directly to CLI/TUI invocations.
- Example: `aegis deploy --project 01910a3b --branch staging`

### Tier 4 — Environment Variables (`AEGIS_*`)
All configuration variables can be set in the shell or CI environment using the `AEGIS_` prefix.
- `AEGIS_DAEMON_HOST=0.0.0.0`
- `AEGIS_DAEMON_PORT=50051`
- `AEGIS_LOG_LEVEL=debug`
- `AEGIS_DATABASE_PATH=/var/lib/aegis/aegis.db`

### Tier 3 — Local Project Configuration (`./aegis.toml`)
Project-specific settings stored in the application repository root directory.

```toml
[project]
id = "01910a3b-7f12-7890-8b01-123456789abc"
name = "my-api"
runtime = "Node.js"

[build]
install_command = "pnpm install"
build_command = "pnpm run build"
output_dir = "dist"

[deploy]
strategy = "GracefulSwitch"
health_check_url = "http://127.0.0.1:3000/health"
health_check_timeout_secs = 5
max_retained_versions = 3
```

### Tier 2 — Global Workspace Configuration (`~/.aegis/config.toml`)
Host-wide defaults applied across all projects on a single server or workstation.

```toml
[daemon]
host = "127.0.0.1"
port = 50051
database_path = "~/.aegis/aegis.db"
log_level = "info"
max_retained_versions = 2

[plugins]
enabled = ["slack", "github"]
```

### Tier 1 — Platform Defaults

| Setting | Default Value |
| :--- | :--- |
| `daemon.host` | `"127.0.0.1"` |
| `daemon.port` | `50051` |
| `daemon.database_path` | `"aegis.db"` |
| `daemon.log_level` | `"info"` |
| `deploy.max_retained_versions` | `2` |
| `deploy.strategy` | `"GracefulSwitch"` |
