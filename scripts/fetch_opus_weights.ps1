# fetch_opus_weights.ps1
# Downloads and extracts the Opus DNN model data files required for the
# conquerd-opus `dnn` feature (DRED + OSCE neural features).
#
# Usage (from the repository root):
#   powershell -ExecutionPolicy Bypass -File scripts/fetch_opus_weights.ps1

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$DNN_HASH = 'a5177ec6fb7d15058e99e57029746100121f68e4890b1467d4094aa336b6013e'
$DNN_URL = "https://media.xiph.org/opus/models/opus_data-$DNN_HASH.tar.gz"
$DownloadAttempts = if ($env:OPUS_DNN_DOWNLOAD_ATTEMPTS) { [int]$env:OPUS_DNN_DOWNLOAD_ATTEMPTS } else { 5 }
$DownloadRetryDelaySec = if ($env:OPUS_DNN_DOWNLOAD_RETRY_DELAY_SEC) { [int]$env:OPUS_DNN_DOWNLOAD_RETRY_DELAY_SEC } else { 20 }

$SCRIPT_DIR = $PSScriptRoot
$OPUS_SRC = Join-Path $SCRIPT_DIR '..\rust\conquerd-opus\opus'
$BUNDLED_TAR = Join-Path $SCRIPT_DIR "..\rust\conquerd-opus\assets\opus_data-$DNN_HASH.tar.gz"
$SENTINEL = Join-Path $OPUS_SRC 'dnn\lace_data.c'
$TAR_LIST = Join-Path $OPUS_SRC 'tar_list.txt'

function Test-DnnFilesComplete {
    if (-not (Test-Path $TAR_LIST)) { return $false }
    foreach ($line in Get-Content $TAR_LIST) {
        $relpath = $line.Trim()
        if ([string]::IsNullOrWhiteSpace($relpath)) { continue }
        if (-not (Test-Path (Join-Path $OPUS_SRC ($relpath -replace '/', '\')))) {
            return $false
        }
    }
    return $true
}

function Verify-Sha256 ([string]$Path, [string]$Expected) {
    $actual = (Get-FileHash $Path -Algorithm SHA256).Hash.ToLower()
    if ($actual -ne $Expected.ToLower()) {
        $nl = [Environment]::NewLine
        throw ("SHA-256 mismatch for " + $Path + $nl +
               "  expected: " + $Expected + $nl +
               "  got:      " + $actual)
    }
}

function Download-WithRetries ([string]$Url, [string]$Dest) {
    for ($attempt = 1; $attempt -le $DownloadAttempts; $attempt++) {
        Write-Host "  Download attempt $attempt/$DownloadAttempts..."
        try {
            Invoke-WebRequest -Uri $Url -OutFile $Dest -UseBasicParsing -TimeoutSec 900
            return
        } catch {
            if ($attempt -eq $DownloadAttempts) { throw }
            Write-Warning "Download failed: $_; retrying in ${DownloadRetryDelaySec}s..."
            Start-Sleep -Seconds $DownloadRetryDelaySec
        }
    }
}

Write-Host 'conquerd-opus: checking Opus DNN model data files...'

if (Test-DnnFilesComplete) {
    Write-Host '  All DNN data files from tar_list.txt already present - nothing to do.'
    exit 0
}

if (-not (Test-Path $OPUS_SRC)) {
    $nl = [Environment]::NewLine
    throw ("Opus submodule not found at " + $OPUS_SRC + $nl +
           'Initialise submodules first:  git submodule update --init --recursive')
}

$tmpTar = [System.IO.Path]::GetTempFileName() + '.tar.gz'
try {
    if (Test-Path $BUNDLED_TAR) {
        Write-Host "  Using bundled tarball: $BUNDLED_TAR"
        Copy-Item $BUNDLED_TAR $tmpTar
    } else {
        Write-Host '  Downloading tarball from Xiph media server...'
        Write-Host "  URL: $DNN_URL"
        Download-WithRetries $DNN_URL $tmpTar
    }

    Write-Host '  Verifying SHA-256...'
    Verify-Sha256 $tmpTar $DNN_HASH
    Write-Host '  Hash OK.'

    Write-Host '  Extracting C data files to opus source tree...'
    tar -xzf $tmpTar -C $OPUS_SRC

    if (-not (Test-Path $SENTINEL)) {
        $nl = [Environment]::NewLine
        throw ("Extraction succeeded but sentinel was not created: " + $SENTINEL + $nl +
               'The tarball may not match the expected opus commit.')
    }

    Write-Host '  Extraction complete.'
} finally {
    if (Test-Path $tmpTar) { Remove-Item $tmpTar -ErrorAction SilentlyContinue }
}

Write-Host 'conquerd-opus: DNN model data files ready.'
Write-Host '  You can now build with:  cargo build -p conquerd-client --features qt-ui'