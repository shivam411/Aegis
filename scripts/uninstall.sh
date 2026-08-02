#!/usr/bin/env bash
#
# Aegis Uninstaller
# https://github.com/shivam411/Aegis
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/shivam411/Aegis/main/scripts/uninstall.sh | bash
#

set -euo pipefail

INSTALL_DIR="${HOME}/.local/bin"
CONFIG_DIR="${HOME}/.aegis"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

info()  { echo -e "${CYAN}${BOLD}[info]${NC}  $*"; }
ok()    { echo -e "${GREEN}${BOLD}[  ok]${NC}  $*"; }
warn()  { echo -e "${YELLOW}${BOLD}[warn]${NC}  $*"; }

echo ""
echo -e "${BOLD}  Aegis Uninstaller${NC}"
echo ""

# Remove binaries
removed=0
for bin in aegis-daemon aegis-cli aegis-tui; do
    if [ -f "${INSTALL_DIR}/${bin}" ]; then
        rm -f "${INSTALL_DIR}/${bin}"
        info "Removed ${INSTALL_DIR}/${bin}"
        removed=$((removed + 1))
    fi
done

if [ "${removed}" -eq 0 ]; then
    warn "No Aegis binaries found in ${INSTALL_DIR}/."
else
    ok "Removed ${removed} binaries."
fi

# Prompt before removing config
if [ -d "${CONFIG_DIR}" ]; then
    echo ""
    echo -e "  Aegis configuration directory exists at: ${CYAN}${CONFIG_DIR}${NC}"
    echo ""
    read -rp "  Remove configuration and data? [y/N] " choice
    case "${choice}" in
        [yY]|[yY][eE][sS])
            rm -rf "${CONFIG_DIR}"
            ok "Removed ${CONFIG_DIR}"
            ;;
        *)
            info "Kept ${CONFIG_DIR}"
            ;;
    esac
fi

echo ""
echo -e "${GREEN}${BOLD}  ✓ Aegis uninstalled.${NC}"
echo ""
