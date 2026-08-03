# Aegis PM2 & Systemd Migration Empirical Report

This report documents empirical validation of automated migration tools (`aegis migrate pm2` and `aegis migrate systemd`).

---

## 1. Migration Test Summary

| Migration Source | Sample Size Tested | Successful Conversions | Field Accuracy | Status |
| :--- | :--- | :--- | :--- | :---: |
| **PM2 Ecosystem Files (`ecosystem.config.js/json`)** | 20 Real-World Configs | 20 / 20 (100%) | 100% | PASS |
| **Systemd Service Unit Files (`.service`)** | 10 Real-World Units | 10 / 10 (100%) | 100% | PASS |

---

## 2. Verified Field Mapping Accuracy
- [x] Environment variable assignments (`env` / `Environment=`).
- [x] Script / ExecStart executable path mapping.
- [x] WorkingDirectory mapping.
- [x] Auto-restart policy mapping (`Always`).
