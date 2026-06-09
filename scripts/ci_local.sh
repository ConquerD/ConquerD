#!/usr/bin/env bash
# Run the same checks as .github/workflows/ci.yml before pushing.
#
# Mirrors the Linux "Rust tests" job (fmt, clippy, tests, cargo-audit for both
# Cargo workspaces). Run from the repository root:
#
#   bash scripts/ci_local.sh
#
# Faster iteration (lint only):
#
#   bash scripts/ci_local.sh --skip-tests --skip-audit
#
set -euo pipefail

RUST_TOOLCHAIN="${RUST_TOOLCHAIN:-1.89.0}"
SKIP_TESTS=0
SKIP_AUDIT=0
SKIP_OPUS_FETCH=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-tests) SKIP_TESTS=1 ;;
        --skip-audit) SKIP_AUDIT=1 ;;
        --skip-opus-fetch) SKIP_OPUS_FETCH=1 ;;
        --toolchain) RUST_TOOLCHAIN="$2"; shift ;;
        -h|--help)
            echo "Usage: bash scripts/ci_local.sh [--skip-tests] [--skip-audit] [--skip-opus-fetch] [--toolchain VERSION]"
            exit 0
            ;;
        *) echo "Unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RUST_DIR="$REPO_ROOT/rust"
CLIENT_DIR="$RUST_DIR/conquerd-client"

step() {
    echo ""
    echo "==> $1"
}

run_cargo() {
    local dir="$1"
    shift
    (cd "$dir" && cargo "$@")
}

echo "ConquerD local CI (toolchain $RUST_TOOLCHAIN)"
echo "Repo: $REPO_ROOT"

step "Ensure git submodules (recursive)"
git -C "$REPO_ROOT" submodule update --init --recursive

step "Install Rust $RUST_TOOLCHAIN (rustfmt + clippy)"
rustup toolchain install "$RUST_TOOLCHAIN" --component rustfmt --component clippy
export RUSTUP_TOOLCHAIN="$RUST_TOOLCHAIN"

step "Verify version metadata stays in sync"
if command -v pwsh >/dev/null 2>&1; then
    pwsh "$REPO_ROOT/scripts/check_version_sync.ps1"
elif command -v powershell >/dev/null 2>&1; then
    powershell -ExecutionPolicy Bypass -File "$REPO_ROOT/scripts/check_version_sync.ps1"
else
    echo "pwsh or powershell required for scripts/check_version_sync.ps1" >&2
    exit 1
fi

if [[ "$SKIP_OPUS_FETCH" -eq 0 ]]; then
    step "Fetch Opus DNN model weights"
    bash "$REPO_ROOT/scripts/fetch_opus_weights.sh"
fi

step "cargo fmt --check (rust/ workspace)"
run_cargo "$RUST_DIR" fmt --all -- --check

step "cargo fmt --check (client workspace)"
run_cargo "$CLIENT_DIR" fmt --all -- --check

step "cargo clippy (rust/ workspace, -D warnings)"
run_cargo "$RUST_DIR" clippy --all -- -D warnings

step "Release manifest signer self-test"
run_cargo "$RUST_DIR" run -p conquerd-installer --bin sign-release-manifest -- --self-test

if [[ "$SKIP_TESTS" -eq 0 ]]; then
    step "cargo test --all --release (rust/ workspace)"
    run_cargo "$RUST_DIR" test --all --release

    step "cargo test (client workspace, headless)"
    run_cargo "$CLIENT_DIR" test
fi

step "cargo clippy (client workspace, headless, -D warnings)"
run_cargo "$CLIENT_DIR" clippy -p conquerd-client --no-default-features -- -D warnings

if [[ "$SKIP_AUDIT" -eq 0 ]]; then
    if ! command -v cargo-audit >/dev/null 2>&1; then
        step "Installing cargo-audit (not on PATH)"
        cargo install cargo-audit --locked
    fi

    step "cargo audit (rust/ workspace)"
    (cd "$RUST_DIR" && cargo audit --file Cargo.lock)

    step "cargo audit (client workspace)"
    (cd "$CLIENT_DIR" && cargo audit --file Cargo.lock)
fi

echo ""
echo "Local CI passed."