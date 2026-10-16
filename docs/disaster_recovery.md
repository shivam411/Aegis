# Aegis Disaster Recovery & Fault Tolerance Guide

This document defines disaster recovery procedures, failure recovery objectives, and manual fallback steps for Aegis.

> **Status:** the recovery time objectives below are targets, not measurements. Restart recovery (apps restored after a daemon restart) is tested in CI (see [`chaos_testing.md`](chaos_testing.md)). Backup and restore are covered by unit tests (archive round trip, damaged and malicious archives, retention, an offline restore that puts back the database and key) and HTTP API tests; there is no end-to-end restore on a real server in CI yet.

---

## 1. Disaster Recovery Matrix

| Incident Scenario | Expected Automated Behavior | Manual Recovery Protocol | Recovery Time Objective (RTO) |
| :--- | :--- | :--- | :---: |
| **Daemon DB corruption or loss** | The daemon fails to start or reports database errors | Restore the latest backup (section 2) | `< 5 minutes` |
| **Server loss** | – | Install Aegis on a new server with the same `aegis.toml`, copy a downloaded backup over and restore it (section 2) | `< 30 minutes` |
| **Host Reboot Mid-Deployment** | Process supervisor detects interrupted deployment state on startup | Run `aegis rollback` or `aegis deploy` to reactivate prior active symlink | `< 30 seconds` |
| **Artifact Storage Loss** | Verification stage detects missing artifact package | Re-trigger build stage via `aegis deploy` to re-package release artifact, or restore a backup with releases | `< 120 seconds` |
| **Failed Migration** | Parser halts with validation error | Original systemd/PM2 configuration left untouched; run `aegis validate` | `< 10 seconds` |
| **Power Loss Outage** | Daemon performs WAL log recovery on boot | Process supervisor restarts active release PIDs automatically | `< 15 seconds` |

---

## 2. Backups and restore

### What a backup contains

One `aegis-backup-<timestamp>.tar.gz`:

| Entry | Why |
| :--- | :--- |
| `manifest.json` (first) | Aegis version, creation time, database schema version, and every entry with its size and SHA-256 |
| `aegis.db` | A consistent snapshot taken with SQLite `VACUUM INTO` (safe while the daemon runs): events, users, tokens, sealed secrets, metrics |
| `secrets.key` | The key app secrets are encrypted with. **Without it, restored secrets can't be read.** |
| `certs/` | ACME account and certificates, so HTTPS works at once after a restore |
| `projects/<id>/releases/`, `current.json`, `artifacts/` | Release directories on disk (the live one and those kept for rollback) and their metadata. Left out by "without releases" backups. |

Logs are not backed up. Archives are `0600` in a `0700` directory. They hold password hashes and the secrets key, so keep downloaded copies as carefully as the server itself.

### Making backups

- **Dashboard:** Account → Backups → "Back up now" (optionally without releases), then Download.
- **API** (signed-in users only; API tokens get 403): `POST /api/v1/backups` with `{"include_releases": false}` or `{}`, `GET /api/v1/backups`, `GET /api/v1/backups/{name}/download`, `DELETE /api/v1/backups/{name}`.
- **On the server** (works while the daemon runs): `sudo -u aegis AEGIS_CONFIG=/etc/aegis/aegis.toml aegis-daemon admin backup [--no-releases] [--out PATH]`. It prints the archive's path.
- **Scheduled:** `[backup] enabled = true` with `interval_hours` or `daily_at` (UTC). See [`configuration.md`](configuration.md#backups-backup).

Backups in `<data_dir>/backups` beyond `[backup] keep` (default 7) are deleted, oldest first. Every backup, failure and deletion is recorded as a `BackupCreated`, `BackupFailed` or `BackupDeleted` event. Copy archives off the server (for example by downloading them, or with `rsync` from `<data_dir>/backups`): a backup on the same disk doesn't help if the disk is lost.

### Restoring

Restores are offline. The daemon must be stopped: it holds `<data_dir>/aegis.lock` while it runs, and the restore also refuses if anything answers on the daemon's gRPC or HTTP port.

```bash
sudo systemctl stop aegis
sudo -u aegis AEGIS_CONFIG=/etc/aegis/aegis.toml \
  aegis-daemon admin restore /var/lib/aegis/backups/aegis-backup-20261016T033000000Z.tar.gz
sudo systemctl start aegis
```

(`/var/lib/aegis` is the data directory `install.sh --server` sets up. Running the restore as root also works: restored files are handed to the data directory's owner.)

The restore:

1. reads the manifest and refuses a backup whose database schema is newer than this Aegis build, or one made by a different Aegis version unless `--force` is given;
2. extracts the archive into `<data_dir>/.restore-staging-<timestamp>/`, checking every entry against the manifest's SHA-256 and refusing anything outside the layout above (absolute paths, `..`, symlinks leaving their release, unlisted files, hard links or devices). If anything is wrong, the staging directory is removed and **nothing is changed**;
3. moves the current database (and its `-wal`/`-shm` files), `secrets.key`, `certs/` and, if the backup has releases, `artifacts/` and each project's `releases/` and `current.json` into `<data_dir>/pre-restore-<timestamp>/`;
4. moves the restored files into place (`daemon.database_path` may be outside the data directory). If a step fails, the moves are undone.

On the next start the daemon rebuilds its state from the restored events, runs any newer migrations, and starts the apps that were running when the backup was taken. Delete `pre-restore-<timestamp>/` once everything works.

Notes:

- A backup without releases restores the database and keys only; release directories on disk are left as they are. An app whose recorded release is no longer on disk can't be started or rolled back to that release; deploy it again.
- Events recorded after the backup (deployments, logins, audit history) are lost. The restore itself is not recorded as an event, since the daemon is stopped.
- Logs in `projects/<id>/logs/` are not touched.

---

## 3. Emergency Manual Fallback
If the Aegis daemon is completely unreachable, managed application binaries remain intact in release storage directories (`<data_dir>/projects/<project_id>/releases/<version>/`). They can be executed directly via terminal.
