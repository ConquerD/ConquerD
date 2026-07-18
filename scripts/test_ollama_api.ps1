# Preflight helper for ConquerD's Ollama integration (HTTP only, no client binary).
# Usage:
#   .\scripts\test_ollama_api.ps1
#   .\scripts\test_ollama_api.ps1 -BaseUrl http://127.0.0.1:11434
#   .\scripts\test_ollama_api.ps1 -Prompt "Say hi in one sentence" -Model gemma3:latest
#
# Related:
#   .\build_headless.bat
#   .\scripts\test_ollama_auto_reply.ps1 -Profile .clientA
#   .\run_clientA_headless.bat

param(
    [string]$BaseUrl = "http://127.0.0.1:11434",
    [string]$Model = "",
    [string]$Prompt = ""
)

$ErrorActionPreference = "Stop"
$BaseUrl = $BaseUrl.TrimEnd("/")

Write-Host "=== Ollama API probe ==="
Write-Host "Base URL: $BaseUrl"
Write-Host ""

# 1) /api/tags — same endpoint ConquerD uses for the model combo
try {
    $tags = Invoke-RestMethod -Uri "$BaseUrl/api/tags" -TimeoutSec 5
} catch {
    Write-Host "FAIL: GET $BaseUrl/api/tags"
    Write-Host "  $($_.Exception.Message)"
    Write-Host ""
    Write-Host "Is Ollama running? Try: ollama serve"
    exit 1
}

$names = @($tags.models | ForEach-Object { $_.name }) | Sort-Object
Write-Host "OK: /api/tags returned $($names.Count) model(s)"
foreach ($n in $names) {
    Write-Host "  - $n"
}

if (-not $Model) {
    $Model = $names | Select-Object -First 1
}

if (-not $Model) {
    Write-Host ""
    Write-Host "No models installed. Pull one, e.g.:"
    Write-Host "  ollama pull llama3.2"
    exit 2
}

Write-Host ""
Write-Host "Selected model: $Model"

# 2) Optional generate smoke test
if ($Prompt) {
    Write-Host ""
    Write-Host "POST /api/generate (stream=false) ..."
    $body = @{
        model  = $Model
        prompt = $Prompt
        stream = $false
        system = "You are a short debug assistant."
    } | ConvertTo-Json

    try {
        $resp = Invoke-RestMethod -Uri "$BaseUrl/api/generate" -Method Post -Body $body -ContentType "application/json" -TimeoutSec 120
        Write-Host "OK generate response:"
        Write-Host $resp.response
    } catch {
        Write-Host "FAIL: /api/generate"
        Write-Host "  $($_.Exception.Message)"
        exit 3
    }
} else {
    Write-Host ""
    Write-Host "Skip generate (pass -Prompt '...' to smoke-test chat)."
}

Write-Host ""
Write-Host "Done. ConquerD Settings → AI should list the same models."
exit 0
