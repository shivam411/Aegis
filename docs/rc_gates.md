# Aegis Release Candidate (RC1–RC5) Validation Gates

This document defines the 5 Release Candidate gates required before General Availability (v1.0).

> **Status:** Aegis is pre-1.0. RC1 is partly covered by CI; RC2–RC5 have not started. See the [scorecard](scorecard.md).

---

## 1. Release Candidate Gate Matrix

```
[RC1] Functional Correctness Matrix
      ↓
[RC2] Chaos Engineering & Fault Injection
      ↓
[RC3] Performance & SLO Benchmarks
      ↓
[RC4] PM2 & systemd Migration
      ↓
[RC5] Closed Public Beta
```

---

## 2. Gate Details

### RC1 — Functional Correctness Matrix
Verifies the command contracts (`init`, `doctor`, `validate`, `start`, `stop`, `restart`, `deploy`, `rollback`, `events`, `status`) across platforms (Linux, macOS, Windows) and stacks (Rust, Node.js, Java, Go, Python).

*Status: partly covered.* CI runs these commands against a Node.js app on Linux ([`e2e-smoke.sh`](../scripts/e2e-smoke.sh)). Other platforms are only built; other stacks aren't deployed in CI.

### RC2 — Chaos Engineering
Verifies the 10 fault-injection scenarios in [`chaos_testing.md`](chaos_testing.md).

*Status: partly covered.* Some scenarios are tested in CI (see that page); there is no chaos run yet.

### RC3 — Performance Validation
Enforces CI regression gates for the targets in [`slo_targets.md`](slo_targets.md).

*Status: not started.* Benchmarks compile in CI but aren't run, and there are no thresholds.

### RC4 — Migration Experience
Verifies `aegis migrate pm2` and `aegis migrate systemd` ([`migration_guide.md`](migration_guide.md)) against real config files.

*Status: not started.* Both commands are experimental and untested.

### RC5 — Public Beta
Recruits 10–20 production users on different stacks to collect operational feedback ([`beta_program.md`](beta_program.md)).

*Status: not started.*
