#!/usr/bin/env bash
# fetch_opus_weights.sh
# Downloads and extracts the Opus DNN model data files required for the
# conquerd-opus `dnn` feature (DRED + OSCE neural features).
#
# Background
# ----------
# The Xiph.Org Foundation distributes the DNN model weights as C source arrays
# in a tarball on the Xiph media server.  The tarball filename *is* its own
# SHA-256 hash, so the download is self-verifying.  The C files must be
# present at `rust/conquerd-opus/opus/dnn/` before cmake builds libopus.
#
# What this script does:
#   1. Skips extraction if the sentinel `lace_data.c` already exists (idempotent).
#   2. Downloads the tarball and verifies its SHA-256.
#   3. Extracts the C data files into `rust/conquerd-opus/opus/` so cmake
#      compiles them into libopus as static C arrays.
#
# Usage (from the repository root):
#   bash scripts/fetch_opus_weights.sh

set -euo pipefail

# ── Configuration ─────────────────────────────────────────────────────────────
# These values correspond to the DNN data package for the libopus commit
# tracked by the conquerd-opus submodule.
# The hash is the SHA-256 of the tarball itself (it is embedded in the URL).
DNN_HASH="a5177ec6fb7d15058e99e57029746100121f68e4890b1467d4094aa336b6013e"
DNN_URL="https://media.xiph.org/opus/models/opus_data-${DNN_HASH}.tar.gz"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OPUS_SRC="$SCRIPT_DIR/../rust/conquerd-opus/opus"
SENTINEL="$OPUS_SRC/dnn/lace_data.c"

# ── Helpers ───────────────────────────────────────────────────────────────────
verify_sha256() {
    local file="$1"
    local expected="$2"
    local actual
    if command -v sha256sum &>/dev/null; then
        actual=$(sha256sum "$file" | awk '{print $1}')
    else
        actual=$(shasum -a 256 "$file" | awk '{print $1}')
    fi
    if [[ "$actual" != "$expected" ]]; then
        echo "  ERROR: SHA-256 mismatch for '$file'"
        echo "    expected: $expected"
        echo "    got:      $actual"
        return 1
    fi
}

# ── Main ──────────────────────────────────────────────────────────────────────
echo "conquerd-opus: checking Opus DNN model data files..."

if [[ -f "$SENTINEL" ]]; then
    echo "  Already present ($SENTINEL) — nothing to do."
    exit 0
fi

if [[ ! -d "$OPUS_SRC" ]]; then
    echo "  ERROR: Opus submodule not found at '$OPUS_SRC'."
    echo "  Initialise submodules first:  git submodule update --init --recursive"
    exit 1
fi

echo "  Downloading tarball from Xiph media server..."
echo "  URL: $DNN_URL"

TMP_TAR="$(mktemp --suffix=.tar.gz 2>/dev/null || mktemp /tmp/conquerd_opus.XXXXXX)"

cleanup() { rm -f "$TMP_TAR"; }
trap cleanup EXIT

if command -v curl &>/dev/null; then
    curl -fsSL "$DNN_URL" -o "$TMP_TAR"
else
    wget -q "$DNN_URL" -O "$TMP_TAR"
fi

echo "  Verifying SHA-256..."
verify_sha256 "$TMP_TAR" "$DNN_HASH"
echo "  Hash OK."

echo "  Extracting C data files to opus source tree..."
# The tarball contains paths like `dnn/lace_data.c` (relative, no top-level
# directory wrapper), so extracting to the opus source root places them at
# `opus/dnn/lace_data.c` where cmake's lpcnet_sources.mk expects them.
tar -xzf "$TMP_TAR" -C "$OPUS_SRC"

if [[ ! -f "$SENTINEL" ]]; then
    echo "  ERROR: Extraction succeeded but sentinel '$SENTINEL' was not created."
    echo "  The tarball may not match the expected opus commit."
    exit 1
fi

echo "  Extraction complete."
echo "conquerd-opus: DNN model data files ready."
echo "  You can now build with:  cargo build -p conquerd-client --features qt-ui"

