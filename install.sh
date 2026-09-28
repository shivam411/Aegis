#!/usr/bin/env bash
#
# Aegis Installer
# https://github.com/shivam411/Aegis
#
# Usage:
#   curl -fsSL https://shivam411.github.io/Aegis/install.sh | bash
#       Install the binaries for the current user (~/.local/bin).
#
#   curl -fsSL https://shivam411.github.io/Aegis/install.sh | sudo bash -s -- --server [--domain aegis.example.com]
#       Install Aegis as a system service on a Linux server (systemd):
#       binaries in /usr/local/bin, a dedicated 'aegis' user, config in
#       /etc/aegis/aegis.toml, state in /var/lib/aegis, and a hardened unit.
#
# Options:
#   --server            System-service install (requires root and systemd)
#   --domain NAME       Public hostname for the web API behind a reverse proxy (with --server)
#   --bin-dir DIR       Install binaries from DIR instead of downloading a release
#   --no-start          Write everything but don't enable/start the service
#   --print-unit        Print the systemd unit and exit
#

set -euo pipefail

REPO="shivam411/Aegis"
INSTALL_DIR="${HOME}/.local/bin"
SERVER=0
DOMAIN=""
BIN_DIR=""
NO_START=0
AEGIS_USER="aegis"
STATE_DIR="/var/lib/aegis"
CONFIG_DIR="/etc/aegis"
UNIT_PATH="/etc/systemd/system/aegis.service"
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
# systemd unit for --server
# ──────────────────────────────────────────────
print_unit() {
    cat <<UNIT
[Unit]
Description=Aegis deployment daemon
Documentation=https://github.com/${REPO}
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${AEGIS_USER}
Group=${AEGIS_USER}
Environment=AEGIS_CONFIG=${CONFIG_DIR}/aegis.toml
Environment=HOME=${STATE_DIR}
WorkingDirectory=${STATE_DIR}
ExecStartPre=/usr/local/bin/aegis-daemon check-config
ExecStart=/usr/local/bin/aegis-daemon run
Restart=on-failure
RestartSec=5

# On SIGTERM Aegis stops its apps itself (they come back on the next start),
# so signal only the daemon and give it time before anything is killed.
KillMode=mixed
TimeoutStopSec=60

# Apps run inside this service's cgroup; delegation lets Aegis manage
# per-app CPU/memory limits (roadmap Phase 4).
Delegate=yes

# Hardening. Apps inherit these restrictions.
NoNewPrivileges=yes
ProtectSystem=strict
ReadWritePaths=${STATE_DIR}
# Read-only (not hidden) so deployments can copy projects from /home.
ProtectHome=read-only
PrivateTmp=yes
PrivateDevices=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectKernelLogs=yes
ProtectClock=yes
ProtectHostname=yes
RestrictSUIDSGID=yes
RestrictRealtime=yes
RestrictNamespaces=yes
LockPersonality=yes
SystemCallArchitectures=native
UMask=0027
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
UNIT
}

default_server_config() {
    cat <<CONFIG
# Aegis daemon configuration. See docs/configuration.md.
[daemon]
host = "127.0.0.1"          # gRPC for the local CLI; must stay on loopback
port = 50051
database_path = "${STATE_DIR}/aegis.db"
data_dir = "${STATE_DIR}"
log_level = "info"

[web]
enabled = true
host = "127.0.0.1"          # public access goes through TLS or a reverse proxy
port = 8420
CONFIG
    if [ -n "${DOMAIN}" ]; then
        cat <<CONFIG
behind_proxy = true         # e.g. Caddy terminating HTTPS for ${DOMAIN}
allowed_hosts = ["${DOMAIN}"]
CONFIG
    fi
}

# ──────────────────────────────────────────────
# Binaries
# ──────────────────────────────────────────────
install_binaries() {
    local target="$1" os arch version archive_name download_url src
    mkdir -p "${target}"

    if [ -n "${BIN_DIR}" ]; then
        src="${BIN_DIR}"
        version="(local build)"
    else
        check_deps
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

        info "Downloading ${archive_name}..."
        TMP_DIR="$(mktemp -d)"
        trap 'rm -rf "${TMP_DIR:-}"' EXIT
        curl -fsSL -o "${TMP_DIR}/${archive_name}" "${download_url}" \
            || err "Download failed. Check that release ${version} has asset ${archive_name}."
        ok "Downloaded ${archive_name}"

        info "Extracting..."
        if [ "${os}" = "windows" ]; then
            unzip -qo "${TMP_DIR}/${archive_name}" -d "${TMP_DIR}/aegis" 2>/dev/null \
                || err "Extraction failed. Is 'unzip' installed?"
        else
            mkdir -p "${TMP_DIR}/aegis"
            tar -xzf "${TMP_DIR}/${archive_name}" -C "${TMP_DIR}/aegis" \
                || err "Extraction failed."
        fi
        src="${TMP_DIR}/aegis"
    fi

    info "Installing to ${target}/"
    local found=0
    for bin in aegis-daemon aegis-cli aegis-tui; do
        if [ -f "${src}/${bin}" ]; then
            install -m 0755 "${src}/${bin}" "${target}/${bin}"
            found=$((found + 1))
        fi
    done
    if [ "${found}" -eq 0 ]; then
        err "No Aegis binaries found in ${src}."
    fi
    ok "Installed ${found} binaries ${version} to ${target}/"
}

# ──────────────────────────────────────────────
# --server
# ──────────────────────────────────────────────
server_install() {
    [ "$(id -u)" -eq 0 ] || err "--server must run as root (use sudo)."
    [ "$(uname -s)" = "Linux" ] || err "--server supports Linux with systemd only."
    if [ "${NO_START}" -eq 0 ] && ! command -v systemctl &>/dev/null; then
        err "systemctl not found; --server needs systemd (or use --no-start)."
    fi

    install_binaries /usr/local/bin

    if ! id -u "${AEGIS_USER}" &>/dev/null; then
        useradd --system --home-dir "${STATE_DIR}" --shell /usr/sbin/nologin "${AEGIS_USER}"
        ok "Created system user '${AEGIS_USER}'"
    fi
    install -d -m 0750 -o "${AEGIS_USER}" -g "${AEGIS_USER}" "${STATE_DIR}"
    install -d -m 0750 -o root -g "${AEGIS_USER}" "${CONFIG_DIR}"

    if [ -f "${CONFIG_DIR}/aegis.toml" ]; then
        warn "Keeping existing ${CONFIG_DIR}/aegis.toml"
    else
        default_server_config > "${CONFIG_DIR}/aegis.toml"
        chown root:"${AEGIS_USER}" "${CONFIG_DIR}/aegis.toml"
        chmod 0640 "${CONFIG_DIR}/aegis.toml"
        ok "Wrote ${CONFIG_DIR}/aegis.toml"
    fi
    AEGIS_CONFIG="${CONFIG_DIR}/aegis.toml" /usr/local/bin/aegis-daemon check-config \
        || err "The configuration in ${CONFIG_DIR}/aegis.toml is not valid (see above)."

    print_unit > "${UNIT_PATH}"
    chmod 0644 "${UNIT_PATH}"
    ok "Wrote ${UNIT_PATH}"

    if [ "${NO_START}" -eq 1 ]; then
        warn "Not starting the service (--no-start). Start it with: systemctl enable --now aegis"
        return
    fi
    systemctl daemon-reload
    systemctl enable --now aegis
    ok "Service 'aegis' enabled and started"

    local pw_file="${STATE_DIR}/initial-admin-password"
    for _ in $(seq 1 50); do
        [ -f "${pw_file}" ] && break
        sleep 0.2
    done

    echo ""
    echo -e "${GREEN}${BOLD}  ✓ Aegis is running as a system service.${NC}"
    echo ""
    if [ -f "${pw_file}" ]; then
        echo "  Admin login:     admin / $(cat "${pw_file}")"
        echo "                   (also in ${pw_file}; removed once you change the password)"
    else
        echo "  Admin account:   already set up (reset with: sudo -u ${AEGIS_USER} AEGIS_CONFIG=${CONFIG_DIR}/aegis.toml aegis-daemon admin reset-password)"
    fi
    echo "  Status & logs:   systemctl status aegis   ·   journalctl -u aegis -f"
    echo ""
    if [ -n "${DOMAIN}" ]; then
        echo "  The API listens on 127.0.0.1:8420 for a reverse proxy. With Caddy (automatic HTTPS):"
        echo ""
        echo -e "    ${CYAN}${DOMAIN} {${NC}"
        echo -e "    ${CYAN}    reverse_proxy 127.0.0.1:8420${NC}"
        echo -e "    ${CYAN}}${NC}"
        echo ""
        echo "  Then allow HTTP/HTTPS through your firewall, e.g.: ufw allow 80,443/tcp"
        echo "  Aegis doesn't change firewall rules itself."
    else
        echo "  The API listens on 127.0.0.1:8420 only. From your computer:"
        echo ""
        echo -e "    ${CYAN}ssh -N -L 8420:127.0.0.1:8420 you@this-server${NC}"
        echo -e "    ${CYAN}curl http://localhost:8420/healthz${NC}"
        echo ""
        echo "  For public HTTPS access, re-run with --domain your.domain (reverse proxy)"
        echo "  or set [web.tls] in ${CONFIG_DIR}/aegis.toml. See docs/security.md."
    fi
    echo ""
}

# ──────────────────────────────────────────────
# Main
# ──────────────────────────────────────────────
parse_args() {
    while [ $# -gt 0 ]; do
        case "$1" in
            --server) SERVER=1 ;;
            --domain) shift; DOMAIN="${1:-}"; [ -n "${DOMAIN}" ] || err "--domain needs a hostname" ;;
            --bin-dir) shift; BIN_DIR="${1:-}"; [ -d "${BIN_DIR}" ] || err "--bin-dir needs a directory" ;;
            --no-start) NO_START=1 ;;
            --print-unit) print_unit; exit 0 ;;
            -h|--help) sed -n '2,24p' "$0" 2>/dev/null || true; exit 0 ;;
            *) err "Unknown option: $1" ;;
        esac
        shift
    done
    if [ -n "${DOMAIN}" ] && [ "${SERVER}" -eq 0 ]; then
        err "--domain only applies with --server"
    fi
    # [[ =~ ]] matches the whole value; grep would accept one good line of many.
    if [ -n "${DOMAIN}" ] && ! [[ "${DOMAIN}" =~ ^[A-Za-z0-9.-]+$ ]]; then
        err "--domain must be a plain hostname"
    fi
}

main() {
    parse_args "$@"

    echo ""
    echo -e "${BOLD}  ╔══════════════════════════════════════╗${NC}"
    echo -e "${BOLD}  ║        ${CYAN}Aegis Installer${NC}${BOLD}               ║${NC}"
    echo -e "${BOLD}  ║   Self-hosted deployment platform    ║${NC}"
    echo -e "${BOLD}  ╚══════════════════════════════════════╝${NC}"
    echo ""

    if [ "${SERVER}" -eq 1 ]; then
        server_install
        return
    fi

    install_binaries "${INSTALL_DIR}"

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
    echo -e "${GREEN}${BOLD}  ✓ Aegis installed successfully!${NC}"
    echo ""
    echo "  Get started:"
    echo ""
    echo -e "    ${CYAN}aegis-daemon${NC}              Start the daemon"
    echo -e "    ${CYAN}aegis-cli status${NC}          Check daemon health"
    echo -e "    ${CYAN}aegis-cli init${NC}            Initialize a project"
    echo -e "    ${CYAN}aegis-cli deploy${NC}          Deploy an application"
    echo ""
    echo "  On a server, install as a system service instead: install.sh --server"
    echo "  Documentation: https://shivam411.github.io/Aegis/"
    echo ""
}

main "$@"
