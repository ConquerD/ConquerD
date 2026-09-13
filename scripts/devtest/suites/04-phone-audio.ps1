<#
    Phone-side audio invariants.

    Observation only - nothing here drives the UI. The checks that need a live
    voice session skip when there isn't one, so this suite is safe to run at
    any time and only reports red on a genuine regression.

    Join voice on the phone before running for full coverage.
#>

$Suite = 'phone-audio'

if ((Get-PhoneDeviceCount) -ne 1) {
    Skip-Test $Suite 'phone audio' 'no single attached device'
    return
}

$appPid = Get-PhonePid
if ($appPid -eq '') {
    Skip-Test $Suite 'phone audio' 'client not running on device'
    return
}

$log = Get-PhoneAppLog

# ── Stream usage ───────────────────────────────────────────────────────────
#
# The whole point of the vendored cpal fork (rust/patches/cpal) is that the
# Oboe output stream opens as VoiceCommunication rather than the stock
# Usage::Media. Android routes a media-usage stream to the loudspeaker and
# ignores setCommunicationDevice() for it, so earpiece/headset selection is
# impossible and no platform echo canceller is attached to capture. If this
# ever reads USAGE_MEDIA again, the patch has been dropped - most likely by a
# cargo update resolving cpal from crates.io instead of the fork.

$stream = Get-PhoneAudioStream
if ($null -eq $stream) {
    Skip-Test $Suite 'audio stream usage is VoiceCommunication' 'no started AAudio stream (not in a call?)'
    Skip-Test $Suite 'communication device applied' 'no started AAudio stream (not in a call?)'
} else {
    Assert-Equal $Suite 'audio stream usage is VoiceCommunication' 'USAGE_VOICE_COMMUNICATION' $stream.Usage

    # Routing only takes effect once something owns the communication route;
    # AudioRouter claims it for the life of a call or room-voice session.
    $mode = Get-PhoneAudioMode
    $applied = ''
    if ($null -ne $mode) { $applied = $mode.AppliedCommunicationDevice }
    Assert-True $Suite 'communication device applied' ($applied -notmatch 'null') `
        "no preferred communication device while a stream is live: $applied"
}

# ── Playback health ────────────────────────────────────────────────────────
#
# Underruns starve the playback ring; PLC stretches to fill the gap, which is
# what "slowed down / warbling" audio actually is. The jitter buffer grows to
# compensate, so a climbing depth is the same story told twice.

$jitter = @($log | Select-String -Pattern 'Jitter buffer depth')
if ($jitter.Count -eq 0) {
    Skip-Test $Suite 'playback underruns within tolerance' 'no jitter-buffer samples (no audio received yet)'
} else {
    $worst = 0.0
    foreach ($m in $jitter) {
        if ($m.Line -match 'underruns (\d+)/(\d+)') {
            $under = [double]$Matches[1]
            $total = [double]$Matches[2]
            if ($total -gt 0) {
                $ratio = $under / $total
                if ($ratio -gt $worst) { $worst = $ratio }
            }
        }
    }
    # 10%: below this is ordinary network jitter the buffer absorbs; sustained
    # double digits is audible as stretching rather than as an occasional tick.
    Assert-True $Suite 'playback underruns within tolerance' ($worst -lt 0.10) `
        ("worst underrun ratio {0:P0} across {1} sample(s)" -f $worst, $jitter.Count)
}

# ── Encryption + transport ─────────────────────────────────────────────────

# Room audio is sealed under the room group key before any transport is
# chosen. Without a real key every frame is dropped ahead of the send, which
# is total silence in both directions while signalling looks perfectly healthy.
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


# The relay-dial de-dupe landing on the client side. Seeing this line is fine
# and expected; what it proves is the guard exists at all.
$dedupe = @($log | Select-String -Pattern 'connect already in flight').Count
if ($dedupe -gt 0) {
    Add-Result $Suite 'relay dial de-dupe active' 'Pass' "$dedupe collapsed dial(s)"
} else {
    Skip-Test $Suite 'relay dial de-dupe active' 'no concurrent grants observed in window'
}
