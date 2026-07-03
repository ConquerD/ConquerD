<#
.SYNOPSIS
    Build ConquerD into a portable Windows distribution (Rust + Qt).

.DESCRIPTION
    Builds conquerd-client (--features qt-ui,webengine) and conquerd-installer with
    cargo, runs windeployqt6 to gather Qt runtime DLLs, optionally signs
    all binaries, then produces:

        dist\ConquerD\                   — portable folder (copy and run)
        dist\ConquerD-x.y.z-win64.7z    — redistributable archive

    Requirements:
      * Rust + cargo on PATH (msvc toolchain, x86_64-pc-windows-msvc)
      * Qt 6.x MSVC install — auto-detected or set QT_DIR
      * signtool.exe in PATH for code signing (optional)
      * 7z.exe for archiving (required — install via winget/choco if absent)

    Environment variables (all optional):
      QT_DIR                  — override Qt MSVC root, e.g. C:\Qt\6.8.3\msvc2022_64
      CONQUERD_DEBUG          — set to "1" to do a debug build instead of release
      CONQUERD_SIGN_THUMBPRINT  — SHA-1 cert thumbprint in Windows store
      CONQUERD_SIGN_PFX         — path to .pfx file
      CONQUERD_SIGN_PASSWORD    — password for .pfx
      CONQUERD_SIGN_TIMESTAMP   — RFC 3161 URL (default: DigiCert)
      CONQUERD_SIGN_AUTO        — set to sign with best-available cert

.USAGE
    .\build_win64.ps1
    $env:QT_DIR="C:\Qt\6.8.3\msvc2022_64"; .\build_win64.ps1

#>

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$ROOT      = $PSScriptRoot
$RUST_DIR  = Join-Path $ROOT "rust"
$CLIENT_DIR = Join-Path $RUST_DIR "conquerd-client"
$QML_DIR   = Join-Path $CLIENT_DIR "qml"
$DIST      = Join-Path $ROOT "dist"
$BUNDLE    = Join-Path $DIST "ConquerD"

$PROFILE_NAME = if ($env:CONQUERD_DEBUG -eq "1") { "debug" } else { "release" }
[string[]]$CARGO_ARGS = if ($PROFILE_NAME -eq "release") { @("--release") } else { @() }

# Debug console toggle: set CONQUERD_DEBUG_CONSOLE=1 to keep the terminal window
# attached (enables the `console` Cargo feature which removes windows_subsystem = "windows").
$_features = "qt-ui"
if ($env:CONQUERD_DEBUG_CONSOLE -eq "1") {
    $_features += ",console"
    Write-Host "    [debug] Console window enabled (CONQUERD_DEBUG_CONSOLE=1)"
}
# ── Version ──────────────────────────────────────────────────────────────────
$_cargoToml = Join-Path $RUST_DIR "conquerd-client\Cargo.toml"
$_vLine = Select-String -Path $_cargoToml -Pattern '^version\s*=\s*"([^"]+)"' |
    Select-Object -First 1
if (-not $_vLine) { Write-Error "Could not parse version from $_cargoToml" }
$VERSION = $_vLine.Matches.Groups[1].Value
Write-Host "==> ConquerD v$VERSION  (profile: $PROFILE_NAME)"

# ── Locate Qt ─────────────────────────────────────────────────────────────────
$QT_ROOT = $null
if ($env:QT_DIR -and (Test-Path (Join-Path $env:QT_DIR "bin\windeployqt6.exe"))) {
    $QT_ROOT = $env:QT_DIR
}
if (-not $QT_ROOT) {
    foreach ($candidate in @(
        "C:\Qt\6.8.3\msvc2022_64",
        "C:\Qt\6.8.2\msvc2022_64",
        "C:\Qt\6.7.3\msvc2022_64",
        "C:\Qt\6.7.2\msvc2022_64"
    )) {
        if (Test-Path (Join-Path $candidate "bin\windeployqt6.exe")) {
            $QT_ROOT = $candidate
            break
        }
    }
}
if (-not $QT_ROOT) {
    Write-Error @"
Qt 6 MSVC install not found.
Set QT_DIR to the msvc2022_64 root (e.g. C:\Qt\6.8.3\msvc2022_64) or install
Qt 6 via the online installer at https://www.qt.io/download.
"@
}
Write-Host "    Qt root : $QT_ROOT"
$env:PATH = "$QT_ROOT\bin;$env:PATH"
$env:QMAKE = Join-Path $QT_ROOT "bin\qmake6.exe"
Write-Host "    QMAKE   : $env:QMAKE"
$WINDEPLOYQT = Join-Path $QT_ROOT "bin\windeployqt6.exe"

function Resolve-VcInstallDir {
    if ($env:VCINSTALLDIR -and (Test-Path $env:VCINSTALLDIR)) {
        return $env:VCINSTALLDIR
    }

    $vswhereCandidates = @(
        (Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"),
        (Join-Path $env:ProgramFiles "Microsoft Visual Studio\Installer\vswhere.exe")
    )
    foreach ($vswhere in $vswhereCandidates) {
        if (-not (Test-Path $vswhere)) { continue }
        $installationPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>$null
        if ($LASTEXITCODE -eq 0 -and $installationPath) {
            $vcDir = Join-Path $installationPath "VC"
            if (Test-Path $vcDir) { return $vcDir }
        }
    }

    return $null
}

# Include the Qt WebEngine (Chromium) scheme handler for the in-app node portal
# (conquerd:// custom scheme). Auto-detected from the Qt install. Override with
# CONQUERD_NO_WEBENGINE=1 to force-disable (e.g. Qt WebEngine not installed).
$_weProbe = Join-Path $QT_ROOT "include\QtWebEngineCore\QWebEngineProfile.h"
if ($env:CONQUERD_NO_WEBENGINE -ne "1" -and (Test-Path $_weProbe)) {
    $_features += ",webengine"
    Write-Host "    [web] Qt WebEngine portal enabled"
} elseif ($env:CONQUERD_NO_WEBENGINE -eq "1") {
    Write-Host "    [web] Qt WebEngine portal disabled (CONQUERD_NO_WEBENGINE=1)"
} else {
    Write-Host "    [web] Qt WebEngine NOT found — portal disabled" -ForegroundColor Yellow
    Write-Host "         Install via Qt Maintenance Tool: Qt 6.x > Additional Libraries > Qt WebEngine" -ForegroundColor Yellow
    if ($env:CONQUERD_BUILD_ID) {
        Write-Error @"
CI/release builds require Qt WebEngine for the in-app supernode portal.
Install the module, e.g.:
  pwsh scripts/install_qt_windows.ps1
"@
    }
}

# ── Build conquerd-client (Qt UI) ─────────────────────────────────────────────
Write-Host "`n==> Building conquerd-client ($PROFILE_NAME)..."
# conquerd-client is its own workspace root (rust/conquerd-client/) so that the
# Windows-local cxx-qt patch does not affect server-side builds on Linux.
# Wrap in try/catch to absorb the spurious NativeCommandError PS 7+ raises
# when any native process writes to stderr, even on success.
Push-Location $CLIENT_DIR
$_prevPref = $ErrorActionPreference; $ErrorActionPreference = "Continue"
& cargo build @CARGO_ARGS --features $_features
$_clientExit = $LASTEXITCODE
$ErrorActionPreference = $_prevPref
Pop-Location
if ($_clientExit -ne 0) { Write-Error "cargo build conquerd-client failed (exit $_clientExit)" }

function Get-ClientSourceHash {
    $outGlob = Join-Path $RUST_DIR "target\$PROFILE_NAME\build\conquerd-client-*\output"
    $outFile = Get-ChildItem -Path $outGlob -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $outFile) { return $null }
    foreach ($line in Get-Content $outFile.FullName) {
        if ($line -match 'cargo:rustc-env=CONQUERD_SOURCE_HASH=(.+)') {
            return $Matches[1]
        }
    }
    return $null
}

# Optional second pass: bake CONQUERD_RELEASE_PROOF for official CI/local release builds.
if ($env:CONQUERD_RELEASE_SIGN_KEY -and (Test-Path $env:CONQUERD_RELEASE_SIGN_KEY)) {
    $sourceHash = Get-ClientSourceHash
    $buildId = if ($env:CONQUERD_BUILD_ID) {
        $env:CONQUERD_BUILD_ID
    } else {
        $tag = (& git -C $ROOT describe --tags --exact-match HEAD 2>$null)
        $sha = (& git -C $ROOT rev-parse --short=12 HEAD 2>$null)
        if ($tag) { "release-$($tag -replace '^v','')-$sha" } else { $sha }
    }
    Write-Host "`n==> Signing release build claim (build_id=$buildId)..."
    $claimArgs = @(
        "run", "-p", "conquerd-installer", "--manifest-path", (Join-Path $RUST_DIR "Cargo.toml"),
        "--bin", "sign-release-manifest", "--",
        "--sign-build-claim",
        "--private-key", $env:CONQUERD_RELEASE_SIGN_KEY,
        "--build-id", $buildId,
        "--claim-version", $VERSION
    )
    if ($sourceHash) { $claimArgs += @("--source-hash", $sourceHash) }
    Push-Location $ROOT
    $_prevPrefClaim = $ErrorActionPreference; $ErrorActionPreference = "Continue"
    $proof = & cargo @claimArgs 2>$null
    $_claimExit = $LASTEXITCODE
    $ErrorActionPreference = $_prevPrefClaim
    Pop-Location
    if ($_claimExit -ne 0 -or -not $proof) {
        Write-Error "Failed to sign release build claim (exit $_claimExit)"
    }
    $env:CONQUERD_RELEASE_PROOF = $proof.Trim()
    Write-Host "    Rebuilding conquerd-client with CONQUERD_RELEASE_PROOF..."
    Push-Location $CLIENT_DIR
    $_prevPref4 = $ErrorActionPreference; $ErrorActionPreference = "Continue"
    & cargo build @CARGO_ARGS --features $_features
    $_clientExit2 = $LASTEXITCODE
    $ErrorActionPreference = $_prevPref4
    Pop-Location
    if ($_clientExit2 -ne 0) { Write-Error "cargo rebuild with release proof failed (exit $_clientExit2)" }
}

$CLIENT_EXE = Join-Path $RUST_DIR "target\$PROFILE_NAME\conquerd-client.exe"
if (-not (Test-Path $CLIENT_EXE)) {
    Write-Error "conquerd-client.exe not found at $CLIENT_EXE"
}

# ── Build conquerd-installer ──────────────────────────────────────────────────
Write-Host "`n==> Building conquerd-installer ($PROFILE_NAME)..."
Push-Location $RUST_DIR
$_prevPref2 = $ErrorActionPreference; $ErrorActionPreference = "Continue"
& cargo build @CARGO_ARGS -p conquerd-installer
$_installerExit = $LASTEXITCODE
$ErrorActionPreference = $_prevPref2
Pop-Location
if ($_installerExit -ne 0) { Write-Error "cargo build conquerd-installer failed (exit $_installerExit)" }

$INSTALLER_EXE = Join-Path $RUST_DIR "target\$PROFILE_NAME\conquerd-installer.exe"
if (-not (Test-Path $INSTALLER_EXE)) {
    Write-Error "conquerd-installer.exe not found at $INSTALLER_EXE"
}

# ── Prepare dist folder ───────────────────────────────────────────────────────
Write-Host "`n==> Preparing dist\ConquerD\..."
if (Test-Path $BUNDLE) {
    cmd /c "rmdir /s /q `"$BUNDLE`""
    if (Test-Path $BUNDLE) {
        Write-Error "Failed to remove $BUNDLE -- close any running ConquerD processes and retry."
    }
}
New-Item -ItemType Directory -Path $BUNDLE | Out-Null

$BUNDLE_EXE       = Join-Path $BUNDLE "ConquerD.exe"
$BUNDLE_INSTALLER = Join-Path $BUNDLE "conquerd-installer.exe"
Copy-Item $CLIENT_EXE    $BUNDLE_EXE
Copy-Item $INSTALLER_EXE $BUNDLE_INSTALLER
Write-Host "    Copied binaries"

# ── windeployqt6 ─────────────────────────────────────────────────────────────
Write-Host "`n==> Running windeployqt6..."
$vcInstallDir = Resolve-VcInstallDir
$compilerRuntimeArg = if ($vcInstallDir) {
    $env:VCINSTALLDIR = $vcInstallDir
    Write-Host "    VC runtime deployment enabled: $vcInstallDir"
    "--compiler-runtime"
} else {
    Write-Host "    VC runtime deployment skipped (Visual C++ tools not found)"
    "--no-compiler-runtime"
}
$_prevPref3 = $ErrorActionPreference; $ErrorActionPreference = "Continue"
$windeployArgs = @(
    "--qmldir", $QML_DIR,
    "--no-translations",
    "--skip-plugin-types", "position",
    $compilerRuntimeArg,
    $BUNDLE_EXE
)
& $WINDEPLOYQT @windeployArgs
$_wdqtExit = $LASTEXITCODE
$ErrorActionPreference = $_prevPref3
if ($_wdqtExit -ne 0) { Write-Error "windeployqt6 failed (exit code $_wdqtExit)" }
Write-Host "    Qt runtime deployed"

if ($_features -like "*webengine*") {
    $qtWebEngineResources = Join-Path $QT_ROOT "resources"
    $bundleWebEngineResources = Join-Path $BUNDLE "resources"
    if (Test-Path (Join-Path $qtWebEngineResources "qtwebengine_resources.pak")) {
        Copy-Item $qtWebEngineResources $bundleWebEngineResources -Recurse -Force
        Write-Host "    Qt WebEngine resources deployed"
    } else {
        Write-Warning "Qt WebEngine resources not found under $qtWebEngineResources"
    }
    $qtLocales = Join-Path $QT_ROOT "translations\qtwebengine_locales"
    $bundleLocales = Join-Path $BUNDLE "locales"
    if (Test-Path $qtLocales) {
        Copy-Item $qtLocales $bundleLocales -Recurse -Force
        Write-Host "    Qt WebEngine locales deployed"
    } else {
        # aqt installs Chromium locale packs under resources/ on some Qt builds.
        $altLocales = Join-Path $QT_ROOT "resources\locales"
        if (Test-Path $altLocales) {
            Copy-Item $altLocales $bundleLocales -Recurse -Force
            Write-Host "    Qt WebEngine locales deployed (from resources/locales)"
        }
    }
}

# ── Code-sign binaries (optional) ─────────────────────────────────────────────
#
#   CONQUERD_SIGN_THUMBPRINT  -- SHA-1 thumbprint of a cert in the Windows Store
#   CONQUERD_SIGN_PFX         -- path to a .pfx file (OV cert, local builds)
#   CONQUERD_SIGN_PASSWORD    -- password for the .pfx file
#   CONQUERD_SIGN_TIMESTAMP   -- RFC 3161 URL (default: DigiCert)
#
# If none are set the signing step is skipped (development builds).

$_signtool     = Get-Command "signtool" -ErrorAction SilentlyContinue
$_signThumb    = $env:CONQUERD_SIGN_THUMBPRINT
$_signPfx      = $env:CONQUERD_SIGN_PFX
$_signPassword = $env:CONQUERD_SIGN_PASSWORD
$_timestampUrl = if ($env:CONQUERD_SIGN_TIMESTAMP) { $env:CONQUERD_SIGN_TIMESTAMP } `
                 else { "http://timestamp.digicert.com" }

function Invoke-SignBinary ([string]$Path) {
    $signArgs = @("sign", "/fd", "SHA256", "/tr", $_timestampUrl, "/td", "SHA256")
    if ($_signThumb) {
        $signArgs += @("/sha1", $_signThumb)
    } elseif ($_signPfx) {
        $signArgs += @("/f", $_signPfx)
        if ($_signPassword) { $signArgs += @("/p", $_signPassword) }
    } else {
        $signArgs += "/a"
    }
    $signArgs += $Path
    & $_signtool.Source @signArgs
    if ($LASTEXITCODE -ne 0) { Write-Error "signtool failed for $Path" }
}

$_doSign = $_signtool -and ($_signThumb -or $_signPfx -or $env:CONQUERD_SIGN_AUTO)
if ($_doSign) {
    Write-Host "`n==> Code-signing binaries..."
    foreach ($bin in @($BUNDLE_EXE, $BUNDLE_INSTALLER)) {
        Write-Host "    Signing: $(Split-Path $bin -Leaf)"
        Invoke-SignBinary $bin
    }
    Write-Host "    Code signing complete."
} elseif (-not $_signtool) {
    Write-Host "`n    [sign] signtool.exe not found -- install Windows SDK to enable code signing"
} else {
    Write-Host "`n    [sign] Skipped -- set CONQUERD_SIGN_THUMBPRINT or CONQUERD_SIGN_PFX to sign"
}

# ── Copy installer to dist\ root (run-alongside-archive entry point) ──────────
$DIST_INSTALLER = Join-Path $DIST "conquerd-installer.exe"
Copy-Item $INSTALLER_EXE $DIST_INSTALLER -Force
Write-Host "`n    Copied conquerd-installer.exe to dist\ (detect-archive entry point)"

# ── Create .7z archive ────────────────────────────────────────────────────────
# The installer downloads this 7z from GitHub Releases for updates.
# It contains the full self-contained portable `ConquerD/` folder (exe + Qt runtime + QML + resources).
$archiveName = "ConquerD-${VERSION}-win64.7z"
$archivePath = Join-Path $DIST $archiveName

$sevenZip = Get-Command "7z" -ErrorAction SilentlyContinue
if (-not $sevenZip) {
    # Common locations on GitHub runners and dev machines
    $candidates = @(
        "C:\Program Files\7-Zip\7z.exe",
        "C:\Program Files (x86)\7-Zip\7z.exe",
        "${env:ProgramFiles}\7-Zip\7z.exe"
    )
    foreach ($c in $candidates) {
        if (Test-Path $c) {
            $sevenZip = @{ Source = $c }
            break
        }
    }
}
if (-not $sevenZip) {
    Write-Error @"
7z.exe not found. Install 7-Zip before building release archives:
  winget install --id 7zip.7zip
  choco install 7zip -y
Release archives must be non-solid (-ms=off) so the installer's embedded sevenz-rust decoder can extract every file.
"@
}
Write-Host "`n==> Creating 7z archive with 7-Zip (non-solid, installer-compatible)..."
& $sevenZip.Source a -t7z -mx=9 -ms=off $archivePath "$BUNDLE\*" | Out-Null
if ($LASTEXITCODE -ne 0) { Write-Error "7z failed to create archive" }

if (Test-Path $archivePath) {
    Write-Host "    Archive ready: $archivePath"
}

# ── Report results ────────────────────────────────────────────────────────────
Write-Host ""
Write-Host "==> Build successful!" -ForegroundColor Green

$dirSize = [math]::Round(
    (Get-ChildItem $BUNDLE -Recurse | Measure-Object -Property Length -Sum).Sum / 1MB, 0)
Write-Host "    Launcher  : $BUNDLE_EXE"
Write-Host "    Installer : $BUNDLE_INSTALLER"
Write-Host "    Folder    : $BUNDLE\"
Write-Host "    Size      : $dirSize MB"

if (Test-Path $archivePath) {
    $archiveSize = [math]::Round((Get-Item $archivePath).Length / 1MB, 1)
    $sha     = (Get-FileHash $archivePath -Algorithm SHA256).Hash.ToLower()
    $shaFile = Join-Path $DIST "$archiveName.sha256"
    "$sha  $archiveName" | Set-Content $shaFile -NoNewline
    Write-Host "    Archive   : $archivePath ($archiveSize MB)"
    Write-Host "    SHA-256   : $sha"
}

Write-Host ""
Write-Host "    ConquerD v${VERSION}" -ForegroundColor Cyan
