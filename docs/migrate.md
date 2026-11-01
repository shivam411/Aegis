# Migrate to Aegis

Short version: **run Aegis next to the old setup, check it works, then switch traffic.** Never stop the old app first.

Pick where you come from:

| From | Go to |
| :--- | :--- |
| PM2 | [PM2](#from-pm2) |
| systemd service | [systemd](#from-systemd) |
| Docker / Compose | [Docker](#from-docker-or-compose) |
| Another Aegis server | [Move Aegis to a new server](#move-aegis-to-a-new-server) |
| Many apps in one folder | [Scan a folder](migration_guide.md#4-many-pm2-apps-at-once-scan-the-server) |

Install Aegis first: [install.md](install.md).

## Before you start

1. List what the app needs: start command, port, env vars, secrets, domain, files git ignores (`.env`).
2. Note the old port. Aegis will use its own (`$PORT`).
3. Keep the old process running.

## From PM2

```bash
cd /path/to/app
aegis migrate pm2            # reads ecosystem.config.js/json, writes aegis.toml
aegis validate
aegis deploy
```

Check the warnings it prints. Move secrets from `[env]` into the dashboard (Settings → Secrets).

## From systemd

```bash
aegis migrate systemd --service /etc/systemd/system/my-app.service
aegis validate && aegis deploy
```

## From Docker or Compose

No converter. Add this to `aegis.toml` in the app's repo:

```toml
[run]
runtime = "docker"
```

Aegis builds your `Dockerfile` (or pulls the image) and runs the container. Map ports and env in Settings. For Compose, migrate one service at a time. Databases can stay where they are.

## Switch traffic

1. Open the app in the dashboard. Check Logs and the live URL check (Settings).
2. Add the domain (Settings → Domains). Aegis gets the certificate.
3. Point DNS at the Aegis server, or change your reverse proxy to the new port.
4. Watch for a day, then stop the old process.

To go back, point traffic at the old setup again.

## Move Aegis to a new server

1. Old server: Account → Backups → tick **Include releases** → create → download.
2. New server: install the same Aegis version, stop it.
3. Copy the backup over and restore (offline):

   ```bash
   aegis-daemon admin restore /var/lib/aegis/backups/aegis-backup-<time>.tar.gz
   ```

4. Start Aegis. Apps start again. Update DNS.

The backup includes the secrets key; keep it private. Details: [disaster_recovery.md](disaster_recovery.md).

## After migrating

- Secrets are not available at build time; use plain variables for build settings.
- Git-ignored files (`.env`) are not copied. Use the app's **Env file** setting.
- Listen on `$PORT`.

More detail: [migration_guide.md](migration_guide.md).
