<#
.SYNOPSIS
    Verify that conquerd-client/Cargo.toml [package].version matches every other
    tracked crate shipped in a release.

.DESCRIPTION
    Run from the repo root:

        pwsh scripts/check_version_sync.ps1

    Exit status:
        0  all versions match.
        1  one or more mismatches (printed to stderr).

    Invoked from CI so a forgotten Cargo.toml bump fails the build instead of
    producing inconsistent ProductVersion metadata in signed PE files.
#>

[CmdletBinding()]
param(
    [Parameter()]
    [string]$BumpTo
)

$ErrorActionPreference = 'Stop'

$repoRoot    = Split-Path -Parent $PSScriptRoot
$rustDir     = Join-Path $repoRoot 'rust'
$clientCargo = Join-Path (Join-Path $rustDir 'conquerd-client') 'Cargo.toml'

$trackedCrates = @(
    'conquerd-features',
    'conquerd-installer',
    'conquerd-supernode'
)

function Get-CrateVersion {
    param([string]$CargoToml)
    if (-not (Test-Path $CargoToml)) {
        Write-Error "ERROR: missing $CargoToml"
        exit 1
    }
    $text = Get-Content $CargoToml -Raw
    if ($text -match '(?m)^version\s*=\s*"([^"]+)"') {
        return $Matches[1]
    }
    Write-Error "ERROR: no [package].version in $CargoToml"
    exit 1
}

$expected = Get-CrateVersion $clientCargo

$mismatches = @()
foreach ($crate in $trackedCrates) {
    $cargoToml = Join-Path (Join-Path $rustDir $crate) 'Cargo.toml'
    $actual = Get-CrateVersion $cargoToml
    if ($actual -ne $expected) {
        $mismatches += [PSCustomObject]@{ Crate = $crate; Version = $actual }
    }
}

if ($BumpTo) {
    Write-Host "`nBUMP MODE: Updating all tracked crates to $BumpTo" -ForegroundColor Yellow

    function Set-Version {
        param([string]$Path, [string]$Version)
        if (-not (Test-Path $Path)) {
            Write-Error "Missing: $Path"
            exit 1
        }
        $content = Get-Content $Path -Raw
        $newContent = $content -replace '(?m)^(version\s*=\s*)"[0-9]+\.[0-9]+\.[0-9]+"', "`$1`"$Version`""
        Set-Content -Path $Path -Value $newContent -NoNewline
        Write-Host "  Updated: $Path" -ForegroundColor Green
    }

    Set-Version $clientCargo $BumpTo
    foreach ($crate in $trackedCrates) {
        $path = Join-Path (Join-Path $rustDir $crate) 'Cargo.toml'
        Set-Version $path $BumpTo
    }

    Write-Host "`nAll crates updated to $BumpTo." -ForegroundColor Green
    Write-Host "Recommended next steps:" -ForegroundColor Cyan
    Write-Host "  1. git add -u && git commit -m 'Bump version to $BumpTo'"
    Write-Host "  2. git tag v$BumpTo"
    Write-Host "  3. Push commit + tag, then let CI/release workflow run."
    exit 0
}

if ($mismatches.Count -gt 0) {
    Write-Host "Version drift detected (conquerd-client version=$expected):" -ForegroundColor Red
    foreach ($m in $mismatches) {
        Write-Host "  rust/$($m.Crate)/Cargo.toml = $($m.Version)" -ForegroundColor Red
    }
    Write-Host "Bump every crate's [package].version to match conquerd-client." -ForegroundColor Red
    exit 1
}

Write-Host "OK: all $($trackedCrates.Count) Rust crates match conquerd-client version=$expected"
exit 0


