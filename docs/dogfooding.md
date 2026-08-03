# Aegis Dogfooding Deployment Architecture

This document specifies the "Aegis deploying Aegis" self-deployment pipeline.

Aegis is maintained and deployed using Aegis itself on production infrastructure.

---

## 1. Dogfood Pipeline Architecture

```
GitHub Push (main branch)
       ↓
GitHub Actions CI Build (Linux amd64 / arm64)
       ↓
Publish Aegis Release Artifacts
       ↓
Webhook Signal to Live Aegis Daemon
       ↓
Aegis Builder Stage: Package & Verify Aegis Binary
       ↓
Aegis Strategy Stage: GracefulSwitch to New Aegis Daemon
       ↓
Pre-Activation Health Probe (gRPC GetStatus HTTP/gRPC check)
       ↓
Atomic Symlink Pointer Swap
       ↓
Graceful Drain & Restart of Daemon Process
```

---

## 2. Advantages of Dogfooding
1. **Immediate Feedback**: Weaknesses in signal handling, log streaming, or configuration reloading are discovered internally before end users encounter them.
2. **Stress Testing**: Verifies zero-downtime gRPC streaming and event persistence during live upgrades.
