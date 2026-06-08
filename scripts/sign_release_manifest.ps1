<#
.SYNOPSIS
  Thin wrapper around the sign-release-manifest binary for easy use on Windows.

.DESCRIPTION
  Builds/runs the Rust signer on demand and forwards arguments.
  Supports the common flows:
    - Generate unsigned skeleton
    - Sign a (edited) manifest with your private seed
    - Verify a signed manifest against the key in the source tree
    - Self-test the canonical+sign+verify pipeline

  Private key can be:
  - 32-byte raw seed file
  - 64-hex file
  - Direct PEM from openssl genpkey (supported natively now):
      C:\Users\AWOL\release-signer-private.pem  (or any path to the .pem)

  The project uses a Cargo workspace under the `rust/` subdirectory.

.EXAMPLE
  # From repo root
  .\scripts\sign_release_manifest.ps1 --generate-unsigned

  # Sign after editing
  .\scripts\sign_release_manifest.ps1 --private-key C:\secure\seed.bin -i releases_manifest.json -o releases_manifest.json

  # Verify
  .\scripts\sign_release_manifest.ps1 --verify -i releases_manifest.json

  # Self test
  .\scripts\sign_release_manifest.ps1 --self-test
#>
[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments=$true)]
    [string[]]$RemainingArgs
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $repoRoot

try {
    Write-Host "==> sign-release-manifest wrapper" -ForegroundColor Cyan

    # The workspace root is rust/Cargo.toml
    & cargo run -p conquerd-installer --manifest-path rust/Cargo.toml --bin sign-release-manifest -- @RemainingArgs

    if ($LASTEXITCODE -ne 0) {
        throw "sign-release-manifest failed with exit code $LASTEXITCODE"
    }
}
finally {
    Pop-Location
}
