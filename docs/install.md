# Installation Guide

## One-Line Installer (Recommended)

The fastest way to install Aegis on Linux or macOS:

```bash
curl -fsSL https://raw.githubusercontent.com/shivam411/Aegis/main/install.sh | bash
```

This script automatically:

1. Detects your operating system (Linux / macOS)
2. Detects your CPU architecture (x86_64 / ARM64)
3. Queries the latest stable release from GitHub
4. Downloads the correct binary archive
5. Installs `aegis-daemon`, `aegis-cli`, and `aegis-tui` to `~/.local/bin/`, plus a link `aegis -> aegis-cli`
6. Verifies your `PATH` includes `~/.local/bin`

After installation, verify:

```bash
aegis --version
```

**`aegis` and `aegis-cli`.** The CLI binary is `aegis-cli`. The installer (both the user install and `--server`, which uses `/usr/local/bin`) adds a symbolic link named `aegis` next to it, and the documentation uses `aegis ...` throughout. Upgrades (`aegis upgrade`, the dashboard, `aegis-daemon self-update`) replace `aegis-cli` in place, so the link keeps working. If something else named `aegis` is already in that directory, the installer leaves it alone and you run `aegis-cli` instead. With a manual install or a source build, either create the link yourself (`ln -s aegis-cli ~/.local/bin/aegis`) or type `aegis-cli` wherever the docs say `aegis`.

---

## Manual Binary Download

If you prefer to download manually:

1. Visit the [GitHub Releases](https://github.com/shivam411/Aegis/releases/latest) page.
2. Download the archive matching your platform:

| Platform | Archive |
| :--- | :--- |
| Linux x86_64 | `aegis-linux-amd64.tar.gz` |
| Linux ARM64 | `aegis-linux-arm64.tar.gz` |
| macOS x86_64 | `aegis-darwin-amd64.tar.gz` |
| macOS ARM64 (Apple Silicon) | `aegis-darwin-arm64.tar.gz` |
| Windows x86_64 | `aegis-windows-amd64.zip` |

3. Extract and move binaries to your PATH:

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

Binaries will be at:

```
target/release/aegis-daemon
target/release/aegis-cli
target/release/aegis-tui
```

Copy them to a directory on your `PATH`:

```bash
cp target/release/aegis-daemon target/release/aegis-cli target/release/aegis-tui ~/.local/bin/
ln -sfn aegis-cli ~/.local/bin/aegis    # optional: the name the docs use
```

---

## Uninstall

To remove Aegis:

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

Once installed, follow the [Quickstart Guide](quickstart.md) to deploy your first application.
