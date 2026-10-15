# Aegis Public Launch Readiness Guide

This guide lists what must be in place before a public launch. A box is checked only when the item exists and, for quality items, runs in CI.

---

## 1. Launch Preparation Checklist

### Documentation
- [x] Quick start (`README.md`, [`quickstart.md`](quickstart.md)).
- [x] Architecture overview ([`architecture.md`](architecture.md)).
- [x] Public API contract ([`api_contract.md`](api_contract.md)) and HTTP API reference ([`http_api.md`](http_api.md), [`openapi.json`](openapi.json)).
- [x] CLI contract ([`cli_contract.md`](cli_contract.md)).
- [x] Event catalog ([`event_catalog.md`](event_catalog.md)).
- [x] State machines ([`state_machines.md`](state_machines.md)).
- [x] Error taxonomy ([`error_model.md`](error_model.md)).
- [x] Configuration hierarchy ([`configuration.md`](configuration.md)).
- [x] Schema versioning policy ([`versioning.md`](versioning.md)).
- [x] Security model ([`security.md`](security.md)).
- [x] Service Level Objectives defined ([`slo_targets.md`](slo_targets.md)). They are targets; none is measured yet.

### Quality
- [x] `cargo fmt`, `cargo clippy -D warnings` and `cargo test --workspace` pass in CI ([`ci.yml`](../.github/workflows/ci.yml)).
- [x] End-to-end deploy scenarios run in CI against the real binaries ([`scripts/e2e-smoke.sh`](../scripts/e2e-smoke.sh)).
- [x] Dashboard browser tests run in CI ([`ui-tests/`](../ui-tests/)).
- [x] Benchmarks compile in CI (`benchmarks/`).
- [ ] Benchmarks run in CI with regression thresholds.
- [ ] Tests run on macOS and Windows (they are only built today).
- [ ] Production certification scenarios all covered by tests ([`production_certification.md`](production_certification.md); most are).
- [ ] Soak test completed and published ([`soak_testing.md`](soak_testing.md)).
- [ ] Aegis deploys itself ([`dogfooding.md`](dogfooding.md) is a design, not yet running).
- [ ] Experimental CLI commands (`replay`, `inspect --at`, `incident`, `investigate`, `upgrade`, `migrate`) either tested or clearly marked experimental in `--help` ([`feature_maturity.md`](feature_maturity.md)).

### Open Source Community Governance
- [x] Contributor guidelines (`CONTRIBUTING.md`).
- [x] Code of conduct (`CODE_OF_CONDUCT.md`).
- [x] Security reporting policy (`SECURITY.md`).
- [x] Community support guide (`SUPPORT.md`).
- [x] Issue templates (`.github/ISSUE_TEMPLATE/`).
- [x] PR template (`.github/PULL_REQUEST_TEMPLATE.md`).
- [x] CI workflow (`.github/workflows/ci.yml`).
