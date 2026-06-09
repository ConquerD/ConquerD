#!/usr/bin/env bash
# ============================================================================
# build_linux.sh — Build ConquerD for Linux (Rust + Qt, AppImage)
# ============================================================================
# Produces:
#   dist/ConquerD-X.X.X-x86_64.AppImage
#   dist/ConquerD-X.X.X-x86_64.AppImage.sha256
#
# Prerequisites:
#   1. Rust toolchain (cargo) on PATH.
#   2. Qt 6 installed; set QT_DIR or CMAKE_PREFIX_PATH.
#      e.g. export QT_DIR=/opt/Qt/6.8.3/gcc_64
#   3. linuxdeployqt or appimagetool on PATH.
#      linuxdeployqt: https://github.com/probonopd/linuxdeployqt/releases
#      appimagetool:  https://github.com/AppImage/appimagetool/releases
#   4. cmake on PATH.
#
# System packages (Debian/Ubuntu):
#   sudo apt install build-essential cmake libgl-dev libxkbcommon-dev \
#                    libdbus-1-dev libpulse-dev libminiupnpc-dev fuse
#
# Usage:
#   ./build_linux.sh             # debug build
#   CONQUERD_RELEASE=1 ./build_linux.sh  # release build (optimised)
# ============================================================================

set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
RUST_DIR="$ROOT/rust"
CLIENT_DIR="$RUST_DIR/conquerd-client"

# ── Auto-detect Qt 6 ─────────────────────────────────────────────────────────
if [ -z "${QT_DIR:-}" ]; then
    for CANDIDATE in \
        /opt/Qt/6.8.3/gcc_64 \
        /opt/Qt/6.8.*/gcc_64 \
        "$HOME/Qt/6.8.3/gcc_64" \
        "$HOME/Qt/6.8.*/gcc_64" \
        /usr/local/Qt-6.8.3; do
        if [ -f "$CANDIDATE/bin/qmake" ]; then
            QT_DIR="$CANDIDATE"
            break
        fi
    done
fi
if [ -z "${QT_DIR:-}" ]; then
    echo "ERROR: Qt 6 not found. Set QT_DIR to the Qt installation root."
    exit 1
fi
echo "==> Using Qt: $QT_DIR"
export PATH="$QT_DIR/bin:$PATH"
export CMAKE_PREFIX_PATH="$QT_DIR"

# ── Read version from Cargo.toml ─────────────────────────────────────────────
VERSION=$(grep -m1 '^version' "$RUST_DIR/conquerd-client/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
echo "==> Building ConquerD v${VERSION} for Linux"

PROFILE="debug"
CARGO_FLAGS=""
if [ "${CONQUERD_RELEASE:-0}" = "1" ]; then
    PROFILE="release"
    CARGO_FLAGS="--release"
fi

# ── Build ─────────────────────────────────────────────────────────────────────
# conquerd-client is its own workspace root (see rust/conquerd-client/Cargo.toml).
echo ""
echo "==> cargo build --features qt-ui $CARGO_FLAGS  (conquerd-client workspace)"
cd "$CLIENT_DIR"
cargo build --features qt-ui $CARGO_FLAGS

echo ""
echo "==> cargo build -p conquerd-installer $CARGO_FLAGS"
cd "$RUST_DIR"
cargo build -p conquerd-installer $CARGO_FLAGS

BINARY="$RUST_DIR/target/$PROFILE/conquerd-client"
INSTALLER_BIN="$RUST_DIR/target/$PROFILE/conquerd-installer"

# ── Assemble AppDir ───────────────────────────────────────────────────────────
DIST="$ROOT/dist"
APPDIR="$DIST/ConquerD.AppDir"
echo ""
echo "==> Assembling AppDir at $APPDIR..."

rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin"
mkdir -p "$APPDIR/usr/share/applications"
mkdir -p "$APPDIR/usr/share/icons/hicolor/256x256/apps"

cp "$BINARY" "$APPDIR/usr/bin/conquerd"
cp "$INSTALLER_BIN" "$APPDIR/usr/bin/conquerd-installer"
chmod +x "$APPDIR/usr/bin/conquerd" "$APPDIR/usr/bin/conquerd-installer"

# Desktop integration
cp "$ROOT/packaging/AppRun" "$APPDIR/AppRun"
chmod +x "$APPDIR/AppRun"
cp "$ROOT/packaging/conquerd.desktop" "$APPDIR/usr/share/applications/conquerd.desktop"
ln -sf usr/share/applications/conquerd.desktop "$APPDIR/conquerd.desktop"

# Icon (PNG)
if [ -f "$ROOT/assets/conquerd_256.png" ]; then
    cp "$ROOT/assets/conquerd_256.png" "$APPDIR/conquerd.png"
    cp "$ROOT/assets/conquerd_256.png" "$APPDIR/usr/share/icons/hicolor/256x256/apps/conquerd.png"
elif [ -f "$ROOT/assets/conquerd.ico" ]; then
    # Fallback: convert ICO to PNG using ImageMagick if available
    if command -v convert &>/dev/null; then
        convert "$ROOT/assets/conquerd.ico[0]" "$APPDIR/conquerd.png"
        cp "$APPDIR/conquerd.png" "$APPDIR/usr/share/icons/hicolor/256x256/apps/conquerd.png"
    fi
fi

# Qt libraries
echo ""
echo "==> Running linuxdeployqt..."
LDQT="${LINUXDEPLOYQT:-$(command -v linuxdeployqt 2>/dev/null || true)}"
if [ -n "$LDQT" ] && [ -f "$LDQT" ]; then
    "$LDQT" "$APPDIR/usr/bin/conquerd" -appimage \
        -qmldir="$RUST_DIR/conquerd-client/qml" \
        -no-translations
else
    # linuxdeployqt not found — copy Qt libs manually + use windeployqt-style copy
    echo "  WARNING: linuxdeployqt not found; Qt libraries not bundled automatically."
    echo "  Install from: https://github.com/probonopd/linuxdeployqt/releases"
fi

# ── Package as AppImage ───────────────────────────────────────────────────────
APPIMAGETOOL="${APPIMAGETOOL:-$(command -v appimagetool 2>/dev/null || true)}"
if [ -z "$APPIMAGETOOL" ]; then
    echo ""
    echo "WARNING: appimagetool not found; skipping AppImage packaging."
    echo "Install from: https://github.com/AppImage/appimagetool/releases"
else
    ARCH="x86_64"
    APPIMAGE="$DIST/ConquerD-${VERSION}-${ARCH}.AppImage"
    echo ""
    echo "==> Creating AppImage: $APPIMAGE"
    ARCH="$ARCH" "$APPIMAGETOOL" "$APPDIR" "$APPIMAGE"
    sha256sum "$APPIMAGE" | tee "${APPIMAGE}.sha256"
    echo "==> Done: $APPIMAGE"
fi
