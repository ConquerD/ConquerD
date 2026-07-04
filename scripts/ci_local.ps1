<#
.SYNOPSIS
    Run the same checks as .github/workflows/ci.yml before pushing.

.DESCRIPTION
    Mirrors the Linux "Rust tests" job and the Windows "Rust tests (non-Qt)"
    job: version sync, Opus weights, fmt, clippy, release-manifest self-test,
    release-mode tests, and cargo-audit (both Cargo workspaces).

    Supernode packaging (linux-x86_64, linux-aarch64, win64) is validated in
    separate CI jobs — run scripts/build_supernode.sh or build_supernode.ps1 locally
    after a successful ci_local pass when touching supernode release paths.

    Run from the repository root:

        powershell -ExecutionPolicy Bypass -File scripts/ci_local.ps1

    Faster iteration (lint only, no tests/audit):

        powershell -ExecutionPolicy Bypass -File scripts/ci_local.ps1 -SkipTests -SkipAudit

.PARAMETER RustToolchain
    Must match env.RUST_TOOLCHAIN in ci.yml (default: 1.89.0).

.PARAMETER SkipTests
    Skip cargo test --release (saves several minutes).

.PARAMETER SkipAudit
    Skip cargo-audit (requires no network / advisory-db fetch).

.PARAMETER SkipOpusFetch
    Skip scripts/fetch_opus_weights.ps1 (safe when all tar_list.txt dnn files exist).
#>

[CmdletBinding()]
param(
    [string]$RustToolchain = '1.89.0',
    [switch]$SkipTests,
    [switch]$SkipAudit,
    [switch]$SkipOpusFetch
)

$ErrorActionPreference = 'Stop'

$RepoRoot = Split-Path -Parent $PSScriptRoot
$RustDir = Join-Path $RepoRoot 'rust'
$ClientDir = Join-Path $RustDir 'conquerd-client'

function Write-Step([string]$Name) {
    Write-Host ""
    Write-Host "==> $Name" -ForegroundColor Cyan
}

function Invoke-Step([string]$Name, [scriptblock]$Body) {
    Write-Step $Name
    Push-Location $RepoRoot
    try {
        & $Body
        if ($LASTEXITCODE -and $LASTEXITCODE -ne 0) {
            throw "Step failed with exit code $LASTEXITCODE"
        }
    }
    finally {
        Pop-Location
    }
}

function Invoke-Cargo([string]$WorkingDir, [string[]]$CargoArgs) {
    Push-Location $WorkingDir
    try {
        & cargo @CargoArgs
        if ($LASTEXITCODE -ne 0) {
            throw "cargo $($CargoArgs -join ' ') failed (exit $LASTEXITCODE)"
        }
    }
    finally {
        Pop-Location
    }
}

Write-Host "ConquerD local CI (toolchain $RustToolchain)" -ForegroundColor Green
Write-Host "Repo: $RepoRoot"

Invoke-Step 'Ensure git submodules (recursive)' {
    git submodule update --init --recursive
}

Invoke-Step "Install Rust $RustToolchain (rustfmt + clippy)" {
    rustup toolchain install $RustToolchain --component rustfmt --component clippy
    $env:RUSTUP_TOOLCHAIN = $RustToolchain
}

Invoke-Step 'Verify version metadata stays in sync' {
    & (Join-Path $PSScriptRoot 'check_version_sync.ps1')
}

if (-not $SkipOpusFetch) {
    Invoke-Step 'Fetch Opus DNN model weights' {
        & (Join-Path $PSScriptRoot 'fetch_opus_weights.ps1')
    }
}

Invoke-Step 'cargo fmt --check (rust/ workspace)' {
    Invoke-Cargo $RustDir @('fmt', '--all', '--', '--check')
}

Invoke-Step 'cargo fmt --check (client workspace)' {
    Invoke-Cargo $ClientDir @('fmt', '--all', '--', '--check')
}

Invoke-Step 'cargo clippy (rust/ workspace, -D warnings)' {
    Invoke-Cargo $RustDir @('clippy', '--all', '--', '-D', 'warnings')
}

Invoke-Step 'Release manifest signer self-test' {
    Invoke-Cargo $RustDir @(
        'run', '-p', 'conquerd-installer', '--bin', 'sign-release-manifest', '--', '--self-test'
    )
}

if (-not $SkipTests) {
    Invoke-Step 'cargo test --all --release (rust/ workspace)' {
        Invoke-Cargo $RustDir @('test', '--all', '--release')
    }

    Invoke-Step 'cargo test (client workspace, headless)' {
        Invoke-Cargo $ClientDir @('test')
    }
}

Invoke-Step 'cargo clippy (client workspace, headless, -D warnings)' {
    Invoke-Cargo $ClientDir @(
        'clippy', '-p', 'conquerd-client', '--no-default-features', '--', '-D', 'warnings'
    )
}

if (-not $SkipAudit) {
    if (-not (Get-Command cargo-audit -ErrorAction SilentlyContinue)) {
        Write-Step 'Installing cargo-audit (not on PATH)'
        cargo install cargo-audit --locked
    }

    Invoke-Step 'cargo audit (rust/ workspace)' {
        Push-Location $RustDir
        try {
            cargo audit --file Cargo.lock
            if ($LASTEXITCODE -ne 0) { throw "cargo audit failed (exit $LASTEXITCODE)" }
        }
        finally {
            Pop-Location
        }
    }

    Invoke-Step 'cargo audit (client workspace)' {
        Push-Location $ClientDir
        try {
            cargo audit --file Cargo.lock
            if ($LASTEXITCODE -ne 0) { throw "cargo audit failed (exit $LASTEXITCODE)" }
        }
        finally {
            Pop-Location
        }
    }
}

Write-Host ""
Write-Host "Local CI passed." -ForegroundColor Green