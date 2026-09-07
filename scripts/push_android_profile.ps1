<#
.SYNOPSIS
    Copy a desktop ConquerD profile onto a connected Android device.

.DESCRIPTION
    Moves an existing identity (and optionally its peer, room and chat stores)
    into the Android app's private storage, so the phone comes up as the same
    peer your desktop already is.

    This is a MOVE, not a device link. The supernode keys relay connections,
    signaling senders and the endpoint mailbox by identity, so a second live
    connection with the same identity evicts the first - and room group keys are
    sealed per member identity to a single signaling target, so the losing
    device fails closed and sees nothing in rooms. Run one at a time.

    App-private storage is not writable by `adb push`, so each file goes to
    /data/local/tmp first and is copied in via `run-as`, which works because
    debug builds are debuggable. Release builds are not, and this will refuse.

.PARAMETER ProfileDir
    Desktop profile to copy from. Defaults to $env:USERPROFILE\.conquerd.

.PARAMETER IdentityOnly
    Copy only identity.dat, leaving the phone with empty peer/room/chat stores.
    The identity is the same, so trusted peers still recognise you - you just
    start with no local history.

.PARAMETER Serial
    Target a specific device when more than one is attached (adb -s).

.EXAMPLE
    ./scripts/push_android_profile.ps1
    ./scripts/push_android_profile.ps1 -IdentityOnly
#>
[CmdletBinding()]
param(
    [string]$ProfileDir = (Join-Path $env:USERPROFILE ".conquerd"),
    [switch]$IdentityOnly,
    [string]$Serial
)

$ErrorActionPreference = "Stop"

$PackageId = "com.conquerd.client"
# Must match ConquerdCore.homeDir on the Kotlin side.
$RemoteHome = "files/conquerd"

# --- Locate adb ----------------------------------------------------------
$adb = (Get-Command adb -ErrorAction SilentlyContinue).Source
if (-not $adb) {
    $candidate = Join-Path $env:LOCALAPPDATA "Android\Sdk\platform-tools\adb.exe"
    if (Test-Path $candidate) { $adb = $candidate }
}
if (-not $adb) {
    throw "adb not found. Add platform-tools to PATH or install the Android SDK."
}

$adbArgs = @()
if ($Serial) { $adbArgs += @("-s", $Serial) }

# Arguments are passed as one explicit array rather than as remaining
# arguments. PowerShell binds any bare token starting with "-" to a parameter
# name, so `Invoke-Adb shell ... mkdir -p files/conquerd` would swallow the
# "-p" and hand Android a mkdir with no path.
function Invoke-Adb {
    param([Parameter(Mandatory = $true)][string[]]$Arguments)

    # adb reports normal progress on stderr - `adb push` writes its transfer
    # rate there on success. Windows PowerShell turns native stderr into error
    # records, so with $ErrorActionPreference = "Stop" a transfer that worked
    # would abort the script. Judge success by the exit code alone.
    $previous = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        $output = & $adb @adbArgs @Arguments 2>&1
    } finally {
        $ErrorActionPreference = $previous
    }

    if ($LASTEXITCODE -ne 0) {
        throw "adb $($Arguments -join ' ') failed:`n$output"
    }
    return $output
}

# --- Preflight -----------------------------------------------------------
if (-not (Test-Path $ProfileDir)) {
    throw "Profile directory not found: $ProfileDir"
}

$identity = Join-Path $ProfileDir "identity.dat"
if (-not (Test-Path $identity)) {
    throw "No identity.dat in $ProfileDir - is that a ConquerD profile?"
}

# @() is load-bearing: a pipeline that matches nothing yields $null, and
# $null.Count is $null rather than 0 in PowerShell 5.1 - so without it the
# "no device" check silently passes and the first push fails instead.
$devices = @((& $adb devices) -split "`n" |
    Where-Object { $_ -match "\sdevice$" })
if ($devices.Count -eq 0) {
    throw @"
No authorised device. On the phone:
  Settings > About phone > tap 'Build number' 7 times
  Settings > System > Developer options > USB debugging
Then reconnect and accept the 'Allow USB debugging' prompt.
"@
}
if ($devices.Count -gt 1 -and -not $Serial) {
    throw "More than one device attached. Re-run with -Serial <id>:`n$($devices -join "`n")"
}

# `run-as` is the whole mechanism here; without it there is no way into
# app-private storage on a non-rooted device.
$previousPreference = $ErrorActionPreference
$ErrorActionPreference = "Continue"
try {
    $runAsCheck = & $adb @adbArgs shell run-as $PackageId id 2>&1
} finally {
    $ErrorActionPreference = $previousPreference
}
if ($LASTEXITCODE -ne 0) {
    throw @"
Cannot 'run-as $PackageId'. Either the app is not installed, or the installed
build is a release build (not debuggable). Install the debug APK first:
  adb install -r android/app/build/outputs/apk/debug/app-debug.apk
Reported: $runAsCheck
"@
}

# --- Choose the file set -------------------------------------------------
# Deliberately excludes settings.json / settings.ini (desktop-shaped: window
# geometry, capture devices, shortcut state) and quic_listener_port, which is
# specific to the machine that wrote it.
$files = @("identity.dat")
if (-not $IdentityOnly) {
    $files += @("peers.dat", "my_rooms.dat", "chat_history.db")
}

Write-Host "Source : $ProfileDir"
Write-Host "Target : $PackageId ($RemoteHome)"
Write-Host ""

# SQLite keeps recent writes in the -wal sidecar, so copying chat_history.db
# alone from a running client silently loses them. Checkpoint by closing the
# desktop client, or accept the loss.
$desktop = @(Get-Process -Name "ConquerD", "conquerd-client" -ErrorAction SilentlyContinue)
if ($desktop.Count -gt 0) {
    throw @"
The desktop client is still running (PID $($desktop[0].Id)).

Close it first. Two reasons, both of which bite silently:
  * Its chat database has writes sitting in chat_history.db-wal that a clean
    exit checkpoints into the .db this script copies. Copy now and you lose
    them.
  * This identity would then be live on two devices at once. The supernode
    keys relay connections, signaling senders and the endpoint mailbox by
    identity alone, so the second connection evicts the first, and room group
    keys reach only one of them.
"@
}

$wal = Join-Path $ProfileDir "chat_history.db-wal"
if (-not $IdentityOnly -and (Test-Path $wal) -and (Get-Item $wal).Length -gt 0) {
    Write-Warning @"
chat_history.db-wal is non-empty even though no client is running. Recent
messages may not have been checkpointed into the .db being copied.
"@
}

# The core holds identity, peer, room and chat stores open while it runs.
# Swapping the files underneath it would leave the in-memory state disagreeing
# with disk, and SQLite writing a WAL for a database that no longer exists.
Write-Host "  stopping the app"
Invoke-Adb -Arguments @("shell", "am", "force-stop", $PackageId) | Out-Null

Invoke-Adb -Arguments @("shell", "run-as", $PackageId, "mkdir", "-p", $RemoteHome) | Out-Null

# A chat database arriving from another profile must not inherit the sidecar
# files of the one it replaces. SQLite salts its -wal against the database it
# belongs to, and a mismatched pair is a corruption risk rather than a clean
# rejection -- so clear them and let SQLite recreate both on first open.
if (-not $IdentityOnly) {
    foreach ($sidecar in @("chat_history.db-wal", "chat_history.db-shm")) {
        Invoke-Adb -Arguments @("shell", "run-as", $PackageId, "rm", "-f", "$RemoteHome/$sidecar") | Out-Null
    }
    Write-Host "  cleared stale SQLite sidecar files"
}

foreach ($name in $files) {
    $local = Join-Path $ProfileDir $name
    if (-not (Test-Path $local)) {
        Write-Host "  skip  $name (not present)"
        continue
    }

    $staged = "/data/local/tmp/$name"
    Invoke-Adb -Arguments @("push", $local, $staged) | Out-Null
    Invoke-Adb -Arguments @("shell", "run-as", $PackageId, "cp", $staged, "$RemoteHome/$name") | Out-Null

    # `cp` keeps the mode of an existing target but gives a newly created one
    # the mode of the source - and the staging copy in /data/local/tmp is 0666.
    # The app directory is 0700 so nothing else can walk in, but trust state
    # should not be left world-writable regardless.
    Invoke-Adb -Arguments @("shell", "run-as", $PackageId, "chmod", "600", "$RemoteHome/$name") | Out-Null

    # Staging lives in world-readable space, so don't leave key material there.
    Invoke-Adb -Arguments @("shell", "rm", "-f", $staged) | Out-Null

    $size = (Get-Item $local).Length
    Write-Host "  copied $name ($size bytes)"
}

Write-Host ""
Write-Host "Done. Launch the app and unlock with the SAME passphrase this profile uses."
Write-Host ""
Write-Host "Note: the desktop unlocks from the Windows keyring, which caches the derived"
Write-Host "key rather than the passphrase - that cache does not travel, and Android has"
Write-Host "no Keystore backend yet, so the phone will prompt on every launch."
Write-Host ""
Write-Host "Run only one device at a time on this identity. See docs/ANDROID.md."
