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

1. **Download and verify.** The release's `SHA256SUMS` and its signature
   `SHA256SUMS.minisig` are downloaded over HTTPS. The signature must be
   valid, made by one of the release keys built into your Aegis, and made for
   this release; then the archive for this machine
   (`aegis-<os>-<arch>.tar.gz`) must match its SHA-256 there. Otherwise
   nothing happens (`UpdateFailed` with stage `signature` or `download`). The
   new `aegis-daemon` (and `aegis-cli`/`aegis-tui` if they are installed next
   to it) are written next to the old ones, and the new daemon must answer
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
"Updated to vX. Signed by release key …" (an `UpdateApplied` event with
`signed_by` is recorded by the new version).

## Signed releases

Every release's `SHA256SUMS` is signed with the maintainers' minisign key
(see [`releasing.md`](releasing.md) and
[`security.md`](security.md#self-update)). The keys your build accepts are
shown by `aegis-daemon self-update --check` and `aegis upgrade --check`
("Release keys") and in `release_keys` of `GET /api/v1/system/update`.

**A build without a release key.** Until the maintainers generate the key,
builds contain a placeholder and can't verify any release. Then:

- the dashboard shows the update but no "Upgrade now"; the banner explains
  that this build has no release signing key and gives the command below;
- the update helper refuses (it never installs an unsigned release);
- `aegis-daemon self-update`, `aegis upgrade` and `install.sh` refuse unless
  you add `--allow-unsigned`. With it, only the SHA-256 checksums are checked
  (they come from the same release, so they don't prove who published it);
  a signature that is present but invalid is still refused. It prints a
  warning, and the daemon records the install in the event log
  (`UpdateApplied` with `allow_unsigned: true`) when it next starts.

```sh
# Check the release yourself first, e.g. with minisign and the published key:
#   minisign -Vm SHA256SUMS -x SHA256SUMS.minisig -P RWQ...
sudo env AEGIS_CONFIG=/etc/aegis/aegis.toml aegis-daemon self-update --allow-unsigned --restart-unit aegis.service
```

The same flag also installs an older, unsigned release on a build that has a
key (e.g. `self-update --version v0.7.0 --force --allow-unsigned`).

**A release signed with a key your build doesn't know** (after a key
rotation you skipped) fails with "signed by key X, which this build does not
trust". Re-run install.sh, which carries the current keys, or verify the
release by hand and use `--allow-unsigned` once.

## How it is installed depends on who owns the binary

The banner says which of these applies (`mode` in the API):

| Mode | When | What happens |
| :--- | :--- | :--- |
| `in_place` | The daemon can write the directory its binary is in (per-user install in `~/.local/bin`, a hand-run daemon) | The daemon does all four steps itself. |
| `helper` | `install.sh --server` (root-owned `/usr/local/bin`, daemon running as `aegis` under `ProtectSystem=strict`) | The daemon backs up, then writes `/var/lib/aegis/update/request.json` containing only the version. The root-owned `aegis-update.path` unit starts `aegis-update.service`, which runs `aegis-daemon self-update --from-request`: it deletes the request, downloads and verifies that release **itself**, refuses anything that isn't newer than the installed version, swaps the binaries and restarts `aegis.service`. On failure it writes `result.json`, which the daemon reports as `UpdateFailed`. |
| `manual` | Neither (e.g. installed with `--no-self-update`, or binaries owned by another user) | "Upgrade now" is not offered; admins see the exact command to run, e.g. `sudo env AEGIS_CONFIG=/etc/aegis/aegis.toml /usr/local/bin/aegis-daemon self-update --restart-unit aegis.service`, which takes the pre-update backup itself (see below). |
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

# Per-user install (replaces the binaries next to aegis-cli):
aegis upgrade
```

`self-update` and `aegis upgrade` use the same download, signature and
checksum verification and swap code as the dashboard. install.sh checks the
same signature (with OpenSSL 1.1.1+, or minisign) and checksum before it
installs anything.

`aegis-daemon self-update` takes the same pre-update backup as "Upgrade now"
(database, secrets key and certificates, no releases; `trigger: "update"` in
the `BackupCreated` event) once the new release has been downloaded and
verified, just before the binaries are replaced. If the backup fails, nothing
is installed; `--no-backup` skips it. It works while the daemon runs, and
when run as root the archive is given to the data directory's owner, so the
dashboard can list it and retention can rotate it. The `UpdateApplied` event
the new daemon records names the backup. (`--from-request`, run by the root
helper, takes none: the daemon backed up before asking. `aegis upgrade`, the
per-user CLI command, takes none either; run `aegis-daemon admin backup`
first.)

**Which configuration (and database) it uses.** `aegis-daemon` reads
`$AEGIS_CONFIG` when it is set; otherwise `./aegis.toml` if the current
directory has one; otherwise `/etc/aegis/aegis.toml` if it exists (the file
`install.sh --server` writes); otherwise built-in defaults. So a plain
`sudo aegis-daemon self-update` on a server install finds the server's
database (`/var/lib/aegis/aegis.db`) and data directory without
`AEGIS_CONFIG`. It prints the configuration, database and data directory it
uses before downloading anything. Run as root, the backup still reads the
real database and data directory; the archive (mode 0600) and the backups
directory (mode 0700) are given to the data directory's owner (the `aegis`
user), as are SQLite journal files the backup may have created.

On a server install (configuration in `/etc/aegis/aegis.toml`, or the
`aegis.service` unit installed), a missing database is an error rather than
"nothing to back up": `self-update` stops before downloading, names the
database path and configuration it looked at, and asks you to set
`AEGIS_CONFIG` or to run again with `--no-backup` to update without a backup.

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
- **Download, signature, verification, backup or helper failure:** nothing
  was swapped; the banner and an `UpdateFailed` event say why (`stage`:
  `download`, `signature`, `verify`, `install`, `backup`, `request`,
  `helper`, `restart`). Fix the cause and click "Upgrade now" again. A
  `signature` failure means the release is unsigned, signed by an unknown
  key, or was modified: don't work around it without finding out why.
- **The machine dies mid-swap:** each binary is replaced by one rename, so
  each is either old or new. With the CLI and TUI, a crash between renames can
  leave them on different versions; rerun `self-update --force`.

## Events

`UpdateAvailable`, `UpdateStarted`, `UpdateApplied`, `UpdateFailed`; see
[`event_catalog.md`](event_catalog.md#aegis-updates). Security details are in
[`security.md`](security.md#self-update).
