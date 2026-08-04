#!/usr/bin/env bash
#
# Aegis Installer
# https://github.com/shivam411/Aegis
#
# Usage:
#   curl -fsSL https://shivam411.github.io/Aegis/install.sh | bash
#
# This script:
#   1. Detects OS and CPU architecture
#   2. Queries the latest release from GitHub
#   3. Downloads the correct binary archive
#   4. Installs to ~/.local/bin/
#   5. Verifies PATH
#

set -euo pipefail

REPO="shivam411/Aegis"
INSTALL_DIR="${HOME}/.local/bin"
GITHUB_API="https://api.github.com/repos/${REPO}/releases/latest"

# ──────────────────────────────────────────────
# Colors
# ──────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

info()  { echo -e "${CYAN}${BOLD}[info]${NC}  $*"; }
ok()    { echo -e "${GREEN}${BOLD}[  ok]${NC}  $*"; }
warn()  { echo -e "${YELLOW}${BOLD}[warn]${NC}  $*"; }
err()   { echo -e "${RED}${BOLD}[fail]${NC}  $*"; exit 1; }

# ──────────────────────────────────────────────
# Detect OS
# ──────────────────────────────────────────────
detect_os() {
    local os
    os="$(uname -s)"
    case "${os}" in
        Linux*)  echo "linux" ;;
        Darwin*) echo "darwin" ;;
        MINGW*|MSYS*|CYGWIN*) echo "windows" ;;
        *) err "Unsupported operating system: ${os}" ;;
    esac
}

# ──────────────────────────────────────────────
# Detect Architecture
# ──────────────────────────────────────────────
detect_arch() {
    local arch
    arch="$(uname -m)"
    case "${arch}" in
        x86_64|amd64)  echo "amd64" ;;
        aarch64|arm64) echo "arm64" ;;
        *) err "Unsupported architecture: ${arch}" ;;
    esac
}

# ──────────────────────────────────────────────
# Check for required tools
# ──────────────────────────────────────────────
check_deps() {
    for cmd in curl tar; do
        if ! command -v "${cmd}" &>/dev/null; then
            err "Required command not found: ${cmd}. Please install it and try again."
        fi
    done
}

# ──────────────────────────────────────────────
# Query latest release tag from GitHub API
# ──────────────────────────────────────────────
get_latest_version() {
    local version
    version=$(curl -fsSL "${GITHUB_API}" | grep '"tag_name"' | head -1 | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')
    if [ -z "${version}" ]; then
        err "Failed to fetch latest release version from GitHub."
    fi
    echo "${version}"
}

# ──────────────────────────────────────────────
# Main
# ──────────────────────────────────────────────
main() {
    echo ""
    echo -e "${BOLD}  ╔══════════════════════════════════════╗${NC}"
    echo -e "${BOLD}  ║        ${CYAN}Aegis Installer${NC}${BOLD}               ║${NC}"
    echo -e "${BOLD}  ║   Zero-downtime deployment platform  ║${NC}"
    echo -e "${BOLD}  ╚══════════════════════════════════════╝${NC}"
    echo ""

    check_deps

    local os arch version archive_name download_url

    os="$(detect_os)"
    arch="$(detect_arch)"
    version="$(get_latest_version)"

    info "Detected:  OS=${os}  Arch=${arch}"
    info "Latest release: ${version}"

    if [ "${os}" = "windows" ]; then
        archive_name="aegis-${os}-${arch}.zip"
    else
        archive_name="aegis-${os}-${arch}.tar.gz"
    fi

    download_url="https://github.com/${REPO}/releases/download/${version}/${archive_name}"

    # Create install directory
    mkdir -p "${INSTALL_DIR}"

    # Download
    info "Downloading ${archive_name}..."
    TMP_DIR="$(mktemp -d)"
    trap 'rm -rf "${TMP_DIR:-}"' EXIT

    curl -fsSL -o "${TMP_DIR}/${archive_name}" "${download_url}" \
        || err "Download failed. Check that release ${version} has asset ${archive_name}."

    ok "Downloaded ${archive_name}"

    # Extract
    info "Extracting..."
    if [ "${os}" = "windows" ]; then
        unzip -qo "${TMP_DIR}/${archive_name}" -d "${TMP_DIR}/aegis" 2>/dev/null \
            || err "Extraction failed. Is 'unzip' installed?"
    else
        mkdir -p "${TMP_DIR}/aegis"
        tar -xzf "${TMP_DIR}/${archive_name}" -C "${TMP_DIR}/aegis" \
            || err "Extraction failed."
    fi

    # Install binaries
    info "Installing to ${INSTALL_DIR}/"
    local found=0
    for bin in aegis-daemon aegis-cli aegis-tui; do
        if [ -f "${TMP_DIR}/aegis/${bin}" ]; then
            mv "${TMP_DIR}/aegis/${bin}" "${INSTALL_DIR}/${bin}"
            chmod +x "${INSTALL_DIR}/${bin}"
            found=$((found + 1))
        fi
    done

    if [ "${found}" -eq 0 ]; then
        err "No Aegis binaries found in the archive. The release may have a different structure."
    fi

    ok "Installed ${found} binaries to ${INSTALL_DIR}/"

    # Verify PATH
    if echo "${PATH}" | tr ':' '\n' | grep -qx "${INSTALL_DIR}"; then
        ok "PATH already includes ${INSTALL_DIR}"
    else
        warn "${INSTALL_DIR} is not in your PATH."
        echo ""
        echo "  Add it by appending this to your shell profile (~/.bashrc, ~/.zshrc, etc.):"
        echo ""
        echo -e "    ${CYAN}export PATH=\"\${HOME}/.local/bin:\${PATH}\"${NC}"
        echo ""
        echo "  Then reload your shell:"
        echo ""
        echo -e "    ${CYAN}source ~/.bashrc${NC}"
        echo ""
    fi

    # Done
    echo ""
    echo -e "${GREEN}${BOLD}  ✓ Aegis ${version} installed successfully!${NC}"
    echo ""
    echo "  Get started:"
    echo ""
    echo -e "    ${CYAN}aegis-daemon${NC}              Start the daemon"
    echo -e "    ${CYAN}aegis-cli status${NC}          Check daemon health"
    echo -e "    ${CYAN}aegis-cli init${NC}            Initialize a project"
    echo -e "    ${CYAN}aegis-cli deploy${NC}          Deploy an application"
    echo -e "    ${CYAN}aegis-tui${NC}                 Launch the terminal UI"
    echo ""
    echo "  Documentation: https://shivam411.github.io/Aegis/"
    echo ""
}

main "$@"
