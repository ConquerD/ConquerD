<#
.SYNOPSIS
    Build and package conquerd-supernode for Windows x86_64.

.DESCRIPTION
    Produces:
      dist\conquerd-supernode-X.X.X-win64.zip
      dist\conquerd-supernode-X.X.X-win64.zip.sha256

    Run from the repository root:

        powershell -ExecutionPolicy Bypass -File scripts\build_supernode.ps1

        $env:CONQUERD_RELEASE = '1'
        $env:CONQUERD_BUILD_ID = 'release-1.0.0-abc123'
        powershell -ExecutionPolicy Bypass -File scripts\build_supernode.ps1
#>

[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$Root = Split-Path -Parent $PSScriptRoot
$RustDir = Join-Path $Root 'rust'
$Dist = Join-Path $Root 'dist'

$Version = (Select-String -Path (Join-Path $RustDir 'conquerd-supernode\Cargo.toml') -Pattern '^version\s*=' | Select-Object -First 1).Line -replace '.*"(.*)".*', '$1'
$Platform = 'win64'

$Profile = 'debug'
$CargoArgs = @('build', '-p', 'conquerd-supernode')
if ($env:CONQUERD_RELEASE -eq '1' -or $env:CONQUERD_DEBUG -ne '1') {
    $Profile = 'release'
    $CargoArgs += '--release'
}

Write-Host "==> Building conquerd-supernode v$Version for $Platform (profile: $Profile)"

Push-Location $RustDir
try {
    & cargo @CargoArgs
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed (exit $LASTEXITCODE)"
    }
}
finally {
    Pop-Location
}

$Binary = Join-Path $RustDir "target\$Profile\conquerd-supernode.exe"
if (-not (Test-Path $Binary)) {
    throw "Expected binary at $Binary"
}

$StagingName = "conquerd-supernode-$Version-$Platform"
$Staging = Join-Path $Dist $StagingName
$Archive = Join-Path $Dist "$StagingName.zip"

New-Item -ItemType Directory -Force -Path $Dist | Out-Null
if (Test-Path $Staging) {
    Remove-Item -Recurse -Force $Staging
}
New-Item -ItemType Directory -Force -Path $Staging | Out-Null
Copy-Item $Binary (Join-Path $Staging 'conquerd-supernode.exe') -Force

if (Test-Path $Archive) {
    Remove-Item -Force $Archive
}
Compress-Archive -Path $Staging -DestinationPath $Archive -CompressionLevel Optimal
Remove-Item -Recurse -Force $Staging

$Hash = (Get-FileHash -Path $Archive -Algorithm SHA256).Hash.ToLower()
Set-Content -Path "$Archive.sha256" -Value "$Hash  $(Split-Path $Archive -Leaf)" -NoNewline

Write-Host "==> Package ready: $Archive"
Get-Content "$Archive.sha256"