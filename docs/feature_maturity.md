# Aegis Feature Maturity Taxonomy

To maintain total transparency and build operational trust with maintainers and operators, Aegis classifies every platform capability into one of 4 Maturity Tiers.

---

## 1. Maturity Tiers

| Tier | Icon | Definition & Requirements |
| :--- | :---: | :--- |
| **Experimental** | 🧪 | Implemented and functionally correct, undergoing initial validation. |
| **Validated** | 🟡 | Verified by automated unit, integration, and contract test suites. |
| **Dogfooded** | 🟢 | Used in production by Aegis itself to deploy Aegis. |
| **Production Proven** | 🔵 | Successfully verified in production by external enterprise users. |

---

## 2. Feature Maturity Matrix

| Feature / Capability | Status | Evidence Document |
| :--- | :---: | :--- |
| **Event Store & Event Bus** | 🟢 | [`validation/dogfood/dogfood-report.md`](../validation/dogfood/dogfood-report.md) |
| **Detector Pipeline** | 🟡 | [`validation/compatibility/compatibility-report.md`](../validation/compatibility/compatibility-report.md) |
| **Runtime Process Supervisor** | 🟡 | [`validation/soak/72h-report.md`](../validation/soak/72h-report.md) |
| **Atomic Release Switcher** | 🟡 | [`validation/chaos/chaos-report.md`](../validation/chaos/chaos-report.md) |
| **PM2 & Systemd Migration Engine** | 🟡 | [`validation/migration/migration-report.md`](../validation/migration/migration-report.md) |
| **Deployment Replay (`aegis replay`)** | 🧪 | [`docs/proof_phase.md`](proof_phase.md) |
| **Time Travel Inspection (`aegis inspect --at`)** | 🧪 | [`docs/proof_phase.md`](proof_phase.md) |
| **Outage Investigation Engine (`aegis investigate`)** | 🧪 | [`docs/proof_phase.md`](proof_phase.md) |
