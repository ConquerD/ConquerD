# fetch_opus_weights.ps1
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
#   powershell -ExecutionPolicy Bypass -File scripts/fetch_opus_weights.ps1

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# ── Configuration ─────────────────────────────────────────────────────────────
# These values correspond to the DNN data package for the libopus commit
# tracked by the conquerd-opus submodule.
# The hash is the SHA-256 of the tarball itself (it is embedded in the URL).
$DNN_HASH = "a5177ec6fb7d15058e99e57029746100121f68e4890b1467d4094aa336b6013e"
$DNN_URL  = "https://media.xiph.org/opus/models/opus_data-" + $DNN_HASH + ".tar.gz"

$SCRIPT_DIR = $PSScriptRoot
$OPUS_SRC   = Join-Path $SCRIPT_DIR "..\rust\conquerd-opus\opus"
$SENTINEL   = Join-Path $OPUS_SRC "dnn\lace_data.c"

# ── Helpers ───────────────────────────────────────────────────────────────────
function Verify-Sha256 ([string]$Path, [string]$Expected) {
    $actual = (Get-FileHash $Path -Algorithm SHA256).Hash.ToLower()
    if ($actual -ne $Expected.ToLower()) {
        $nl = [Environment]::NewLine
        throw ("SHA-256 mismatch for " + $Path + $nl +
               "  expected: " + $Expected + $nl +
               "  got:      " + $actual)
    }
}

# ── Main ──────────────────────────────────────────────────────────────────────
Write-Host "conquerd-opus: checking Opus DNN model data files..."

if (Test-Path $SENTINEL) {
    Write-Host ("  Already present (" + $SENTINEL + ") — nothing to do.")
    exit 0
}

if (-not (Test-Path $OPUS_SRC)) {
    $nl = [Environment]::NewLine
    throw ("Opus submodule not found at " + $OPUS_SRC + $nl +
           "Initialise submodules first:  git submodule update --init --recursive")
}

Write-Host "  Downloading tarball from Xiph media server..."
Write-Host ("  URL: " + $DNN_URL)

$tmpTar = [System.IO.Path]::GetTempFileName() + ".tar.gz"
try {
    Invoke-WebRequest -Uri $DNN_URL -OutFile $tmpTar -UseBasicParsing

    Write-Host "  Verifying SHA-256..."
    Verify-Sha256 $tmpTar $DNN_HASH
    Write-Host "  Hash OK."

    Write-Host "  Extracting C data files to opus source tree..."
    # The tarball contains paths like `dnn/lace_data.c` (relative, no top-level
    # directory wrapper), so extracting to the opus source root places them at
    # `opus/dnn/lace_data.c` where cmake's lpcnet_sources.mk expects them.
    tar -xzf $tmpTar -C $OPUS_SRC

    if (-not (Test-Path $SENTINEL)) {
        $nl = [Environment]::NewLine
        throw ("Extraction succeeded but sentinel was not created: " + $SENTINEL + $nl +
               "The tarball may not match the expected opus commit.")
    }

    Write-Host "  Extraction complete."
} finally {
    if (Test-Path $tmpTar) { Remove-Item $tmpTar -ErrorAction SilentlyContinue }
}

Write-Host "conquerd-opus: DNN model data files ready."
Write-Host "  You can now build with:  cargo build -p conquerd-client --features qt-ui"

