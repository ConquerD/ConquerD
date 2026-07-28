<#
.SYNOPSIS
    Install Qt for Windows release/CI builds via aqtinstall.

.DESCRIPTION
    Uses the external 7-Zip binary instead of py7zr's parallel extractor.
    aqtinstall v3.3.0 on GitHub Actions Windows runners often fails with
    py7zr.exceptions.Bad7zFile when multiple archives extract concurrently
    (miurahr/aqtinstall#995). --external avoids that code path.

    WebEngine pulls in WebChannel + Positioning; windeployqt fails if they
    are missing from the install.

.PARAMETER Version
    Qt version tag (default: 6.8.3).

.PARAMETER OutputDir
    aqt -O root (default: C:\Qt). Produces <OutputDir>\<Version>\msvc2022_64.
#>
[CmdletBinding()]
param(
    [string]$Version = '6.8.3',
    [string]$OutputDir = 'C:\Qt',
    [int]$MaxAttempts = 3
)

$ErrorActionPreference = 'Stop'

function Resolve-SevenZip {
    $candidates = @(
        'C:\Program Files\7-Zip\7z.exe',
        'C:\Program Files (x86)\7-Zip\7z.exe',
        "${env:ProgramFiles}\7-Zip\7z.exe"
    )
    foreach ($path in $candidates) {
        if (Test-Path $path) { return $path }
    }
    $cmd = Get-Command '7z' -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    throw @"
7-Zip not found. Install before running aqt, e.g.:
  choco install 7zip -y
"@
}

$sevenZip = Resolve-SevenZip
Write-Host "Using external 7-Zip extractor: $sevenZip"

pip install --disable-pip-version-check 'aqtinstall==3.3.0'

$aqtArgs = @(
    'install-qt', 'windows', 'desktop', $Version, 'win64_msvc2022_64',
    '-O', $OutputDir,
    '-m', 'qtwebengine', 'qtwebchannel', 'qtpositioning', 'qtmultimedia',
    '--external', $sevenZip
)

for ($attempt = 1; $attempt -le $MaxAttempts; $attempt++) {
    Write-Host "aqt install attempt $attempt/$MaxAttempts (Qt $Version -> $OutputDir)"
    & aqt @aqtArgs
    if ($LASTEXITCODE -eq 0) {
        $qtRoot = Join-Path $OutputDir "$Version\msvc2022_64"
        Write-Host "Qt installed: $qtRoot"
        exit 0
    }
    if ($attempt -lt $MaxAttempts) {
        Write-Warning "aqt failed (exit $LASTEXITCODE); retrying in 20s..."
        Start-Sleep -Seconds 20
    }
}

throw "aqt install failed after $MaxAttempts attempts (see miurahr/aqtinstall#995)"