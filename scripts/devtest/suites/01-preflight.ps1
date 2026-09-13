<#
    Is the rig actually plugged in?

    Runs first and fast. Every later suite assumes a reachable cluster, an
    attached phone and a built headless client; without this, their failures
    read as product regressions instead of "adb is not on PATH". A red here
    means fix the environment, not the code.
#>

$Suite = 'preflight'

# ── Supernode manager ──────────────────────────────────────────────────────

$managerDir = Get-ManagerDir
Assert-True $Suite 'supernode-manager present' (Test-Path (Join-Path $managerDir 'launch.ps1')) `
    "missing $managerDir\launch.ps1"

# Presence only - never the contents. This file holds a live SSH password.
Assert-True $Suite 'manager secrets configured' (Test-Path (Join-Path $managerDir 'secrets.local.ps1')) `
    'create secrets.local.ps1 from the example (see the test-supernode skill)'

Assert-True $Suite 'manager inventory present' (Test-Path (Join-Path $managerDir 'inventory.toml')) `
    'inventory.toml is how launch.ps1 resolves acdc/ac1'

# ── Phone ──────────────────────────────────────────────────────────────────

$adb = Get-AdbPath
Assert-True $Suite 'adb available' ($null -ne $adb) 'install platform-tools or put adb on PATH'

if ($null -ne $adb) {
    $count = Get-PhoneDeviceCount
    # Exactly one: every adb call here is unqualified, so a second device makes
    # the target ambiguous rather than merely redundant.
    Assert-Equal $Suite 'exactly one device attached' 1 $count

    if ($count -eq 1) {
        $pkgs = Invoke-Adb @('shell', 'pm', 'list', 'packages', (Get-PhonePackage))
        $installed = $null -ne ($pkgs | Select-String -Pattern (Get-PhonePackage))
        Assert-True $Suite 'client installed on phone' $installed `
            "install with: gradlew assembleDebug; adb install -r app-debug.apk"
    }
}

# ── Desktop ────────────────────────────────────────────────────────────────

$headless = Get-HeadlessBinary
Assert-True $Suite 'headless client built' ($null -ne $headless) `
    'build with build_headless.bat (target-headless/)'

Assert-True $Suite 'desktop profile present' (Test-Path (Get-ClientProfileDir)) `
    'run run_client.bat once to create .clientA'
