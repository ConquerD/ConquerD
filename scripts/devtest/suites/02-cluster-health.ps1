<#
    Is the cluster one cluster?

    Version skew across members is the failure that looks like a product bug:
    peers multi-home onto all four nodes, so one member running older bytes
    produces symptoms that move around the room and never reproduce twice.
    Checking the binary hashes match is far cheaper than chasing that.
#>

$Suite = 'cluster'

$acdc = Get-ClusterStatus -HostName 'acdc'
$ac1 = Get-ClusterStatus -HostName 'ac1'

if ($null -eq $acdc -or $acdc.Count -eq 0) {
    Skip-Test $Suite 'cluster reachable' 'manager returned nothing - check SSH/secrets'
    return
}

$allLines = @($acdc) + @($ac1)

# ── Every member up ────────────────────────────────────────────────────────

foreach ($node in (Get-ClusterNodes)) {
    $label = "{0}/{1}" -f $node.HostName, $node.Instance
    $row = $allLines | Where-Object { $_ -match [regex]::Escape($label) } | Select-Object -First 1
    if ($null -eq $row) {
        Add-Result $Suite "$label present in status" 'Fail' 'no status row returned'
        continue
    }
    Assert-True $Suite "$label active" ($row -match 'active=true') $row.Trim()
}

# ── One build across the fleet ─────────────────────────────────────────────

$versions = @()
foreach ($line in $allLines) {
    if ($line -match 'nightly@([0-9a-f]+)') { $versions += $Matches[1] }
}
$distinct = @($versions | Sort-Object -Unique)

if ($distinct.Count -eq 0) {
    Skip-Test $Suite 'uniform build across cluster' 'no version strings parsed from status'
} else {
    Assert-True $Suite 'uniform build across cluster' ($distinct.Count -eq 1) `
        ("mixed builds: {0}" -f ($distinct -join ', '))
}

# ── Cluster links stable ───────────────────────────────────────────────────
#
# Links flap briefly on a deploy, which is expected. A member closing links
# repeatedly while nothing is being deployed means the roster or the firewall
# is wrong, and multi-home fan-out degrades in ways that look like audio bugs.

foreach ($node in (Get-ClusterNodes)) {
    $label = "{0}/{1}" -f $node.HostName, $node.Instance
    $log = @(Get-NodeLog -HostName $node.HostName -Instance $node.Instance -Since '15 min ago' -Grep 'cluster_link')
    # No matching lines is the *healthy* reading here, not an absent log:
    # links are only logged when they come up or go down, so a quiet window
    # means nothing flapped. (PowerShell collapses an empty array to $null on
    # assignment, which is why this re-wraps rather than testing for $null.)
    $closed = @($log | Select-String -Pattern 'Cluster link to .* closed').Count
    # Three closes in fifteen quiet minutes is thrash, not a restart.
    Assert-True $Suite "$label cluster links stable" ($closed -lt 4) `
        "$closed link closures in 15 min"
}
