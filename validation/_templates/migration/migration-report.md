# Aegis PM2 & systemd Migration Report (Template)

> **Template — results below are illustrative, not measured.**
> No migration run has been recorded yet. An earlier version of this file reported 20/20 PM2 and 10/10 systemd conversions (see git history); there was no corpus or test behind those numbers. Today `aegis migrate systemd` reads only `ExecStart=`, and `aegis migrate pm2` doesn't parse the ecosystem file's fields (it only checks for a `name`). To publish a real report, check in a corpus of config files and a test that converts them, fill in every `TBD`, and save the filled-in copy as `validation/migration/migration-report.md`.

---

## 0. Run details

| Field | Value |
| :--- | :--- |
| Date | `TBD` |
| Aegis commit | `TBD` |
| Corpus | `TBD` (path in this repo) |
| Test / CI job | `TBD` |

## 1. Summary

| Source | Files tested | Converted | Fields mapped correctly | Status |
| :--- | :--- | :--- | :--- | :---: |
| **PM2 ecosystem files** | `TBD` | `TBD` | `TBD` | `TBD` |
| **systemd unit files** | `TBD` | `TBD` | `TBD` | `TBD` |

## 2. Field mapping

| Field | PM2 | systemd |
| :--- | :---: | :---: |
| Environment variables (`env` / `Environment=`) | `TBD` | `TBD` |
| Start command (`script` / `ExecStart=`) | `TBD` | `TBD` |
| Working directory (`cwd` / `WorkingDirectory=`) | `TBD` | `TBD` |
| Restart policy (`autorestart` / `Restart=`) | `TBD` | `TBD` |
