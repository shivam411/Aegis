# Aegis Proof Gates

This document defines the gates Aegis must pass to show it is reliable, easy to migrate to, easy to use, fast, and used by real people.

> **Status (2026-10):** none of these gates has been passed. An earlier version of this page checked every box and linked to reports that were not based on real runs; those reports are now templates in [`validation/_templates/`](../validation/_templates/). A box is checked only when a report produced by a checked-in script (or a documented real deployment) is linked next to it. See the [scorecard](scorecard.md) for what CI verifies today.

---

## 1. The Gates

```
[Proof 1] Operational Reliability (soak tests and chaos scenarios)
      ↓
[Proof 2] Migration Simplicity (PM2 and systemd conversion)
      ↓
[Proof 3] Onboarding Usability (install to first deploy)
      ↓
[Proof 4] Performance Compliance (SLO targets, no regressions)
      ↓
[Proof 5] Real-World Adoption (beta users in production)
      ↓
[Proof 6] System Recoverability
```

---

## 2. Exit Criteria

### Proof 1 — Operational Reliability
- [ ] 7-day (168 h) soak test with zero crashes. Not run. Protocol: [`soak_testing.md`](soak_testing.md); template: [`soak-report.md`](../validation/_templates/soak/soak-report.md).
- [ ] All 10 chaos scenarios pass. Partly covered by CI (see [`chaos_testing.md`](chaos_testing.md)); template: [`chaos-report.md`](../validation/_templates/chaos/chaos-report.md).
- [ ] No memory leaks (daemon RSS growth `< 5%` over the soak). Not measured.
- [ ] Restart recovery meets the SLO (`< 2 s`). Not measured.

### Proof 2 — Migration Simplicity
- [ ] 20 real-world PM2 ecosystem files converted without error. Not run; `aegis migrate pm2` doesn't parse ecosystem fields yet.
- [ ] 10 real-world systemd units converted without error. Not run; `aegis migrate systemd` reads only `ExecStart=`. Template: [`migration-report.md`](../validation/_templates/migration/migration-report.md).
- [ ] No critical field-mapping failures.

### Proof 3 — Onboarding Usability
- [ ] New users go from install to first deploy in `< 5 minutes`. Not measured.
- [ ] Unassisted quickstart success rate `> 95%`. Not measured.

### Proof 4 — Performance Compliance
- [ ] Daemon startup `< 500 ms`. Timed by the CI soak (launch until `/healthz` answers, see [`benchmarks.md`](benchmarks.md)); not enforced.
- [ ] CLI command latency `< 100 ms`. Not measured.
- [ ] Event propagation latency `< 10 ms`. Not measured.
- [ ] All published SLOs met ([`slo_targets.md`](slo_targets.md)). Some benchmarks run in CI ([`benchmarks.md`](benchmarks.md)), but most SLOs have none and none is enforced. Template: [`performance-report.md`](../validation/_templates/performance/performance-report.md).

### Proof 5 — Real-World Adoption
- [ ] 10–20 external production beta deployments. None yet ([`beta_program.md`](beta_program.md)).
- [ ] User feedback collected and addressed.

### Proof 6 — System Recoverability
- [ ] Database recovery after power loss ([`disaster_recovery.md`](disaster_recovery.md)). Not tested.
- [x] A failed deployment leaves the live release serving. Tested in CI: failed builds and unhealthy releases ([`lifecycle.rs`](../core/control/tests/lifecycle.rs), [`e2e-smoke.sh`](../scripts/e2e-smoke.sh)). Cutover itself still has a short gap until the Phase 5 reverse proxy.
- [x] Apps come back after a daemon restart, and stopped apps stay stopped. Tested in CI (same files).
- [ ] Manual fallback path for release binaries outside Aegis's control. Not tested.
