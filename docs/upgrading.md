# Upgrading Aegis

> **Maturity: 🧪 Experimental.** Update checks, the dashboard banner and the
> download/verify/swap steps are covered by automated tests against a fake
> GitHub API. The root helper and the restart under systemd have not yet been
> exercised end to end in CI.

Aegis checks GitHub for new releases, shows a banner in the dashboard
("Aegis v0.8.0 is available (you run v0.7.0)"), and lets an admin upgrade from
there. You can always upgrade from the shell instead.

## Update checks

Every 12 hours (first about a minute after start) the daemon asks
`https://api.github.com/repos/shivam411/Aegis/releases/latest` for the latest
**stable** release (GitHub excludes drafts and pre-releases from that call)
and keeps the answer in `<data_dir>/update/state.json`. Failures are logged and
shown to admins in the banner; they never affect the daemon. The first time a
newer release is seen, an `UpdateAvailable` event is recorded.

```toml
[update]
check = true               # false: no update requests at all, and no dashboard upgrades
interval_hours = 12        # 1-744
repo = "shivam411/Aegis"   # owner/name, for forks
```

With `check = false` the daemon makes no network requests for updates, and
"Check now" and "Upgrade now" are refused. `aegis upgrade` and
`aegis-daemon self-update` still work when you run them.

Everyone signed in sees the banner and `GET /api/v1/system/update`. Only
signed-in **admins** can check now (`POST /api/v1/system/update/check`) or
upgrade (`POST /api/v1/system/update/apply`); API tokens can't do either.

## What "Upgrade now" does

1. **Download and verify.** The archive for this machine
   (`aegis-<os>-<arch>.tar.gz`) and the release's `SHA256SUMS` are downloaded
   over HTTPS; the archive must match its SHA-256 or nothing happens. The new
   `aegis-daemon` (and `aegis-cli`/`aegis-tui` if they are installed next to it)
   are written next to the old ones, and the new daemon must answer
   `--version` with the expected version.
2. **Back up.** A backup of the database, secrets key and certificates (no
   release directories) is written to `<data_dir>/backups` like any other
   backup (`trigger: "update"`). If it fails, the update stops.
3. **Swap.** Each binary is replaced with a single atomic rename; the old one
   stays next to it as `aegis-daemon.previous` (`aegis-cli.previous`, ...).
4. **Restart.** The daemon shuts down gracefully and the new binary starts:
   under systemd it exits with status 75 and systemd starts it again
   (`RestartForceExitStatus=75` in install.sh's unit; `Restart=on-failure`
   also covers it); otherwise it re-executes itself.

**What happens to your apps:** on shutdown Aegis stops every app it
supervises (SIGTERM, then up to 10 s to drain) and the new daemon starts them
again from their live releases right after boot. Expect a short outage per
app, typically a few seconds; requests during that window fail (the built-in
proxy restarts too). Deployments still running at that moment are marked
failed ("Interrupted") and must be started again. Sessions survive; the
dashboard shows "Aegis is restarting…", reconnects, reloads, and shows
"Updated to vX" (an `UpdateApplied` event is recorded by the new version).

## How it is installed depends on who owns the binary

The banner says which of these applies (`mode` in the API):

| Mode | When | What happens |
| :--- | :--- | :--- |
| `in_place` | The daemon can write the directory its binary is in (per-user install in `~/.local/bin`, a hand-run daemon) | The daemon does all four steps itself. |
| `helper` | `install.sh --server` (root-owned `/usr/local/bin`, daemon running as `aegis` under `ProtectSystem=strict`) | The daemon backs up, then writes `/var/lib/aegis/update/request.json` containing only the version. The root-owned `aegis-update.path` unit starts `aegis-update.service`, which runs `aegis-daemon self-update --from-request`: it deletes the request, downloads and verifies that release **itself**, refuses anything that isn't newer than the installed version, swaps the binaries and restarts `aegis.service`. On failure it writes `result.json`, which the daemon reports as `UpdateFailed`. |
| `manual` | Neither (e.g. installed with `--no-self-update`, or binaries owned by another user) | "Upgrade now" is not offered; admins see the exact command to run, e.g. `sudo env AEGIS_CONFIG=/etc/aegis/aegis.toml /usr/local/bin/aegis-daemon self-update --restart-unit aegis.service`. Back up first (Account → Backups). |
| `unsupported` | Windows (releases are zip files) or no release for this CPU | Download the release by hand. |

Installs from before this feature don't have the helper; re-run
`install.sh --server` (it keeps your configuration) to add it, or use the
manual command.

## From the shell

```sh
# Server install (root-owned binaries):
sudo env AEGIS_CONFIG=/etc/aegis/aegis.toml aegis-daemon self-update --check
sudo env AEGIS_CONFIG=/etc/aegis/aegis.toml aegis-daemon self-update --restart-unit aegis.service

# A specific version; --force also allows reinstalling, downgrading,
# pre-releases and major-version jumps:
sudo aegis-daemon self-update --version v0.8.0 --force

# Per-user install (re-runs install.sh):
aegis upgrade
```

`self-update` uses the same download, verification and swap code as the
dashboard; `aegis upgrade` re-runs install.sh, which now also checks the
archive against `SHA256SUMS`.

## Never downgrades, and major versions need a decision

- The dashboard and the helper only install a release **newer** than the
  running one. Downgrades and reinstalls need `self-update --force` on the
  server.
- A new **major** version (1.x → 2.x) is not installed from the dashboard;
  the banner asks you to read the release notes and run
  `self-update --version vX --force`. 0.x releases are treated as compatible.

## Rolling back, and the database

Database migrations run forward when the new version starts; old versions
can't open a database a newer version migrated (SQLite migrations are not
reversible, and backup restores refuse a newer schema without `--force`).

- **The new version doesn't start** (systemd keeps restarting it): put the old
  binary back and restart:
  ```sh
  sudo mv /usr/local/bin/aegis-daemon.previous /usr/local/bin/aegis-daemon
  sudo systemctl restart aegis
  ```
  If the new version had already migrated the database, the old one refuses
  to start (`migration ... was previously applied but is missing`). Then also
  restore the pre-update backup (the newest `aegis-backup-*.tar.gz` before the
  `UpdateStarted` event), which loses changes made since the update:
  ```sh
  sudo systemctl stop aegis
  sudo -u aegis AEGIS_CONFIG=/etc/aegis/aegis.toml aegis-daemon admin restore /var/lib/aegis/backups/aegis-backup-....tar.gz
  sudo systemctl start aegis
  ```
  The old version then records `UpdateFailed` (`stage: "restart"`) because it
  is not the version that was installed.
- **Download, verification, backup or helper failure:** nothing was swapped;
  the banner and an `UpdateFailed` event say why (`stage`: `download`,
  `verify`, `install`, `backup`, `request`, `helper`, `restart`). Fix the
  cause and click "Upgrade now" again.
- **The machine dies mid-swap:** each binary is replaced by one rename, so
  each is either old or new. With the CLI and TUI, a crash between renames can
  leave them on different versions; rerun `self-update --force`.

## Events

`UpdateAvailable`, `UpdateStarted`, `UpdateApplied`, `UpdateFailed`; see
[`event_catalog.md`](event_catalog.md#aegis-updates). Security details are in
[`security.md`](security.md#self-update).
