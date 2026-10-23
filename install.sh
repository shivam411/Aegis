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
#   --no-self-update    (with --server) Don't install the root-owned aegis-update
#                       helper; "Upgrade now" in the dashboard then only shows the
#                       command to run (see docs/upgrading.md)
#   --allow-unsigned    Install a release even if it isn't signed, or if this
#                       installer has no release signing key yet. Only the
#                       SHA-256 checksum is checked then. Not recommended.
#   --print-unit        Print the systemd unit and exit
#
# Downloads are verified: the release's SHA256SUMS must be signed by one of
# the release keys below (minisign/Ed25519; checked with OpenSSL 1.1.1+, or
# minisign if OpenSSL can't), and the archive must match its SHA-256 there.
#

set -euo pipefail

REPO="shivam411/Aegis"
INSTALL_DIR="${HOME}/.local/bin"
SERVER=0
DOMAIN=""
BIN_DIR=""
NO_START=0
SELF_UPDATE=1
AEGIS_USER="aegis"
STATE_DIR="/var/lib/aegis"
CONFIG_DIR="/etc/aegis"
UNIT_PATH="/etc/systemd/system/aegis.service"
UPDATE_PATH_UNIT="/etc/systemd/system/aegis-update.path"
UPDATE_SERVICE_UNIT="/etc/systemd/system/aegis-update.service"
GITHUB_API="https://api.github.com/repos/${REPO}/releases/latest"
ALLOW_UNSIGNED="${AEGIS_ALLOW_UNSIGNED:-0}"

# The minisign public keys a release's SHA256SUMS may be signed with, one
# quoted base64 key per line. The same list as core/update/keys/release.pub
# (compiled into Aegis; a test checks that they match). Rotation: see
# docs/releasing.md.
RELEASE_KEYS=(
    "RWTMV7e9bG4H1Hae4D0bgoQMtG0j0ycitWwS+NTOFmVV+XcIRQKPvjVP"  # key ID D4076E6CBDB757CC
)

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
# Check an archive against the release's SHA256SUMS
# ──────────────────────────────────────────────
verify_checksum() {
    local dir="$1" name="$2" expected actual
    expected="$(awk -v f="${name}" '$2 == f || $2 == "*"f { print $1; exit }' "${dir}/SHA256SUMS")"
    [ -n "${expected}" ] || err "SHA256SUMS has no entry for ${name}."
    if command -v sha256sum &>/dev/null; then
        actual="$(sha256sum "${dir}/${name}" | awk '{ print $1 }')"
    elif command -v shasum &>/dev/null; then
        actual="$(shasum -a 256 "${dir}/${name}" | awk '{ print $1 }')"
    else
        err "Neither sha256sum nor shasum is installed; cannot verify the download."
    fi
    [ "${expected}" = "${actual}" ] \
        || err "Checksum mismatch for ${name} (expected ${expected}, got ${actual}). Not installing."
}

# ──────────────────────────────────────────────
# Check the release's signature (SHA256SUMS.minisig)
# ──────────────────────────────────────────────
# A minisign signature file has four lines: an untrusted comment; base64 of
# "ED" + key ID (8 bytes) + Ed25519 signature of the BLAKE2b-512 hash of the
# file; "trusted comment: ..."; base64 of the Ed25519 signature of the first
# signature followed by the trusted comment. A public key is base64 of
# "Ed" + key ID + the 32-byte Ed25519 key. The release workflow puts
# "aegis-release:<tag>" in the trusted comment.

b64decode() {
    if printf 'QQ==' | base64 -d >/dev/null 2>&1; then
        printf '%s' "$1" | base64 -d > "$2" 2>/dev/null
    else
        printf '%s' "$1" | openssl base64 -d -A > "$2" 2>/dev/null
    fi
}

byte_count() { wc -c < "$1" | tr -d ' '; }

hex_of() { od -An -v -tx1 | tr -d ' \n'; }

# minisign prints key IDs as a little-endian number in upper-case hex.
key_id_display() {
    local hex="$1" out="" i
    for ((i = ${#hex} - 2; i >= 0; i -= 2)); do out="${out}${hex:i:2}"; done
    echo "${out}" | tr 'a-f' 'A-F'
}

# The trusted comment must name the release being installed.
comment_names_release() {
    local comment="$1" tag="$2" word words
    read -ra words <<< "${comment}"
    for word in "${words[@]}"; do
        [ "${word}" = "aegis-release:${tag}" ] && return 0
    done
    return 1
}

# OpenSSL 1.1.1+ can check Ed25519 signatures over a BLAKE2b-512 hash.
# (LibreSSL, e.g. macOS's openssl, can't; minisign is used there.)
openssl_can_verify() {
    command -v openssl &>/dev/null || return 1
    openssl dgst -blake2b512 -binary </dev/null >/dev/null 2>&1 || return 1
    openssl pkeyutl -help 2>&1 | grep -q -- '-rawin'
}

# verify_with_openssl DIR TAG: prints the signing key's ID, or explains on
# stderr and fails.
verify_with_openssl() {
    local dir="$1" tag="$2" work="$1/.signature" line2 comment line4 sig_id key
    mkdir -p "${work}"
    line2="$(sed -n 2p "${dir}/SHA256SUMS.minisig" | tr -d '\r')"
    comment="$(sed -n 3p "${dir}/SHA256SUMS.minisig" | tr -d '\r')"
    line4="$(sed -n 4p "${dir}/SHA256SUMS.minisig" | tr -d '\r')"
    case "${comment}" in
        "trusted comment: "*) comment="${comment#trusted comment: }" ;;
        *) echo "SHA256SUMS.minisig is not a minisign signature." >&2; return 1 ;;
    esac
    if ! b64decode "${line2}" "${work}/sig.bin" || [ "$(byte_count "${work}/sig.bin")" != 74 ] \
        || ! b64decode "${line4}" "${work}/global.bin" || [ "$(byte_count "${work}/global.bin")" != 64 ]; then
        echo "SHA256SUMS.minisig is not a minisign signature." >&2
        return 1
    fi
    if [ "$(head -c 2 "${work}/sig.bin")" != "ED" ]; then
        echo "SHA256SUMS.minisig is not a pre-hashed (ED) minisign signature." >&2
        return 1
    fi
    sig_id="$(head -c 10 "${work}/sig.bin" | tail -c 8 | hex_of)"
    tail -c 64 "${work}/sig.bin" > "${work}/sig64.bin"
    openssl dgst -blake2b512 -binary "${dir}/SHA256SUMS" > "${work}/hash.bin"
    { cat "${work}/sig64.bin"; printf '%s' "${comment}"; } > "${work}/global-message.bin"
    for key in "${RELEASE_KEYS[@]}"; do
        b64decode "${key}" "${work}/key.bin" || continue
        [ "$(byte_count "${work}/key.bin")" = 42 ] || continue
        [ "$(head -c 10 "${work}/key.bin" | tail -c 8 | hex_of)" = "${sig_id}" ] || continue
        # DER SubjectPublicKeyInfo for an Ed25519 key, then the key.
        { printf '\x30\x2a\x30\x05\x06\x03\x2b\x65\x70\x03\x21\x00'; tail -c 32 "${work}/key.bin"; } > "${work}/key.der"
        if openssl pkeyutl -verify -pubin -keyform DER -inkey "${work}/key.der" -rawin \
                -in "${work}/hash.bin" -sigfile "${work}/sig64.bin" >/dev/null 2>&1 \
            && openssl pkeyutl -verify -pubin -keyform DER -inkey "${work}/key.der" -rawin \
                -in "${work}/global-message.bin" -sigfile "${work}/global.bin" >/dev/null 2>&1; then
            if ! comment_names_release "${comment}" "${tag}"; then
                echo "The signature was made for another release ('${comment}', expected aegis-release:${tag})." >&2
                return 1
            fi
            key_id_display "${sig_id}"
            return 0
        fi
        echo "The signature on SHA256SUMS (key $(key_id_display "${sig_id}")) is not valid: the file was modified or corrupted." >&2
        return 1
    done
    echo "SHA256SUMS is signed by key $(key_id_display "${sig_id}"), which this installer does not trust." >&2
    return 1
}

# verify_with_minisign DIR TAG: the same check with the minisign tool.
verify_with_minisign() {
    local dir="$1" tag="$2" key comment
    for key in "${RELEASE_KEYS[@]}"; do
        if comment="$(minisign -V -Q -P "${key}" -m "${dir}/SHA256SUMS" -x "${dir}/SHA256SUMS.minisig" 2>/dev/null)"; then
            if ! comment_names_release "${comment}" "${tag}"; then
                echo "The signature was made for another release ('${comment}', expected aegis-release:${tag})." >&2
                return 1
            fi
            if b64decode "${key}" "${dir}/.key.bin"; then
                key_id_display "$(head -c 10 "${dir}/.key.bin" | tail -c 8 | hex_of)"
            else
                echo "${key}"
            fi
            return 0
        fi
    done
    echo "The signature on SHA256SUMS is not valid, or not made by a key this installer trusts." >&2
    return 1
}

# verify_release_signature DIR TAG: checks DIR/SHA256SUMS against
# DIR/SHA256SUMS.minisig (absent if the release has none). Exits on failure.
verify_release_signature() {
    local dir="$1" tag="$2" why="" key_id
    if [ "${#RELEASE_KEYS[@]}" -eq 0 ]; then
        why="This installer has no release signing key yet (see docs/releasing.md)"
    elif [ ! -s "${dir}/SHA256SUMS.minisig" ]; then
        why="Release ${tag} is not signed (it has no SHA256SUMS.minisig)"
    fi
    if [ -n "${why}" ]; then
        if [ "${ALLOW_UNSIGNED}" = 1 ]; then
            warn "${why}."
            warn "Continuing because of --allow-unsigned: only the SHA-256 checksum is checked, and anyone able to publish a release could have replaced both."
            return 0
        fi
        err "${why}, so the download can't be verified. Re-run with --allow-unsigned to install it anyway (SHA-256 only), or build Aegis from source."
    fi
    if openssl_can_verify; then
        key_id="$(verify_with_openssl "${dir}" "${tag}")" \
            || err "The release signature check failed (see above); not installing."
    elif command -v minisign &>/dev/null; then
        key_id="$(verify_with_minisign "${dir}" "${tag}")" \
            || err "The release signature check failed (see above); not installing."
    else
        err "Can't check the release signature: this needs OpenSSL 1.1.1 or newer, or minisign (e.g. 'brew install minisign'). Install one, or re-run with --allow-unsigned (SHA-256 only)."
    fi
    ok "SHA256SUMS is signed by release key ${key_id}"
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
# Exit status 75: a self-update installed a new binary; start it.
RestartForceExitStatus=75

# On SIGTERM Aegis stops its apps itself (they come back on the next start),
# so signal only the daemon and give it time before anything is killed.
KillMode=mixed
TimeoutStopSec=60

# Apps run inside this service's cgroup; delegation lets Aegis create a
# cgroup per app and change its CPU/memory limits live. The daemon moves
# itself into <service>/daemon and puts apps under <service>/apps.
Delegate=yes

# The built-in proxy ([proxy] in aegis.toml) listens on ports 80 and 443.
# This grants only that capability; apps inherit it but nothing else.
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE

# Hardening. Apps inherit these restrictions.
NoNewPrivileges=yes
ProtectSystem=strict
ReadWritePaths=${STATE_DIR}
# ProtectKernelTunables makes /sys read-only; the delegated cgroup must stay
# writable (systemd owns everything else there, so file permissions still
# confine the aegis user to its own subtree).
ReadWritePaths=/sys/fs/cgroup
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

# The self-update helper. The daemon (user aegis) can't replace the root-owned
# binaries in /usr/local/bin. When an admin clicks "Upgrade now" it writes
# ${STATE_DIR}/update/request.json naming only a version; this path unit then
# starts a root one-shot service that downloads that release itself, checks
# its signature and SHA256SUMS, refuses anything not newer than the
# installed version, swaps the binaries (keeping *.previous) and restarts aegis.
print_update_path_unit() {
    cat <<UNIT
[Unit]
Description=Watch for Aegis update requests from the dashboard
Documentation=https://github.com/${REPO}/blob/main/docs/upgrading.md

[Path]
PathExists=${STATE_DIR}/update/request.json
Unit=aegis-update.service

[Install]
WantedBy=multi-user.target
UNIT
}

print_update_service_unit() {
    cat <<UNIT
[Unit]
Description=Install an Aegis update requested from the dashboard
Documentation=https://github.com/${REPO}/blob/main/docs/upgrading.md
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
Environment=AEGIS_CONFIG=${CONFIG_DIR}/aegis.toml
ExecStart=/usr/local/bin/aegis-daemon self-update --from-request --restart-unit aegis.service
TimeoutStartSec=15min
# Root, but it may only write the binaries and the request directory.
ProtectSystem=strict
ReadWritePaths=/usr/local/bin -${STATE_DIR}/update
ProtectHome=yes
PrivateTmp=yes
PrivateDevices=yes
NoNewPrivileges=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictSUIDSGID=yes
LockPersonality=yes
UMask=0022
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

[update]
check = true                # ask GitHub for new releases every 12 hours; false = never

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
        curl -fsSL -o "${TMP_DIR}/SHA256SUMS" \
            "https://github.com/${REPO}/releases/download/${version}/SHA256SUMS" \
            || err "Release ${version} has no SHA256SUMS; refusing to install unverified binaries."
        # Absent for unsigned releases; verify_release_signature decides.
        curl -fsSL -o "${TMP_DIR}/SHA256SUMS.minisig" \
            "https://github.com/${REPO}/releases/download/${version}/SHA256SUMS.minisig" 2>/dev/null \
            || rm -f "${TMP_DIR}/SHA256SUMS.minisig"
        verify_release_signature "${TMP_DIR}" "${version}"
        verify_checksum "${TMP_DIR}" "${archive_name}"
        ok "Downloaded ${archive_name} (SHA-256 verified)"

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
    link_cli_alias "${target}"
}

# The CLI binary is aegis-cli; the docs call it `aegis`. A relative link
# keeps working when an upgrade swaps aegis-cli in place (by rename).
# Something else already named `aegis` is left alone.
link_cli_alias() {
    local target="$1" link current
    link="${target}/aegis"
    [ -f "${target}/aegis-cli" ] || return 0
    if [ -L "${link}" ]; then
        current="$(readlink "${link}")"
        case "${current}" in
            aegis-cli|*/aegis-cli) ;;
            *) warn "${link} points to ${current}, not aegis-cli; leaving it. Run aegis-cli instead."; return 0 ;;
        esac
    elif [ -e "${link}" ]; then
        warn "${link} already exists and is not Aegis; leaving it. Run aegis-cli instead of aegis."
        return 0
    fi
    ln -sfn aegis-cli "${link}"
    ok "Linked ${link} -> aegis-cli (run the CLI as aegis or aegis-cli)"
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

    if [ "${SELF_UPDATE}" -eq 1 ]; then
        print_update_path_unit > "${UPDATE_PATH_UNIT}"
        print_update_service_unit > "${UPDATE_SERVICE_UNIT}"
        chmod 0644 "${UPDATE_PATH_UNIT}" "${UPDATE_SERVICE_UNIT}"
        ok "Wrote ${UPDATE_PATH_UNIT} and ${UPDATE_SERVICE_UNIT} (upgrades from the dashboard)"
    else
        if command -v systemctl &>/dev/null; then
            systemctl disable --now aegis-update.path 2>/dev/null || true
        fi
        rm -f "${UPDATE_PATH_UNIT}" "${UPDATE_SERVICE_UNIT}"
        warn "No self-update helper (--no-self-update). Upgrade with: sudo aegis-daemon self-update"
    fi

    if [ "${NO_START}" -eq 1 ]; then
        warn "Not starting the service (--no-start). Start it with: systemctl enable --now aegis"
        return
    fi
    systemctl daemon-reload
    if [ "${SELF_UPDATE}" -eq 1 ]; then
        systemctl enable --now aegis-update.path
    fi
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
            --no-self-update) SELF_UPDATE=0 ;;
            --allow-unsigned) ALLOW_UNSIGNED=1 ;;
            --print-unit) print_unit; exit 0 ;;
            -h|--help) sed -n '2,31p' "$0" 2>/dev/null || true; exit 0 ;;
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
    echo -e "    ${CYAN}aegis status${NC}              Check daemon health"
    echo -e "    ${CYAN}aegis init${NC}                Initialize a project"
    echo -e "    ${CYAN}aegis deploy${NC}              Deploy an application"
    echo ""
    echo "  'aegis' is a link to the aegis-cli binary; both names work."
    echo "  On a server, install as a system service instead: install.sh --server"
    echo "  Documentation: https://shivam411.github.io/Aegis/"
    echo ""
}

# AEGIS_INSTALL_SH_LIB=1: only define the functions (tests source this file).
if [ "${AEGIS_INSTALL_SH_LIB:-0}" != 1 ]; then
    main "$@"
fi
