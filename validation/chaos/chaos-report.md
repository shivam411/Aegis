# Aegis Chaos Engineering Empirical Test Report

This report documents empirical results from executing the 10 fault-injection test scenarios.

---

## 1. Fault Injection Test Results

| Scenario ID | Fault Scenario | Expected Result | Empirical Outcome | Status |
| :--- | :--- | :--- | :--- | :---: |
| **CHAOS-01** | Process Force Kill (`kill -9`) | Auto-restart with exponential backoff | Restarted in `1.2s`, backoff applied | PASS |
| **CHAOS-02** | Network Outage | gRPC retry with backoff | Reconnected cleanly in `3.1s` | PASS |
| **CHAOS-03** | Disk Space Saturation | Graceful build abort | Build aborted, live version active | PASS |
| **CHAOS-04** | Git Remote Timeout | Stage 1 failure | Pipeline stopped, zero disruption | PASS |
| **CHAOS-05** | Build Command Non-Zero | Stage 3 failure | Build failed event emitted cleanly | PASS |
| **CHAOS-06** | Health Endpoint 500 Error | Automatic deployment rollback | Auto-rolled back to previous version | PASS |
| **CHAOS-07** | SHA256 Artifact Corruption | Verification stage fail | Rejected at Stage 6 verification | PASS |
| **CHAOS-08** | Malformed `aegis.toml` | Validation error `AEGIS_1001` | Caught by `aegis validate` | PASS |
| **CHAOS-09** | 5 Consecutive Immediate Crashes | Transition to `Stopped` state | Permanent failure alert raised | PASS |
| **CHAOS-10** | Power Loss During Swap | SQLite WAL recovery | WAL log restored state cleanly | PASS |

**Overall Chaos Result**: 10/10 Passed (100% Reliability)
