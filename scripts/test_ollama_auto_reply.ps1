# Ollama-only auto-reply smoke test (no identity unlock / no supernode).
# Uses the headless client binary from rust\target-headless\.
#
# Usage:
#   .\scripts\test_ollama_auto_reply.ps1
#   .\scripts\test_ollama_auto_reply.ps1 -Profile .clientA
#   .\scripts\test_ollama_auto_reply.ps1 -Prompt "Remember my name is Sam. What is my name?"
#   .\scripts\test_ollama_auto_reply.ps1 -Model gemma3:latest
#
# Build first if needed:
#   .\build_headless.bat

param(
    [string]$Profile = ".clientA",
    [string]$Prompt = "Reply with exactly the single word: pong",
    [string]$Model = ""
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$HomeDir = Join-Path $Root $Profile
$Settings = Join-Path $HomeDir "settings.json"

$Bin = Join-Path $Root "rust\target-headless\debug\conquerd-client.exe"
if (-not (Test-Path $Bin)) {
    $Bin = Join-Path $Root "rust\target-headless\release\conquerd-client.exe"
}
if (-not (Test-Path $Bin)) {
    Write-Host "Headless binary not found. Running build_headless.bat ..."
    $build = Join-Path $Root "build_headless.bat"
    & $build debug
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $Bin = Join-Path $Root "rust\target-headless\debug\conquerd-client.exe"
}
if (-not (Test-Path $Bin)) {
    throw "Headless binary missing after build: $Bin"
}
if (-not (Test-Path $Settings)) {
    throw "Settings not found: $Settings (create profile with run_client.bat first)"
}

if ($Model) {
    $j = Get-Content $Settings -Raw | ConvertFrom-Json
    $j.ollama_model = $Model
    $j.ollama_enabled = $true
    $j.ollama_auto_respond_direct = $true
    ($j | ConvertTo-Json -Depth 8) | Set-Content -Encoding utf8 $Settings
    Write-Host "Updated $Settings model=$Model"
}

Write-Host "=== Ollama-only smoke test ==="
Write-Host "Profile: $HomeDir"
Write-Host "Binary:  $Bin"
Write-Host "Prompt:  $Prompt"
Write-Host ""

$env:CONQUERD_HOME = $HomeDir
$env:CONQUERD_KEY_DIR = $HomeDir
$env:CONQUERD_OLLAMA_ONLY = "1"
$env:CONQUERD_SIMULATE_INBOUND_CHAT = $Prompt
$env:RUST_LOG = "conquerd_client=info,warn"

& $Bin
exit $LASTEXITCODE
