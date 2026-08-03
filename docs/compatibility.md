# Aegis OS & Runtime Support Compatibility Matrix

This document tracks operating system platforms and polyglot runtime compatibility for Aegis.

---

## 1. Operating System Compatibility Matrix

| Operating System | Architecture | Binary Package | Status | Notes |
| :--- | :--- | :--- | :--- | :--- |
| **Linux (Ubuntu / Debian / RHEL)** | `x86_64` | `aegis-linux-amd64.tar.gz` | ✅ Supported | Primary production target. Zero dependencies. |
| **Linux (Alpine / Arch)** | `arm64` | `aegis-linux-arm64.tar.gz` | ✅ Supported | Full support for ARM64 cloud instances & Raspberry Pi. |
| **macOS (Apple Silicon)** | `arm64` | `aegis-darwin-arm64.tar.gz` | ✅ Supported | macOS local development environment target. |
| **macOS (Intel)** | `x86_64` | `aegis-darwin-amd64.tar.gz` | ✅ Supported | Intel Mac development target. |
| **Windows 10 / 11 / Server** | `x86_64` | `aegis-windows-amd64.zip` | ✅ Supported | Native Windows process supervision & gRPC support. |

---

## 2. Polyglot Runtime Compatibility Matrix

| Runtime Engine | Indicator File | Supported Commands | Build Strategy | Status |
| :--- | :--- | :--- | :--- | :--- |
| **Rust** | `Cargo.toml` | `cargo build --release` | Binary compilation | ✅ Supported |
| **Node.js / Bun / Deno** | `package.json` | `npm install`, `npm run build` | Process / JS runtime | ✅ Supported |
| **Go** | `go.mod` | `go mod download`, `go build` | Binary compilation | ✅ Supported |
| **Python** | `requirements.txt` / `pyproject.toml` | `pip install -r requirements.txt` | Virtualenv / Process | ✅ Supported |
| **Docker Container** | `Dockerfile` | `docker build`, `docker run` | Container image | ⚠️ Experimental |
| **Generic CLI Executable** | User-defined | Custom build & start commands | Direct OS execution | ✅ Supported |
