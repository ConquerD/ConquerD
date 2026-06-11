#!/usr/bin/env bash
# ============================================================================
# build_supernode.sh — Build and package conquerd-supernode for the host platform
# ============================================================================
# Produces:
#   dist/conquerd-supernode-X.X.X-<platform>.tar.gz
#   dist/conquerd-supernode-X.X.X-<platform>.tar.gz.sha256
#
# Supported platform suffixes:
#   linux-x86_64, linux-aarch64, macos-arm64, macos-x86_64
#
# Prerequisites:
#   Rust toolchain (cargo) on PATH.
#
# Usage:
#   ./scripts/build_supernode.sh
#   CONQUERD_RELEASE=1 CONQUERD_BUILD_ID=release-1.0.0-abc123 ./scripts/build_supernode.sh
# ============================================================================

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RUST_DIR="$ROOT/rust"
DIST="$ROOT/dist"

VERSION="$(
    grep -m1 '^version' "$RUST_DIR/conquerd-supernode/Cargo.toml" \
        | sed 's/.*"\(.*\)".*/\1/'
)"

ARCH="$(uname -m)"
OS="$(uname -s)"
case "$OS-$ARCH" in
    Linux-x86_64) PLATFORM="linux-x86_64" ;;
    Linux-aarch64|Linux-arm64) PLATFORM="linux-aarch64" ;;
    Darwin-arm64) PLATFORM="macos-arm64" ;;
    Darwin-x86_64) PLATFORM="macos-x86_64" ;;
    *)
        echo "ERROR: unsupported platform $OS-$ARCH"
        exit 1
        ;;
esac

PROFILE="debug"
CARGO_FLAGS=""
if [ "${CONQUERD_RELEASE:-0}" = "1" ] || [ "${CONQUERD_DEBUG:-0}" != "1" ]; then
    PROFILE="release"
    CARGO_FLAGS="--release"
fi

echo "==> Building conquerd-supernode v${VERSION} for ${PLATFORM} (profile: ${PROFILE})"

cd "$RUST_DIR"
cargo build -p conquerd-supernode $CARGO_FLAGS

BINARY="$RUST_DIR/target/$PROFILE/conquerd-supernode"
if [ ! -f "$BINARY" ]; then
    echo "ERROR: expected binary at $BINARY"
    exit 1
fi

STAGING_NAME="conquerd-supernode-${VERSION}-${PLATFORM}"
STAGING="$DIST/$STAGING_NAME"
ARCHIVE="$DIST/${STAGING_NAME}.tar.gz"

mkdir -p "$DIST"
rm -rf "$STAGING"
mkdir -p "$STAGING"
cp "$BINARY" "$STAGING/conquerd-supernode"
chmod +x "$STAGING/conquerd-supernode"

tar -czf "$ARCHIVE" -C "$DIST" "$STAGING_NAME"
if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$ARCHIVE" > "${ARCHIVE}.sha256"
elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$ARCHIVE" > "${ARCHIVE}.sha256"
else
    echo "WARNING: no sha256sum/shasum found; skipping checksum"
fi
rm -rf "$STAGING"

echo "==> Package ready: $ARCHIVE"
if [ -f "${ARCHIVE}.sha256" ]; then
    cat "${ARCHIVE}.sha256"
fi