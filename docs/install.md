# Installation Guide

## One-Line Installer (Recommended)

Linux or macOS:

```bash
curl -fsSL https://raw.githubusercontent.com/shivam411/Aegis/main/install.sh | bash
```

It detects your OS and CPU (x86_64 / ARM64), downloads the latest release, installs `aegis-daemon`, `aegis-cli` and `aegis-tui` to `~/.local/bin/` with a link `aegis -> aegis-cli`, and checks your `PATH`.

Verify:

```bash
aegis --version
```

**`aegis` and `aegis-cli`.** The binary is `aegis-cli`; the installer (user install and `--server`, which uses `/usr/local/bin`) adds an `aegis` link, and the docs use `aegis ...`. Upgrades keep the link working. If another `aegis` exists there, the installer leaves it and you run `aegis-cli`. For manual or source installs, create the link (`ln -s aegis-cli ~/.local/bin/aegis`) or type `aegis-cli`.

---

## Manual Binary Download

1. Open [GitHub Releases](https://github.com/shivam411/Aegis/releases/latest).
2. Download your archive:

| Platform | Archive |
| :--- | :--- |
| Linux x86_64 | `aegis-linux-amd64.tar.gz` |
| Linux ARM64 | `aegis-linux-arm64.tar.gz` |
| macOS x86_64 | `aegis-darwin-amd64.tar.gz` |
| macOS ARM64 (Apple Silicon) | `aegis-darwin-arm64.tar.gz` |
| Windows x86_64 | `aegis-windows-amd64.zip` |

3. Extract to your PATH:

```bash
tar -xzf aegis-linux-amd64.tar.gz
mv aegis-daemon aegis-cli aegis-tui ~/.local/bin/
chmod +x ~/.local/bin/aegis-*
ln -sfn aegis-cli ~/.local/bin/aegis    # optional: the name the docs use
```

---

## Build from Source

Prerequisites:

- Rust 1.75+ (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)
- SQLite3 development headers
- Protocol Buffers compiler (`protoc`)

```bash
git clone https://github.com/shivam411/Aegis.git
cd Aegis
cargo build --release
```

Binaries:

```
target/release/aegis-daemon
target/release/aegis-cli
target/release/aegis-tui
```

Copy to your `PATH`:

```bash
cp target/release/aegis-daemon target/release/aegis-cli target/release/aegis-tui ~/.local/bin/
ln -sfn aegis-cli ~/.local/bin/aegis    # optional: the name the docs use
```

---

## Uninstall

```bash
curl -fsSL https://raw.githubusercontent.com/shivam411/Aegis/main/scripts/uninstall.sh | bash
```

Or manually:

```bash
rm -f ~/.local/bin/aegis ~/.local/bin/aegis-daemon ~/.local/bin/aegis-cli ~/.local/bin/aegis-tui
rm -rf ~/.aegis
```

The uninstaller removes `~/.local/bin/aegis` only when it is the link to `aegis-cli`. For a `--server` install, stop and disable the services (`systemctl disable --now aegis aegis-update.path`), remove their unit files from `/etc/systemd/system/`, and remove `/usr/local/bin/aegis` (the link) along with `/usr/local/bin/aegis-daemon`, `aegis-cli` and `aegis-tui`.

---

## Next Steps

Follow the [Quickstart Guide](quickstart.md) to deploy your first app.
