# Launch supernode-manager with SNM_SSH_PASSWORD loaded from secrets.local.ps1
#
# Setup (once):
#   Copy-Item secrets.local.ps1.example secrets.local.ps1
#   Edit secrets.local.ps1 and set your SSH password
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

    if (-not $env:SNM_SSH_PASSWORD) {
        throw @'
SNM_SSH_PASSWORD is not set.

Set your password in secrets.local.ps1:
  Copy-Item secrets.local.ps1.example secrets.local.ps1
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