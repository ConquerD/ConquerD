#!/usr/bin/env bash
# Launch the ConquerD native client (debug build).
# Build first: cd rust/conquerd-client && cargo build --features qt-ui

set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
BINARY="$ROOT/rust/target/debug/conquerd-client"

if [ ! -f "$BINARY" ]; then
    echo "ConquerD client binary not found at:"
    echo "  $BINARY"
    echo ""
    echo "Build it first:"
    echo "  cd rust/conquerd-client"
    echo "  cargo build --features qt-ui"
    exit 1
fi

exec "$BINARY" "$@"
