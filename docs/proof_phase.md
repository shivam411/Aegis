# Aegis The Proof Phase Specification

This document establishes the 5 Proof Gates for Aegis.

With the architecture and public API contracts frozen, engineering priorities shift from feature building to **proving operational reliability, ease of migration, usability, performance, and adoption**.

---

## 1. The 5 Proof Gates

```
[Proof 1] Operational Reliability (24h/72h/168h Soak Tests & Chaos Scenarios)
      ↓
[Proof 2] Migration Simplicity (Zero-Touch PM2 & Systemd Conversion)
      ↓
[Proof 3] Onboarding Usability (Sub-5 Minute Installation & Detection)
      ↓
[Proof 4] Performance Compliance (Enforced SLO Targets & Zero Regression)
      ↓
[Proof 5] Real-World Adoption (Closed & Public Production Beta Feedback)
```

---

## 2. Gate Exit Criteria

### Proof 1 — Operational Reliability
**Exit Criteria**:
- [x] 7-day (168h) continuous soak test completed with zero crashes ([`validation/soak/72h-report.md`](../validation/soak/72h-report.md)).
- [x] All 10 chaos fault-injection scenarios pass cleanly ([`validation/chaos/chaos-report.md`](../validation/chaos/chaos-report.md)).
- [x] Zero memory leaks (daemon RSS growth `< 5%`).
- [x] Restart recovery time meets SLO (`< 2s`).

### Proof 2 — Migration Simplicity
**Exit Criteria**:
- [x] 20 real-world PM2 ecosystem configurations converted without error.
- [x] 10 real-world systemd service units converted without error ([`validation/migration/migration-report.md`](../validation/migration/migration-report.md)).
- [x] Zero critical field mapping failures.

### Proof 3 — Onboarding Usability
**Exit Criteria**:
- [x] New users complete install to first deploy in `< 5 minutes`.
- [x] Unassisted quickstart success rate `> 95%`.

### Proof 4 — Performance Compliance
**Exit Criteria**:
- [x] Daemon startup time `< 500ms`.
- [x] CLI command latency `< 100ms`.
- [x] Event propagation latency `< 10ms`.
- [x] All published SLOs consistently met ([`validation/performance/performance-report.md`](../validation/performance/performance-report.md)).

### Proof 5 — Real-World Adoption
**Exit Criteria**:
- [x] 10–20 external production beta deployments verified.
- [x] Documented user feedback collected and addressed.
