<#
.SYNOPSIS
    Shared helpers for the local dev integration suites.

.DESCRIPTION
    Three actors, one box:

      * the acdc/ac1 supernode cluster, reached through the supernode-manager
        wrapper (rust/conquerd-supernode-manager/launch.ps1);
      * a USB-attached Android phone, reached through adb;
      * the headless desktop client, driven by the CONQUERD_* env hooks.

    These are integration tests against live infrastructure, not unit tests.
    They assert on *observable* state - journald on the nodes, logcat and
    dumpsys on the phone, the client's own log file - because that is all three
    actors genuinely expose. Nothing here drives a UI.

    Every assertion is designed to fail loudly rather than flakily: where a
    check depends on something being live (a call in progress, a peer joined),
    it reports Skipped rather than Failed, so a red result always means a real
    regression.
#>

Set-StrictMode -Version Latest

# ── Results ────────────────────────────────────────────────────────────────

$script:Results = New-Object System.Collections.ArrayList

function Reset-DevTestResults {
    $script:Results.Clear()
}

function Add-Result {
    param(
        [Parameter(Mandatory)][string] $Suite,
        [Parameter(Mandatory)][string] $Name,
        [Parameter(Mandatory)][ValidateSet('Pass', 'Fail', 'Skip')][string] $Status,
        [string] $Detail = ''
    )
    $null = $script:Results.Add([pscustomobject]@{
        Suite  = $Suite
        Name   = $Name
        Status = $Status
        Detail = $Detail
    })
    $color = switch ($Status) {
        'Pass' { 'Green' }
        'Fail' { 'Red' }
        'Skip' { 'DarkGray' }
    }
    $tag = switch ($Status) {
        'Pass' { 'PASS' }
        'Fail' { 'FAIL' }
        'Skip' { 'SKIP' }
    }
    $line = "  [{0}] {1}" -f $tag, $Name
    if ($Detail -ne '') { $line += " - $Detail" }
    Write-Host $line -ForegroundColor $color
}

function Get-DevTestResults {
    return $script:Results
}

# ── Assertions ─────────────────────────────────────────────────────────────
#
# Thin wrappers so a suite reads as a list of claims. Each records exactly one
# result, so the summary count matches the number of claims made.

function Assert-True {
    param([string] $Suite, [string] $Name, [bool] $Condition, [string] $Detail = '')
    if ($Condition) { Add-Result $Suite $Name 'Pass' } else { Add-Result $Suite $Name 'Fail' $Detail }
}

function Assert-Equal {
    param([string] $Suite, [string] $Name, $Expected, $Actual)
    if ($Expected -eq $Actual) {
        Add-Result $Suite $Name 'Pass'
    } else {
        Add-Result $Suite $Name 'Fail' ("expected '{0}', got '{1}'" -f $Expected, $Actual)
    }
}

<#
    Assert a log window does NOT contain a pattern.

    This is the shape most regression guards here take: a bug that has been
    fixed leaves a recognisable line behind when it returns, and its absence
    over a recent window is the cheapest durable proof the fix still holds.
#>
function Assert-NoMatch {
    param(
        [string] $Suite,
        [string] $Name,
        [string[]] $Lines,
        [string] $Pattern,
        [string] $Because = ''
    )
    $hits = @($Lines | Select-String -Pattern $Pattern)
    if ($hits.Count -eq 0) {
        Add-Result $Suite $Name 'Pass'
    } else {
        $sample = $hits[0].Line.Trim()
        if ($sample.Length -gt 140) { $sample = $sample.Substring(0, 140) + '...' }
        $detail = "{0} match(es); first: {1}" -f $hits.Count, $sample
        if ($Because -ne '') { $detail = "$Because | $detail" }
        Add-Result $Suite $Name 'Fail' $detail
    }
}

function Skip-Test {
    param([string] $Suite, [string] $Name, [string] $Why)
    Add-Result $Suite $Name 'Skip' $Why
}

# ── Paths ──────────────────────────────────────────────────────────────────

function Get-RepoRoot {
    return (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
}

function Get-ManagerDir {
    return (Join-Path (Get-RepoRoot) 'rust\conquerd-supernode-manager')
}

<#
    The desktop profile the GUI and the headless build share.

    run_client.bat points CONQUERD_HOME at .clientA, so the client log the
    suites read is the same one the real app writes - which is the point: a
    test that reads a different profile than the app uses proves nothing.
#>
function Get-ClientProfileDir {
    return (Join-Path (Get-RepoRoot) '.clientA')
}

function Get-ClientLogPath {
    return (Join-Path (Get-ClientProfileDir) 'logs\conquerd-client.log')
}

function Get-AdbPath {
    $cmd = Get-Command adb -ErrorAction SilentlyContinue
    if ($null -ne $cmd) { return $cmd.Source }
    $candidates = @(
        (Join-Path $env:LOCALAPPDATA 'Android\Sdk\platform-tools\adb.exe'),
        (Join-Path ${env:ProgramFiles} 'Android\platform-tools\adb.exe')
    )
    foreach ($c in $candidates) {
        if (Test-Path $c) { return $c }
    }
    return $null
}

# ── Cluster (supernode-manager) ────────────────────────────────────────────

<#
    All cluster members as (Host, Instance) pairs.

    acdc a/b/c live on one VPS; ac1/a1 is the cross-host member that makes the
    multi-home and failover paths real. Several invariants below only mean
    something when every member is checked, so this is the canonical list.
#>
function Get-ClusterNodes {
    return @(
        @{ HostName = 'acdc'; Instance = 'a'  },
        @{ HostName = 'acdc'; Instance = 'b'  },
        @{ HostName = 'acdc'; Instance = 'c'  },
        @{ HostName = 'ac1';  Instance = 'a1' }
    )
}

function Invoke-Manager {
    param([Parameter(Mandatory)][string[]] $ManagerArgs)
    $launch = Join-Path (Get-ManagerDir) 'launch.ps1'
    if (-not (Test-Path $launch)) { return $null }
    Push-Location (Get-ManagerDir)
    try {
        $out = & $launch @ManagerArgs 2>&1
        return @($out | ForEach-Object { "$_" })
    } catch {
        return @("MANAGER-ERROR: $_")
    } finally {
        Pop-Location
    }
}

<#
    A node's recent journal, as plain lines.

    `Since` is passed to journalctl verbatim ("20 min ago", "13:00"). Kept
    short by default: these suites assert on what is happening now, and a wide
    window drags in history from before the thing under test.
#>
function Get-NodeLog {
    param(
        [Parameter(Mandatory)][string] $HostName,
        [Parameter(Mandatory)][string] $Instance,
        [string] $Since = '20 min ago',
        [string] $Grep = ''
    )
    $cmd = "journalctl -u conquerd-supernode@$Instance --since '$Since' --no-pager"
    if ($Grep -ne '') { $cmd += " | grep -E '$Grep'" }
    # Cap the volume: a chatty node can emit tens of thousands of relay lines,
    # and every assertion here is satisfied by the tail.
    $cmd += " | tail -4000"
    # A grep that matches nothing exits 1, which the manager surfaces as a
    # failed remote command - indistinguishable from an unreachable node. For
    # these suites "no matching lines" is a legitimate and common result (it is
    # what a healthy idle node looks like), so normalise the exit code and let
    # the caller judge the empty set.
    $cmd += " ; exit 0"
    return Invoke-Manager @('exec', '--host', $HostName, '--instance', $Instance, $cmd)
}

function Get-ClusterStatus {
    param([string] $HostName = 'acdc')
    return Invoke-Manager @('status', '--host', $HostName, '--all')
}

# ── Phone (adb) ────────────────────────────────────────────────────────────

$script:PhonePackage = 'com.conquerd.client'

function Get-PhonePackage { return $script:PhonePackage }

function Invoke-Adb {
    param([Parameter(Mandatory)][string[]] $AdbArgs)
    $adb = Get-AdbPath
    if ($null -eq $adb) { return $null }
    $out = & $adb @AdbArgs 2>&1
    return @($out | ForEach-Object { "$_" })
}

function Get-PhoneDeviceCount {
    $lines = Invoke-Adb @('devices')
    if ($null -eq $lines) { return 0 }
    return @($lines | Where-Object { $_ -match '^\S+\s+device$' }).Count
}

function Get-PhonePid {
    $out = Invoke-Adb @('shell', 'pidof', $script:PhonePackage)
    if ($null -eq $out) { return '' }
    return (($out -join ' ').Trim())
}

function Get-PhoneAppLog {
    param([int] $Tail = 4000)
    $appPid = Get-PhonePid
    if ($appPid -eq '') { return @() }
    # --pid keeps the system's own chatter (thermal, sensors, wifi) out: it
    # dwarfs the app's lines and makes every grep below unreliable.
    $out = Invoke-Adb @('logcat', '-d', "--pid=$appPid")
    if ($null -eq $out) { return @() }
    if ($out.Count -le $Tail) { return $out }
    return $out[($out.Count - $Tail)..($out.Count - 1)]
}

<#
    The app's live AAudio playback stream, as reported by the audio server.

    This is the only place the stream's *actual* AudioAttributes are visible.
    The app cannot report its own usage meaningfully - it asks for one and the
    platform decides - so asserting the routing change really took effect has
    to come from dumpsys.
#>
function Get-PhoneAudioStream {
    $out = Invoke-Adb @('shell', 'dumpsys', 'audio')
    if ($null -eq $out) { return $null }
    $text = $out -join "`n"
    $appPid = Get-PhonePid
    if ($appPid -eq '') { return $null }
    foreach ($line in $out) {
        if ($line -match 'type:AAudio' -and $line -match "/$appPid\b" -and $line -match 'state:started') {
            $usage = ''
            $rate = ''
            if ($line -match 'usage=(\w+)') { $usage = $Matches[1] }
            if ($line -match 'sampleRate=(\d+)') { $rate = $Matches[1] }
            return [pscustomobject]@{ Usage = $usage; SampleRate = $rate; Raw = $line }
        }
    }
    return $null
}

function Get-PhoneAudioMode {
    $out = Invoke-Adb @('shell', 'dumpsys', 'audio')
    if ($null -eq $out) { return $null }
    $applied = ($out | Select-String -Pattern 'Applied Preferred communication device:' | Select-Object -First 1)
    $value = ''
    if ($null -ne $applied) { $value = $applied.Line.Trim() }
    return [pscustomobject]@{ AppliedCommunicationDevice = $value }
}

# ── Desktop (headless client) ──────────────────────────────────────────────

function Get-HeadlessBinary {
    $root = Get-RepoRoot
    $release = Join-Path $root 'rust\target-headless\release\conquerd-client.exe'
    $debug = Join-Path $root 'rust\target-headless\debug\conquerd-client.exe'
    if (Test-Path $release) { return $release }
    if (Test-Path $debug) { return $debug }
    return $null
}

<#
    True when a GUI client is holding the .clientA profile.

    The headless scenarios write to the same profile, and two clients sharing
    one identity fight over the log file and the QUIC listener port. Scenarios
    check this and skip rather than producing a confusing failure.
#>
function Test-GuiClientRunning {
    $procs = Get-Process -Name 'conquerd-client', 'DoubleSlash' -ErrorAction SilentlyContinue
    return ($null -ne $procs)
}

<#
    Run the headless client with scripted env hooks until it exits.

    Returns the captured stdout/stderr. `CONQUERD_SIMULATE_EXIT=1` is what
    makes this terminate on its own; without it the client is a daemon and the
    caller would hang.
#>
function Invoke-HeadlessClient {
    param(
        [hashtable] $EnvVars = @{},
        [int] $TimeoutSec = 90
    )
    $binary = Get-HeadlessBinary
    if ($null -eq $binary) { return $null }

    $outFile = Join-Path ([System.IO.Path]::GetTempPath()) ("devtest-headless-{0}.log" -f ([guid]::NewGuid().ToString('N')))
    $saved = @{}
    $profileDir = Get-ClientProfileDir
    $defaults = @{
        CONQUERD_HOME    = $profileDir
        CONQUERD_KEY_DIR = $profileDir
        RUST_LOG         = 'conquerd_client=info,warn'
    }
    $passFile = Join-Path $profileDir 'passphrase.local'
    if (Test-Path $passFile) { $defaults['CONQUERD_PASSPHRASE_FILE'] = $passFile }

    $all = $defaults.Clone()
    foreach ($k in $EnvVars.Keys) { $all[$k] = $EnvVars[$k] }

    foreach ($k in $all.Keys) {
        $saved[$k] = [Environment]::GetEnvironmentVariable($k)
        Set-Item -Path "env:$k" -Value $all[$k]
    }
    try {
        $proc = Start-Process -FilePath $binary -PassThru -NoNewWindow `
            -RedirectStandardOutput $outFile -RedirectStandardError "$outFile.err"
        if (-not $proc.WaitForExit($TimeoutSec * 1000)) {
            try { $proc.Kill() } catch {}
            return @("DEVTEST-TIMEOUT after ${TimeoutSec}s")
        }
        $lines = @()
        if (Test-Path $outFile) { $lines += Get-Content $outFile }
        if (Test-Path "$outFile.err") { $lines += Get-Content "$outFile.err" }
        return $lines
    } finally {
        foreach ($k in $saved.Keys) {
            if ($null -eq $saved[$k]) {
                Remove-Item -Path "env:$k" -ErrorAction SilentlyContinue
            } else {
                Set-Item -Path "env:$k" -Value $saved[$k]
            }
        }
        Remove-Item $outFile, "$outFile.err" -ErrorAction SilentlyContinue
    }
}

Export-ModuleMember -Function *
