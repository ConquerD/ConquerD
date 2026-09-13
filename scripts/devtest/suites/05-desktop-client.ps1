<#
    Desktop-side checks, read from the profile the real app uses.

    The log assertions read .clientA/logs/conquerd-client.log - the same file
    run_client.bat writes - so they describe the client you actually ran rather
    than a synthetic one.

    The driven scenario at the end is opt-in (-IncludeDesktopScenario) because
    it starts a headless client against that same profile, and two clients
    sharing one identity fight over the log and the QUIC listener port.
#>

$Suite = 'desktop'

$logPath = Get-ClientLogPath
if (-not (Test-Path $logPath)) {
    Skip-Test $Suite 'desktop client log' 'no log yet - run run_client.bat once'
    return
}

$fullLog = @(Get-Content $logPath -ErrorAction SilentlyContinue)

# Health checks read a recent window, not the whole file.
#
# The log is truncated at startup, so it is per-session - but a session runs
# for hours, and counting from the top reports a problem that was fixed an
# hour ago as though it were happening now. The window is what makes a red
# result mean "this is wrong currently".
$RecentWindow = 4000
if ($fullLog.Count -gt $RecentWindow) {
    $log = $fullLog[($fullLog.Count - $RecentWindow)..($fullLog.Count - 1)]
} else {
    $log = $fullLog
}

# ── Audio device selection ─────────────────────────────────────────────────
#
# Logged once per pipeline start. Worth asserting present because its absence
# means audio never started at all, which otherwise looks like a network fault.

# Startup-scoped: logged once per pipeline start, which is usually far above
# the recent window, so this one deliberately reads the whole file.
$devices = @($fullLog | Select-String -Pattern 'Audio devices:')
if ($devices.Count -eq 0) {
    Skip-Test $Suite 'audio pipeline started' 'no Audio devices line (never joined voice)'
} else {
    Add-Result $Suite 'audio pipeline started' 'Pass' ($devices[-1].Line -replace '.*Audio devices: ', '')

    # Capture and playback both resample to/from Opus' 48 kHz. A mismatch is
    # not itself a bug - the pipeline resamples - but a capture device that
    # disagrees with the encoder is worth seeing, because it is the first thing
    # to suspect when audio is described as "slowed down".
    $last = $devices[-1].Line
    if ($last -match 'input=(\d+)Hz') {
        $inRate = $Matches[1]
        Assert-True $Suite 'capture rate is 48 kHz' ($inRate -eq '48000') `
            "input device at ${inRate}Hz; pipeline will resample to 48000"
    }
}

# ── Room audio health ──────────────────────────────────────────────────────

Assert-NoMatch $Suite 'room audio is keyed' $log `
    'no real group key' `
    'frames dropped before transport: the group key never arrived'

Assert-NoMatch $Suite 'no opus encode errors' $log 'Opus encode error'

# ── Group-key convergence ──────────────────────────────────────────────────
#
# "room audio is keyed" only proves this client holds *a* key, not that it
# holds the *same* key as everyone else. A desynced room passes every other
# check here and is still completely silent, so the decrypt-failure count is
# the check that actually matters.
#
# Cause seen in the field: keys are in-memory, so a restarted keyer mints
# epoch 0 again; members holding a higher epoch refuse it (a restart is
# indistinguishable from a rollback) and the room splits permanently. The
# recovery is to learn the room's epoch from frames we cannot open - which is
# why any sustained count here means that recovery is not running.

$openFailures = @($log | Select-String -Pattern 'failed to open E2E frame').Count
Assert-True $Suite 'group key converged' ($openFailures -lt 50) `
    "$openFailures undecryptable room frames - this client is on a different key epoch than the room"

Assert-NoMatch $Suite 'no rejected key epochs' $log `
    'rejecting key epoch' `
    'a key offer was refused; a restarted keyer cannot rejoin until epochs reconverge'


Assert-NoMatch $Suite 'no punch self-pairing seen by client' $log `
    'PUNCH_READY names unknown peer' `
    'supernode announced a peer this client cannot resolve'

# ── Signalling noise ───────────────────────────────────────────────────────
#
# Multi-home dedupe: one message arrives once per attached supernode and the
# replay guard drops the duplicates. Entirely correct, but logged at WARN, so
# a long session buries everything else under tens of thousands of lines. Not
# a failure - surfaced so the count is visible when reading a log by hand.

$replay = @($fullLog | Select-String -Pattern 'replayed message').Count
if ($replay -gt 0) {
    Add-Result $Suite 'multi-home dedupe observed' 'Pass' "$replay duplicate(s) dropped (WARN-level noise)"
}

# ── Driven scenario: accept an invite ──────────────────────────────────────
#
# The one end-to-end path the headless hooks genuinely support: pull a real
# reusable invite from the cluster, hand it to a fresh client via
# CONQUERD_ACCEPT_INVITE, and confirm it processed it. Accepting an invite for
# a supernode already trusted is idempotent, so this is safe to re-run.

if (-not $IncludeDesktopScenario) {
    Skip-Test $Suite 'headless accepts a cluster invite' 'pass -IncludeDesktopScenario to run'
    return
}

if (Test-GuiClientRunning) {
    Skip-Test $Suite 'headless accepts a cluster invite' 'a GUI client holds the .clientA profile; close it first'
    return
}

if ($null -eq (Get-HeadlessBinary)) {
    Skip-Test $Suite 'headless accepts a cluster invite' 'headless binary not built'
    return
}

$inviteOut = Invoke-Manager @('invite', '--host', 'acdc', '--instance', 'a')
$inviteUrl = ''
foreach ($line in @($inviteOut)) {
    if ($line -match '(https?://\S+|doubleslash://\S+|d://\S+)') { $inviteUrl = $Matches[1]; break }
}

if ($inviteUrl -eq '') {
    Skip-Test $Suite 'headless accepts a cluster invite' 'manager returned no invite URL'
    return
}

$out = Invoke-HeadlessClient -EnvVars @{
    CONQUERD_ACCEPT_INVITE = $inviteUrl
    CONQUERD_SIMULATE_EXIT = '1'
    RUST_LOG               = 'conquerd_client=info,warn'
} -TimeoutSec 90

if ($null -eq $out) {
    Skip-Test $Suite 'headless accepts a cluster invite' 'headless client did not start'
} elseif (@($out | Select-String -Pattern 'DEVTEST-TIMEOUT').Count -gt 0) {
    Add-Result $Suite 'headless accepts a cluster invite' 'Fail' 'client did not exit within 90s'
} else {
    $accepted = @($out | Select-String -Pattern 'Accepting invite from CONQUERD_ACCEPT_INVITE').Count -gt 0
    Assert-True $Suite 'headless accepts a cluster invite' $accepted `
        'client started but never reported accepting the invite'
}
