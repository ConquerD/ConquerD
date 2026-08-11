#!/usr/bin/env bash
# Report LLVM line/region coverage % for ConquerD Rust crates (cargo-llvm-cov).
#
# Run from the repository root:
#
#   bash scripts/coverage.sh
#   bash scripts/coverage.sh --scope all --html
#   bash scripts/coverage.sh --scope features --fail-under-lines 50
#
# See scripts/coverage.ps1 for the Windows equivalent and full option notes.
set -euo pipefail

RUST_TOOLCHAIN="${RUST_TOOLCHAIN:-1.97.1}"
SCOPE="hot"
HTML=0
FAIL_UNDER=0
SKIP_INSTALL=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --scope) SCOPE="$2"; shift ;;
        --html) HTML=1 ;;
        --fail-under-lines) FAIL_UNDER="$2"; shift ;;
        --skip-install) SKIP_INSTALL=1 ;;
        --toolchain) RUST_TOOLCHAIN="$2"; shift ;;
        -h|--help)
            echo "Usage: bash scripts/coverage.sh [--scope hot|all|features|supernode|client|installer] [--html] [--fail-under-lines N] [--skip-install]"
            exit 0
            ;;
        *) echo "Unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done

case "$SCOPE" in
    hot|all|features|supernode|client|installer) ;;
    *) echo "Invalid --scope: $SCOPE" >&2; exit 2 ;;
esac

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RUST_DIR="$REPO_ROOT/rust"
CLIENT_DIR="$RUST_DIR/conquerd-client"
OUT_DIR="$REPO_ROOT/coverage"

step() {
    echo ""
    echo "==> $1"
}

ensure_tools() {
    if [[ "$SKIP_INSTALL" -eq 1 ]]; then
        return
    fi
    step "Ensure toolchain $RUST_TOOLCHAIN + llvm-tools-preview"
    rustup toolchain install "$RUST_TOOLCHAIN" --component llvm-tools-preview
    export RUSTUP_TOOLCHAIN="$RUST_TOOLCHAIN"
    rustup component add llvm-tools-preview --toolchain "$RUST_TOOLCHAIN"
    if ! cargo llvm-cov --version >/dev/null 2>&1; then
        step "Install cargo-llvm-cov 0.8.7"
        cargo install cargo-llvm-cov --locked --version 0.8.7
    fi
}

# args after name: working_dir then cargo llvm-cov package args...
run_cov() {
    local name="$1"
    local dir="$2"
    shift 2
    step "Coverage: $name"
    mkdir -p "$OUT_DIR"
    local json="$OUT_DIR/${name}.json"
    local lcov="$OUT_DIR/${name}.lcov"
    (
        cd "$dir"
        cargo llvm-cov "$@" --json --summary-only --output-path "$json"
        cargo llvm-cov report --lcov --output-path "$lcov"
        if [[ "$HTML" -eq 1 ]]; then
            local html_dir="$OUT_DIR/html/$name"
            mkdir -p "$html_dir"
            cargo llvm-cov report --html --output-dir "$html_dir"
        fi
    )
}

# Print lines/regions/functions percent from llvm-cov JSON export.
# Usage: json_totals path → prints "lines regions functions covered count"
json_totals() {
    local path="$1"
    python3 - "$path" <<'PY'
import json, sys
path = sys.argv[1]
with open(path, encoding="utf-8") as f:
    raw = json.load(f)
totals = None
if isinstance(raw.get("data"), list) and raw["data"]:
    totals = raw["data"][0].get("totals")
elif "totals" in raw:
    totals = raw["totals"]
if not totals:
    print("n/a n/a n/a ? ?")
    sys.exit(0)

def pct(key):
    obj = totals.get(key) or {}
    if "percent" in obj:
        return float(obj["percent"])
    c, cov = obj.get("count"), obj.get("covered")
    if c and cov is not None and float(c) > 0:
        return 100.0 * float(cov) / float(c)
    return None

def fmt(v):
    return "n/a" if v is None else f"{v:.2f}"

lines = totals.get("lines") or {}
print(
    fmt(pct("lines")),
    fmt(pct("regions")),
    fmt(pct("functions")),
    lines.get("covered", "?"),
    lines.get("count", "?"),
)
PY
}

ensure_tools
mkdir -p "$OUT_DIR"

declare -a NAMES=()
declare -a DIRS=()
declare -a EXTRA=()

queue() {
    NAMES+=("$1")
    DIRS+=("$2")
    EXTRA+=("$3")
}

case "$SCOPE" in
    features)
        queue conquerd-features "$RUST_DIR" "-p conquerd-features"
        ;;
    supernode)
        queue conquerd-supernode "$RUST_DIR" "-p conquerd-supernode"
        ;;
    installer)
        queue conquerd-installer "$RUST_DIR" "-p conquerd-installer"
        ;;
    client)
        queue conquerd-client "$CLIENT_DIR" ""
        ;;
    hot)
        queue conquerd-features "$RUST_DIR" "-p conquerd-features"
        queue conquerd-supernode "$RUST_DIR" "-p conquerd-supernode"
        queue conquerd-client "$CLIENT_DIR" ""
        ;;
    all)
        queue conquerd-features "$RUST_DIR" "-p conquerd-features"
        queue conquerd-supernode "$RUST_DIR" "-p conquerd-supernode"
        queue conquerd-installer "$RUST_DIR" "-p conquerd-installer"
        queue conquerd-client "$CLIENT_DIR" ""
        ;;
esac

for i in "${!NAMES[@]}"; do
    name="${NAMES[$i]}"
    dir="${DIRS[$i]}"
    # shellcheck disable=SC2206
    extra=( ${EXTRA[$i]} )
    if [[ ${#extra[@]} -eq 0 || -z "${extra[0]:-}" ]]; then
        run_cov "$name" "$dir"
    else
        run_cov "$name" "$dir" "${extra[@]}"
    fi
done

SUMMARY="$OUT_DIR/summary.md"
{
    echo "# ConquerD coverage"
    echo ""
    echo "Scope: \`$SCOPE\` · Toolchain: \`$RUST_TOOLCHAIN\` · Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo ""
    echo "| Package | Lines % | Regions % | Functions % | Lines covered |"
    echo "|---|---:|---:|---:|---:|"
    for name in "${NAMES[@]}"; do
        read -r lp rp fp cov count < <(json_totals "$OUT_DIR/${name}.json")
        echo "| $name | $lp | $rp | $fp | ${cov}/${count} |"
    done
    echo ""
    echo "Artifacts: \`coverage/*.lcov\`, \`coverage/*.json\`"
    if [[ "$HTML" -eq 1 ]]; then
        echo "HTML: \`coverage/html/<package>/\`"
    fi
    echo ""
    echo "Notes:"
    echo "- \`conquerd-opus\` (native C/DNN) is excluded — high ROI is protocol/SFU/features/client."
    echo "- Client run is headless (no \`qt-ui\`); Qt/QML UI is not instrumented."
    echo "- Default CI scope is \`hot\`. Raise floors gradually with \`--fail-under-lines\`."
} > "$SUMMARY"

echo ""
echo "=== Coverage summary ==="
cat "$SUMMARY"
echo ""
echo "Wrote $SUMMARY"

if [[ "$FAIL_UNDER" -gt 0 ]]; then
    failed=0
    for name in "${NAMES[@]}"; do
        read -r lp _ _ _ _ < <(json_totals "$OUT_DIR/${name}.json")
        if [[ "$lp" != "n/a" ]]; then
            # bash arithmetic needs integer; use awk for float compare
            if awk "BEGIN { exit !($lp < $FAIL_UNDER) }"; then
                echo "FAIL: $name line coverage ${lp}% < ${FAIL_UNDER}%" >&2
                failed=1
            fi
        fi
    done
    if [[ "$failed" -ne 0 ]]; then
        exit 1
    fi
fi
