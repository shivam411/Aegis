# System

Admins only. Reached from **System** in the top bar (<kbd>g</kbd> <kbd>s</kbd>).

## Updates

Shows the version you run, the latest release, and when Aegis last checked.

- **Check now** asks GitHub for the latest release.
- **Upgrade to …** appears when Aegis can update itself. It backs up, downloads, verifies the signature, swaps the binary and restarts. Apps stop for a few seconds.
- Where it can't (package-managed installs), the card shows the command to run on the server.

More in [Aegis updates](updates.md) and [upgrading](../upgrading.md).

## Storage

What Aegis keeps on disk, split into:

| Category | Cleaned? |
| :--- | :--- |
| Live and rollback releases | Never |
| Old builds (outside the rollback window, failed builds) | Yes |
| Logs | Files not written for N days (default 14) |
| Backups and database snapshots | Those beyond `[backup] keep` |
| Database | Never |
| Temporary files | Older than a day |

**Clean up** lets you pick what to remove and shows what it frees. The live release, rollback targets, the database and the secrets key are never touched.

**Automatic cleanup** does the same every hour. It is off until you tick it. Set how many days of logs to keep, then **Save**. Each cleanup is recorded as `StorageCleaned` in [Events](events.md).

The table lists disk use per app.
