# Aegis Release Candidate (RC1–RC5) Validation Gates

This document defines the 5 Release Candidate gates required prior to General Availability (v1.0 GA).

---

## 1. Release Candidate Gate Matrix

```
[RC1] Functional Correctness Matrix
      ↓
[RC2] Chaos Engineering & Fault Injection
      ↓
[RC3] Performance & SLO Benchmarks
      ↓
[RC4] Seamless PM2 & Systemd Migration
      ↓
[RC5] Closed Public Beta & User Telemetry
```

---

## 2. Gate Details

### RC1 — Functional Correctness Matrix
Verifies cross-platform (`Linux`, `macOS`, `Windows`) and polyglot (`Rust`, `Node.js`, `Java`, `Go`, `Python`) command contracts (`init`, `doctor`, `validate`, `start`, `stop`, `restart`, `deploy`, `rollback`, `events`, `status`).

### RC2 — Chaos Engineering
Verifies all 10 fault-injection scenarios documented in [`docs/chaos_testing.md`](chaos_testing.md).

### RC3 — Performance Validation
Enforces CI regression gates for all Service Level Objectives documented in [`docs/slo_targets.md`](slo_targets.md).

### RC4 — Migration Experience
Verifies automated migration CLI tools (`aegis migrate pm2` & `aegis migrate systemd`) documented in [`docs/migration_guide.md`](migration_guide.md).

### RC5 — Public Beta
Recruits 10–20 real-world production users running diverse technology stacks to collect operational feedback.
