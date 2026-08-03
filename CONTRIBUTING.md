# Contributing to Aegis 🛡️

Thank you for your interest in contributing to **Aegis**! Aegis is an event-sourced deployment platform and process manager built in Rust.

---

## 1. Governance & Architectural Changes (RFC Process)

Aegis enforces a strict architectural freeze. Any major architectural changes, public API modifications, or new domain crates must go through the **RFC Process** before writing code:

1. Review existing RFCs in [`docs/rfcs/`](docs/rfcs/0001-rfc-process.md).
2. Submit a new RFC PR following the template in `docs/rfcs/0001-rfc-process.md`.
3. Discuss and obtain maintainer acceptance before implementation.

---

## 2. Local Development Setup

### Prerequisites
- **Rust**: 1.75+ (`rustup update stable`)
- **SQLite3**: Pre-installed on your system
- **Protocol Buffers Compiler**: `protoc`

### Build & Test
```bash
git clone https://github.com/shivam411/Aegis.git
cd Aegis

# Check compilation across all crates
cargo check --workspace

# Run all unit and integration tests
cargo test --workspace

# Run Clippy lints
cargo clippy --workspace --all-targets -- -D warnings

# Verify code formatting
cargo fmt --all -- --check
```

---

## 3. Pull Request Guidelines

Every Pull Request must:
1. Pass all CI checks (`cargo fmt`, `cargo clippy`, `cargo test`).
2. Include unit or integration tests for new functionality.
3. Update relevant documentation in `docs/` or `README.md`.
4. Follow the PR template format in `.github/PULL_REQUEST_TEMPLATE.md`.
