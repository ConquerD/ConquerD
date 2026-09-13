<#
    Regression guards for bugs this cluster has actually had.

    Each check here corresponds to a fix that landed after a real debugging
    session, and each asserts the absence of the log line that bug emitted.
    That is deliberate: these failures were expensive to find precisely because
    nothing was watching for them, and every one of them is invisible from a
    unit test - they only appear when real peers are attached.

    All are window-based over recent journal output, so they measure the
    cluster as it is running now rather than replaying history.
#>

$Suite = 'supernode'
$Window = '20 min ago'

foreach ($node in (Get-ClusterNodes)) {
    $label = "{0}/{1}" -f $node.HostName, $node.Instance
    $log = Get-NodeLog -HostName $node.HostName -Instance $node.Instance -Since $Window `
        -Grep 'punch|Relay peer|not connected'

    if ($null -eq $log -or $log.Count -eq 0) {
        Skip-Test $Suite "$label invariants" 'no matching log lines in window (node idle?)'
        continue
    }

    # ── 1. Hole-punch self-pairing ─────────────────────────────────────────
    #
    # `PUNCH_READY sent to X <-> X` means a peer was told to punch its own
    # endpoint. The cause was comparing a padded public_id against the relay's
    # un-padded room ids, so self-exclusion never fired; the same mismatch also
    # announced each real pair under a spelling the peer's roster could not
    # resolve, leaving one side "direct" and the other on relay.
    #
    # Ids are truncated to 12 chars in these logs, which is why the backref is
    # on exactly that width.
    Assert-NoMatch $Suite "$label no punch self-pairing" $log `
        'PUNCH_READY sent to ([A-Za-z0-9_-]{12}) \S+ \1[ (]' `
        'peer told to punch its own endpoint (pad-normalisation regression)'

    # ── 2. Relay connect churn ─────────────────────────────────────────────
    #
    # The client de-duped relay dials against a map only populated once a
    # connect *completed*, so a burst of grants each spawned their own. The
    # supernode keeps the newest and drops the rest; the client keeps whichever
    # finished last. When those differ, room audio is written into a socket the
    # server already closed - audible as "everyone is present, nobody can be
    # heard". The signature is one peer connecting repeatedly within a second.
    $connects = @{}
    foreach ($line in $log) {
        if ($line -match '(\d{2}:\d{2}:\d{2}).*Relay peer connected: ([A-Za-z0-9_-]{12})') {
            $key = "{0}|{1}" -f $Matches[1], $Matches[2]
            if ($connects.ContainsKey($key)) { $connects[$key]++ } else { $connects[$key] = 1 }
        }
    }
    # Three in one second is churn; two can be a legitimate reconnect racing a
    # teardown, and flagging that would make this suite cry wolf.
    $churn = @($connects.GetEnumerator() | Where-Object { $_.Value -ge 3 })
    Assert-True $Suite "$label no relay connect churn" ($churn.Count -eq 0) `
        ("{0} peer/second bucket(s) with 3+ connects, e.g. {1}" -f $churn.Count, ($churn | Select-Object -First 1 | ForEach-Object { $_.Key }))

    # ── 3. Punch registration never completing ─────────────────────────────
    #
    # `Cleaning up stale punch registration` on a loop means pairs are expiring
    # instead of firing. One or two is normal - the other peer may simply be
    # gone - but a steady stream was the padded/un-padded bucket split, where
    # the two halves of one pair could never land in the same entry.
    $stale = @($log | Select-String -Pattern 'Cleaning up stale punch registration').Count
    Assert-True $Suite "$label punch registrations complete" ($stale -lt 12) `
        "$stale stale punch registrations in 20 min (pairs are expiring, not firing)"
}
