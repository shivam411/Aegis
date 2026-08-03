# Aegis Version Compatibility Policy

This document defines version compatibility guarantees and deprecation policies across Aegis components.

---

## 1. Compatibility Policy Matrix

| Interface Boundary | Policy Guarantee | Migration / Deprecation Policy |
| :--- | :--- | :--- |
| **CLI ↔ Daemon (`gRPC`)** | Same minor version supported | Backward compatible within minor releases (`v0.x`). |
| **Plugin API** | Stable within major version (`v1.x`) | Deprecation warning 2 minor releases prior to breaking changes. |
| **Event Schema** | Backward compatible within `v1` | Schema version header `schema_version` incremented on changes. |
| **`aegis.toml`** | Auto-migration where possible | Config parser handles legacy field defaults automatically. |

---

## 2. Upgrade Safety Policy
Running `aegis upgrade` will perform a zero-downtime binary swap of the daemon and CLI, verifying health probes before replacing active binary symlinks.
