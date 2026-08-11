<#
.SYNOPSIS
    Report LLVM line/region coverage % for ConquerD Rust crates.

.DESCRIPTION
    Uses cargo-llvm-cov (+ llvm-tools-preview) on both Cargo workspaces.
    Default scope is "hot" high-ROI crates: conquerd-features, conquerd-supernode,
    conquerd-client (headless). Use -Scope all for installer as well.

    Outputs:
      coverage/summary.md          Human-readable table (also printed)
      coverage/<name>.lcov         LCOV for HTML viewers / Codecov
      coverage/<name>.json         cargo-llvm-cov JSON summary
      coverage/html/               Optional HTML (when -Html)

    Run from the repository root:

        powershell -ExecutionPolicy Bypass -File scripts/coverage.ps1
        powershell -ExecutionPolicy Bypass -File scripts/coverage.ps1 -Scope all -Html
        powershell -ExecutionPolicy Bypass -File scripts/coverage.ps1 -FailUnderLines 50

.PARAMETER RustToolchain
    Must match env.RUST_TOOLCHAIN in ci.yml (default: 1.97.1).

.PARAMETER Scope
    hot       — features + supernode + headless client (default)
    all       — hot + installer (still excludes conquerd-opus C/DNN bulk)
    features  — conquerd-features only
    supernode — conquerd-supernode only
    client    — conquerd-client headless only
    installer — conquerd-installer only

.PARAMETER Html
    Also emit HTML reports under coverage/html/<name>/.

.PARAMETER FailUnderLines
    Fail if any measured package's line coverage is below this percent (0 = report only).

.PARAMETER SkipInstall
    Do not install/update cargo-llvm-cov or llvm-tools-preview.
#>

[CmdletBinding()]
param(
    [string]$RustToolchain = '1.97.1',
    [ValidateSet('hot', 'all', 'features', 'supernode', 'client', 'installer')]
    [string]$Scope = 'hot',
    [switch]$Html,
    [int]$FailUnderLines = 0,
    [switch]$SkipInstall
)

$ErrorActionPreference = 'Stop'

$RepoRoot = Split-Path -Parent $PSScriptRoot
$RustDir = Join-Path $RepoRoot 'rust'
$ClientDir = Join-Path $RustDir 'conquerd-client'
$OutDir = Join-Path $RepoRoot 'coverage'

function Write-Step([string]$Name) {
    Write-Host ""
    Write-Host "==> $Name" -ForegroundColor Cyan
}

function Ensure-Tools {
    if ($SkipInstall) { return }
    Write-Step "Ensure toolchain $RustToolchain + llvm-tools-preview"
    rustup toolchain install $RustToolchain --component llvm-tools-preview | Out-Null
    $env:RUSTUP_TOOLCHAIN = $RustToolchain
    rustup component add llvm-tools-preview --toolchain $RustToolchain | Out-Null

    $have = $false
    try {
        $ver = & cargo llvm-cov --version 2>$null
        if ($LASTEXITCODE -eq 0 -and $ver) { $have = $true }
    } catch { $have = $false }
    if (-not $have) {
        Write-Step 'Install cargo-llvm-cov 0.8.7'
        cargo install cargo-llvm-cov --locked --version 0.8.7
    }
}

function Invoke-LlvmCov {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$WorkingDir,
        # Empty is valid (whole workspace / package defaults), e.g. headless client.
        [string[]]$CargoArgs = @()
    )
    Write-Step "Coverage: $Name"
    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

    $lcov = Join-Path $OutDir "$Name.lcov"
    $json = Join-Path $OutDir "$Name.json"
    # One export format per invocation; re-use profdata via `report` for LCOV/HTML.
    $runArgs = [System.Collections.Generic.List[string]]::new()
    $runArgs.Add('llvm-cov')
    if ($CargoArgs -and $CargoArgs.Count -gt 0) {
        foreach ($a in $CargoArgs) { $runArgs.Add($a) }
    }
    $runArgs.Add('--json')
    $runArgs.Add('--summary-only')
    $runArgs.Add('--output-path')
    $runArgs.Add($json)

    Push-Location $WorkingDir
    try {
        & cargo @($runArgs.ToArray())
        if ($LASTEXITCODE -ne 0) {
            throw "cargo llvm-cov failed for $Name (exit $LASTEXITCODE)"
        }

        & cargo llvm-cov report --lcov --output-path $lcov
        if ($LASTEXITCODE -ne 0) {
            throw "cargo llvm-cov report (lcov) failed for $Name (exit $LASTEXITCODE)"
        }

        if ($Html) {
            $htmlDir = Join-Path $OutDir "html\$Name"
            New-Item -ItemType Directory -Force -Path $htmlDir | Out-Null
            & cargo llvm-cov report --html --output-dir $htmlDir
            if ($LASTEXITCODE -ne 0) {
                throw "cargo llvm-cov report (html) failed for $Name (exit $LASTEXITCODE)"
            }
        }
    }
    finally {
        Pop-Location
    }

    return @{ Name = $Name; JsonPath = $json; LcovPath = $lcov }
}

function Read-CoverageTotals([string]$JsonPath) {
    if (-not (Test-Path $JsonPath)) {
        return $null
    }
    $raw = Get-Content -Raw -Path $JsonPath | ConvertFrom-Json
    # cargo-llvm-cov JSON: either top-level data[0].totals or .data[].totals
    $totals = $null
    if ($raw.data -and $raw.data.Count -gt 0 -and $raw.data[0].totals) {
        $totals = $raw.data[0].totals
    } elseif ($raw.totals) {
        $totals = $raw.totals
    }
    if (-not $totals) { return $null }

    function Pct($obj) {
        if ($null -eq $obj) { return $null }
        if ($obj.PSObject.Properties.Name -contains 'percent') { return [double]$obj.percent }
        if ($obj.count -and $obj.covered -ne $null -and [double]$obj.count -gt 0) {
            return ([double]$obj.covered / [double]$obj.count) * 100.0
        }
        return $null
    }

    return [pscustomobject]@{
        Lines   = Pct $totals.lines
        Regions = Pct $totals.regions
        Functions = Pct $totals.functions
        LinesCovered = if ($totals.lines) { $totals.lines.covered } else { $null }
        LinesCount   = if ($totals.lines) { $totals.lines.count } else { $null }
    }
}

Ensure-Tools
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$jobs = @()
switch ($Scope) {
    'features' {
        $jobs += @{ Name = 'conquerd-features'; Dir = $RustDir; Args = @('-p', 'conquerd-features') }
    }
    'supernode' {
        $jobs += @{ Name = 'conquerd-supernode'; Dir = $RustDir; Args = @('-p', 'conquerd-supernode') }
    }
    'installer' {
        $jobs += @{ Name = 'conquerd-installer'; Dir = $RustDir; Args = @('-p', 'conquerd-installer') }
    }
    'client' {
        # Headless: qt-ui is optional and off unless explicitly enabled (matches CI).
        $jobs += @{ Name = 'conquerd-client'; Dir = $ClientDir; Args = @() }
    }
    'hot' {
        $jobs += @{ Name = 'conquerd-features'; Dir = $RustDir; Args = @('-p', 'conquerd-features') }
        $jobs += @{ Name = 'conquerd-supernode'; Dir = $RustDir; Args = @('-p', 'conquerd-supernode') }
        $jobs += @{ Name = 'conquerd-client'; Dir = $ClientDir; Args = @() }
    }
    'all' {
        $jobs += @{ Name = 'conquerd-features'; Dir = $RustDir; Args = @('-p', 'conquerd-features') }
        $jobs += @{ Name = 'conquerd-supernode'; Dir = $RustDir; Args = @('-p', 'conquerd-supernode') }
        $jobs += @{ Name = 'conquerd-installer'; Dir = $RustDir; Args = @('-p', 'conquerd-installer') }
        $jobs += @{ Name = 'conquerd-client'; Dir = $ClientDir; Args = @() }
    }
}

$results = @()
foreach ($j in $jobs) {
    $meta = Invoke-LlvmCov -Name $j.Name -WorkingDir $j.Dir -CargoArgs $j.Args
    $tot = Read-CoverageTotals $meta.JsonPath
    $results += [pscustomobject]@{
        Package   = $j.Name
        LinesPct  = if ($tot) { $tot.Lines } else { $null }
        RegionsPct = if ($tot) { $tot.Regions } else { $null }
        FuncsPct  = if ($tot) { $tot.Functions } else { $null }
        Lines     = if ($tot) { "{0}/{1}" -f $tot.LinesCovered, $tot.LinesCount } else { '?' }
        Json      = $meta.JsonPath
        Lcov      = $meta.LcovPath
    }
}

$md = New-Object System.Collections.Generic.List[string]
$md.Add('# ConquerD coverage')
$md.Add('')
$md.Add("Scope: ``$Scope`` | Toolchain: ``$RustToolchain`` | Generated: $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')")
$md.Add('')
$md.Add('| Package | Lines % | Regions % | Functions % | Lines covered |')
$md.Add('|---|---:|---:|---:|---:|')
foreach ($r in $results) {
    $lp = if ($null -ne $r.LinesPct) { '{0:F2}' -f $r.LinesPct } else { 'n/a' }
    $rp = if ($null -ne $r.RegionsPct) { '{0:F2}' -f $r.RegionsPct } else { 'n/a' }
    $fp = if ($null -ne $r.FuncsPct) { '{0:F2}' -f $r.FuncsPct } else { 'n/a' }
    $md.Add("| $($r.Package) | $lp | $rp | $fp | $($r.Lines) |")
}
$md.Add('')
$md.Add('Artifacts: `coverage/*.lcov`, `coverage/*.json`')
if ($Html) {
    $md.Add('HTML: `coverage/html/<package>/`')
}
$md.Add('')
$md.Add('Notes:')
$md.Add('- `conquerd-opus` (native C/DNN) is excluded; high ROI is protocol/SFU/features/client.')
$md.Add('- Client run is headless (no `qt-ui`); Qt/QML UI is not instrumented.')
$md.Add('- Default CI scope is `hot`. Raise floors gradually with `-FailUnderLines`.')

$summaryPath = Join-Path $OutDir 'summary.md'
# UTF-8 without BOM keeps GitHub step summaries and shell tools happy.
$utf8 = New-Object System.Text.UTF8Encoding $false
[System.IO.File]::WriteAllText($summaryPath, ($md -join "`n") + "`n", $utf8)

Write-Host ""
Write-Host "=== Coverage summary ===" -ForegroundColor Green
Get-Content $summaryPath | Write-Host
Write-Host ""
Write-Host "Wrote $summaryPath"

$failed = $false
if ($FailUnderLines -gt 0) {
    foreach ($r in $results) {
        if ($null -ne $r.LinesPct -and $r.LinesPct -lt $FailUnderLines) {
            Write-Host ("FAIL: {0} line coverage {1:F2}% < {2}%" -f $r.Package, $r.LinesPct, $FailUnderLines) -ForegroundColor Red
            $failed = $true
        }
    }
}

if ($failed) {
    exit 1
}
exit 0
