<#
.SYNOPSIS
    Local dev integration tests across the cluster, the phone and the desktop.

.DESCRIPTION
    Integration tests against live infrastructure, not unit tests. They assert
    on observable state - journald on the supernodes, logcat and dumpsys on the
    phone, the client's own log - and are safe to run at any time: checks that
    need something live (a call in progress, a peer joined) report Skip rather
    than Fail, so red always means a real regression.

    Most of the guards encode bugs this project has actually shipped and fixed.
    They exist because those bugs were invisible to unit tests - they only
    appear with real peers attached to a real cluster.

.PARAMETER Suite
    Run only suites whose filename matches this substring, e.g. -Suite phone.

.PARAMETER IncludeDesktopScenario
    Also run the driven headless scenario, which starts a client against the
    .clientA profile. Requires the GUI client to be closed.

.EXAMPLE
    .\Run-DevTests.ps1
    .\Run-DevTests.ps1 -Suite supernode
    .\Run-DevTests.ps1 -IncludeDesktopScenario

.NOTES
    For the fullest coverage: have the phone and desktop both joined to a voice
    room before running. Several checks skip when no audio stream is live.
#>
[CmdletBinding()]
param(
    [string] $Suite = '',
    [switch] $IncludeDesktopScenario
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Import-Module (Join-Path $PSScriptRoot 'DevTest.psm1') -Force
Reset-DevTestResults

$suiteDir = Join-Path $PSScriptRoot 'suites'
$files = Get-ChildItem $suiteDir -Filter '*.ps1' | Sort-Object Name
if ($Suite -ne '') {
    $files = $files | Where-Object { $_.Name -match [regex]::Escape($Suite) }
}

if (@($files).Count -eq 0) {
    Write-Host "No suites matched '$Suite'." -ForegroundColor Yellow
    exit 2
}

$started = Get-Date
foreach ($file in $files) {
    Write-Host ""
    Write-Host ("== {0}" -f $file.BaseName) -ForegroundColor Cyan
    try {
        # Dot-sourced so suites see the module's functions and the switch above
        # without every one of them re-importing or re-declaring it.
        . $file.FullName
    } catch {
        Add-Result $file.BaseName 'suite executed' 'Fail' "threw: $_"
    }
}

$results = Get-DevTestResults
$pass = @($results | Where-Object { $_.Status -eq 'Pass' }).Count
$fail = @($results | Where-Object { $_.Status -eq 'Fail' }).Count
$skip = @($results | Where-Object { $_.Status -eq 'Skip' }).Count
$elapsed = [int]((Get-Date) - $started).TotalSeconds

Write-Host ""
Write-Host ("{0} passed, {1} failed, {2} skipped in {3}s" -f $pass, $fail, $skip, $elapsed) `
    -ForegroundColor $(if ($fail -gt 0) { 'Red' } else { 'Green' })

if ($fail -gt 0) {
    Write-Host ""
    Write-Host "Failures:" -ForegroundColor Red
    foreach ($r in @($results | Where-Object { $_.Status -eq 'Fail' })) {
        Write-Host ("  {0} / {1}" -f $r.Suite, $r.Name) -ForegroundColor Red
        if ($r.Detail -ne '') { Write-Host ("      {0}" -f $r.Detail) -ForegroundColor DarkRed }
    }
}

# Skips are information, not failure: the rig is frequently idle, and exiting
# non-zero for "no call in progress" would make the suite useless as a habit.
if ($fail -gt 0) { exit 1 } else { exit 0 }
