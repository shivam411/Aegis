# Security Policy & Vulnerability Reporting 🔒

Aegis takes security seriously. We appreciate your efforts to responsibly disclose security vulnerabilities.

---

## Supported Versions

| Version | Supported |
| :--- | :--- |
| `v0.3.x` (MLP) | ✅ Supported |
| `< v0.3.0` | ❌ End of Life |

---

## Reporting a Vulnerability

**Please DO NOT open a public GitHub issue to report a security vulnerability.**

Instead, please report security issues privately via email:
- **Email**: `shivam.antigravity@gmail.com`
- **Subject**: `[SECURITY] Aegis Vulnerability Report`

### What to Include:
1. Description of the vulnerability and potential impact.
2. Steps to reproduce or proof-of-concept exploit.
3. Affected components (`aegis-daemon`, `aegis-cli`, `aegis-event-store`, etc.).

### Response Timeline:
- **Acknowledgement**: Within 48 hours.
- **Assessment & Fix Plan**: Within 7 business days.
- **Security Release**: Issued as a patch update (`v0.3.x`).

---

## Verifying Releases

Every release's `SHA256SUMS` is signed with the maintainers' minisign key (`SHA256SUMS.minisig`). Aegis's self-update, `aegis upgrade` and `install.sh` refuse releases without a valid signature by a key listed in [`core/update/keys/release.pub`](core/update/keys/release.pub). To check a download by hand:

```sh
minisign -Vm SHA256SUMS -x SHA256SUMS.minisig -P <key from core/update/keys/release.pub>
sha256sum -c --ignore-missing SHA256SUMS
```

Key setup, rotation and what to do if the signing key leaks: [docs/releasing.md](docs/releasing.md); the trust model: [docs/security.md](docs/security.md#self-update).
