#!/usr/bin/env bash
# ============================================================================
# install_uri_scheme.sh — Register the conquerd:// URI scheme on Linux
# ============================================================================
# Installs the .desktop file and registers it as the handler for
# conquerd:// URLs so clicking invite links in a browser launches ConquerD.
#
# Usage:
#   ./packaging/install_uri_scheme.sh          # current user only
#   sudo ./packaging/install_uri_scheme.sh     # system-wide
# ============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DESKTOP_FILE="$SCRIPT_DIR/conquerd.desktop"

if [ ! -f "$DESKTOP_FILE" ]; then
    echo "ERROR: conquerd.desktop not found at $DESKTOP_FILE"
    exit 1
fi

if [ "$(id -u)" -eq 0 ]; then
    # System-wide install
    DEST="/usr/share/applications"
    echo "Installing conquerd.desktop system-wide to $DEST..."
    cp "$DESKTOP_FILE" "$DEST/conquerd.desktop"
    update-desktop-database "$DEST" 2>/dev/null || true
else
    # Per-user install
    DEST="$HOME/.local/share/applications"
    mkdir -p "$DEST"
    echo "Installing conquerd.desktop for current user to $DEST..."
    cp "$DESKTOP_FILE" "$DEST/conquerd.desktop"
    update-desktop-database "$DEST" 2>/dev/null || true
fi

# Register as default handler for conquerd:// URIs
xdg-mime default conquerd.desktop x-scheme-handler/conquerd 2>/dev/null || true

echo "Done. The conquerd:// URI scheme is now registered."
echo "Test with: xdg-open 'conquerd://test'"
