# Aegis Standardized Release Process & Compatibility Matrix

This document defines the release process and compatibility badge requirements for every official Aegis release.

---

## 1. Compatibility Matrix Badges

Every Aegis release publishes a verified compatibility matrix badge report across operating systems and polyglot application runtimes:

```text
[OS Compatibility Badges]
Linux (x86_64):           ✅ PASS
Linux (ARM64):            ✅ PASS
macOS (Intel):            ✅ PASS
macOS (Apple Silicon):    ✅ PASS
Windows (x86_64):         ✅ PASS

[Polyglot Stack Badges]
Rust (Cargo):             ✅ PASS
Node.js (npm/yarn/pnpm):  ✅ PASS
Java (Gradle/Maven):      ✅ PASS
Python (pip/poetry):      ✅ PASS
Go (go.mod):              ✅ PASS
```

---

## 2. Mandatory Release Artifact Checklist
Every release tag (e.g. `v0.4.0`) MUST include the following 4 sections in its Release Notes:
1. **Release Notes**: Summary of new capabilities, bug fixes, breaking changes, and migration notes.
2. **Validation Summary**: Empirical evidence report covering benchmarks, chaos results, soak telemetry, and dogfood status.
3. **Compatibility**: Formally supported operating systems, CPU architectures, and runtime stacks.
4. **Known Limitations**: Explicit disclosure of active limitations and experimental features.
