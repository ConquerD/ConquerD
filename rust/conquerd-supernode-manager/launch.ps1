# Launch supernode-manager with SSH credentials loaded from secrets.local.ps1
#
# Setup (once):
#   Copy-Item secrets.local.ps1.example secrets.local.ps1
#   Edit secrets.local.ps1 and set your SSH password (globally, or per host
#   with SNM_SSH_PASSWORD_<HOST> / SNM_SSH_USER_<HOST>)
#
# Usage:
#   .\launch.ps1              # opens TUI (default)
#   .\launch.ps1 status --host acdc
#   .\launch.ps1 invite --host acdc

param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$PassthroughArgs
)

$ErrorActionPreference = "Stop"
$Root = $PSScriptRoot

function Wait-IfNeeded {
    param([int]$Code)
    if ($Code -ne 0) {
        Write-Host ""
        Read-Host "Press Enter to close"
    }
}

try {
    $secretsFile = Join-Path $Root "secrets.local.ps1"
    $secretsExample = Join-Path $Root "secrets.local.ps1.example"

    if (Test-Path $secretsFile) {
        . $secretsFile
    } elseif (Test-Path $secretsExample) {
        Write-Warning "Using secrets.local.ps1.example - copy to secrets.local.ps1 to keep passwords out of git."
        . $secretsExample
    }

    # A per-host-only secrets file is valid, so accept any SNM_SSH_PASSWORD*.
    $passwordVars = @(Get-ChildItem Env: | Where-Object { $_.Name -like 'SNM_SSH_PASSWORD*' -and $_.Value })
    if ($passwordVars.Count -eq 0) {
        throw @'
No SSH password is set.

Set one in secrets.local.ps1:
  Copy-Item secrets.local.ps1.example secrets.local.ps1

Use SNM_SSH_PASSWORD for all hosts, or SNM_SSH_PASSWORD_<HOST> per host
(<HOST> is the inventory host name, uppercased, non-alphanumerics as _).
'@
    }

    $debugExe = Join-Path $Root "target\debug\supernode-manager.exe"
    $releaseExe = Join-Path $Root "target\release\supernode-manager.exe"

    $candidates = @($debugExe, $releaseExe) | Where-Object { Test-Path $_ }
    if ($candidates.Count -eq 0) {
        throw "supernode-manager.exe not found. Run 'cargo build' first."
    }

    # Use the most recently built binary so `cargo build` (debug) is not shadowed by a stale release exe.
    $exe = $candidates | Sort-Object { (Get-Item $_).LastWriteTime } -Descending | Select-Object -First 1

    Push-Location $Root
    try {
        if ($PassthroughArgs.Count -gt 0) {
            & $exe @PassthroughArgs
        } else {
            & $exe
        }
        $code = $LASTEXITCODE
    } finally {
        Pop-Location
    }

    if ($code -ne 0) {
        throw "supernode-manager exited with code $code"
    }
    exit 0
} catch {
    Write-Host $_.Exception.Message -ForegroundColor Red
    Wait-IfNeeded 1
    exit 1
}