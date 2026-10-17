# Aegis OS & Runtime Support Compatibility Matrix

This document tracks operating system platforms and polyglot runtime compatibility for Aegis.

> **What "tested" means today:** CI runs all tests on Ubuntu x86_64 and deploys a Node.js app there. macOS and Windows binaries are built in CI but not tested; Linux ARM64 and macOS x86_64 are cross-built by the release workflow only. Cgroup resource limits, process groups and signals are Linux/Unix features. See the [scorecard](scorecard.md).

---

## 1. Operating System Compatibility Matrix

| Operating System | Architecture | Binary Package | Status | Notes |
| :--- | :--- | :--- | :--- | :--- |
| **Linux (Ubuntu / Debian / RHEL)** | `x86_64` | `aegis-linux-amd64.tar.gz` | ✅ Tested in CI | Primary target. |
| **Linux (Alpine / Arch)** | `arm64` | `aegis-linux-arm64.tar.gz` | 🧪 Built, untested | Cross-compiled in the release workflow. |
| **macOS (Apple Silicon)** | `arm64` | `aegis-darwin-arm64.tar.gz` | 🧪 Built, untested | Local development. No cgroup resource limits. |
| **macOS (Intel)** | `x86_64` | `aegis-darwin-amd64.tar.gz` | 🧪 Built, untested | Local development. No cgroup resource limits. |
| **Windows 10 / 11 / Server** | `x86_64` | `aegis-windows-amd64.zip` | 🧪 Built, untested | Process supervision on Windows is untested. |

---

## 2. Polyglot Runtime Compatibility Matrix

| Runtime Engine | Indicator File | Supported Commands | Build Strategy | Status |
| :--- | :--- | :--- | :--- | :--- |
| **Rust** | `Cargo.toml` | `cargo build --release` | Binary compilation | 🟡 Detected; not deploy-tested |
| **Node.js / Bun / Deno** | `package.json` | `npm install`, `npm run build` | Process / JS runtime | ✅ Node.js deploy-tested in CI |
| **Go** | `go.mod` | `go mod download`, `go build` | Binary compilation | 🟡 Detected; not deploy-tested |
| **Python** | `requirements.txt` / `pyproject.toml` | `pip install -r requirements.txt` | Virtualenv / Process | 🟡 Detected; not deploy-tested |
| **Docker Container** | `Dockerfile` + `[run] runtime = "docker"` | `docker build` (or a prebuilt `image`), supervised container | Container image per release | 🧪 Experimental; deploy-tested in CI against a real Docker daemon ([guide](docker_runtime.md)) |
| **Generic CLI Executable** | User-defined | Custom build & start commands | Direct OS execution | ✅ Supported |
