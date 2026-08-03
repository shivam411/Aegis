# Aegis Closed Beta Program Framework

This document outlines the evaluation framework for closed beta participants testing Aegis.

---

## 1. Beta Cohort Distribution

| Technology Stack | Target Cohort Size | Focus Area |
| :--- | :---: | :--- |
| **Node.js (Next.js / Express)** | 5 Teams | Process swap, PM2 migration, zero-downtime deploys |
| **Java (Spring Boot)** | 5 Teams | JVM restart management, systemd migration, health probes |
| **Rust (Axum / Actix)** | 3 Teams | High-throughput event streaming, binary swaps |
| **Python (FastAPI / Django)** | 3 Teams | Virtual environment management, process monitoring |
| **Go (Fiber / Gin)** | 2 Teams | Lightweight process lifecycle, fast health checks |

---

## 2. Beta Evaluation Scenarios
Every participant completes 8 standardized operational scenarios:
1. Fresh installation via `install.sh`.
2. Import existing application or config (`aegis init` / `aegis migrate`).
3. Execute deployment (`aegis deploy`).
4. Trigger manual restart (`aegis restart`).
5. Recover from simulated process crash.
6. Perform rollback (`aegis rollback`).
7. Inspect logs & operational events (`aegis logs`, `aegis events`).
8. Submit feedback scorecard.
