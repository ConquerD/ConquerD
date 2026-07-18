@echo off
setlocal EnableExtensions

:: ---------------------------------------------------------------------------
:: Ollama assistant debug client
::
:: Same launch path as run_client.bat, but uses a dedicated profile
:: (.clientOllama), seeds Ollama-friendly settings, enables debug logging,
:: and pre-flights the local Ollama HTTP API so model-list issues are easy
:: to distinguish from app bugs.
:: ---------------------------------------------------------------------------

set "ROOT=%~dp0"
set "CONQUERD_HOME=%ROOT%.clientOllama"
set "CONQUERD_KEY_DIR=%CONQUERD_HOME%"
set "LEGACY_HOME=%ROOT%.clientOllama_home"
set "LEGACY_PROFILE_LINK=%LEGACY_HOME%\.conquerd"
set "SETTINGS=%CONQUERD_HOME%\settings.json"
set "LOG_DIR=%CONQUERD_HOME%\logs"
set "LOG_FILE=%LOG_DIR%\conquerd-client.log"

:: Prefer a freshly built binary (release, then debug), then the packaged dist client.
set "RELEASE_BIN=%ROOT%rust\target\release\conquerd-client.exe"
set "DEBUG_BIN=%ROOT%rust\target\debug\conquerd-client.exe"
set "DIST_BIN=%ROOT%dist\ConquerD\ConquerD.exe"
set "BINARY="
set "BINARY_KIND="
set "USE_DEBUG=0"
if exist "%RELEASE_BIN%" (
    set "BINARY=%RELEASE_BIN%"
    set "BINARY_KIND=release (rust\target\release)"
    set "USE_DEBUG=1"
) else if exist "%DEBUG_BIN%" (
    set "BINARY=%DEBUG_BIN%"
    set "BINARY_KIND=debug (rust\target\debug)"
    set "USE_DEBUG=1"
) else if exist "%DIST_BIN%" (
    set "BINARY=%DIST_BIN%"
    set "BINARY_KIND=dist package"
    set "USE_DEBUG=0"
)

:: Prefer verbose client logs for AI debugging (overrides settings when set).
if not defined RUST_LOG set "RUST_LOG=conquerd_client=debug,warn"

:: HiDPI: same notes as run_client.bat
::   set QT_SCALE_FACTOR=1.0
::   set QT_SCALE_FACTOR=0.85

if not defined BINARY (
    echo ConquerD client binary not found.
    echo Tried:
    echo   %DEBUG_BIN%
    echo   %DIST_BIN%
    echo.
    echo Build with:
    echo   cd rust\conquerd-client
    echo   cargo build -p conquerd-client --features "qt-ui,webengine,console"
    echo Or package via build_win64.ps1 so dist\ConquerD\ConquerD.exe exists.
    exit /b 1
)

:: Debug builds need Qt on PATH; dist is self-contained.
if "%USE_DEBUG%"=="1" (
    if defined QT_DIR (
        set "PATH=%QT_DIR%\bin;%PATH%"
    ) else if exist "C:\Qt\6.8.3\msvc2022_64\bin\NUL" (
        set "PATH=C:\Qt\6.8.3\msvc2022_64\bin;%PATH%"
        set "QT_DIR=C:\Qt\6.8.3\msvc2022_64"
    )
)

if not exist "%CONQUERD_HOME%\NUL" mkdir "%CONQUERD_HOME%"
if not exist "%LOG_DIR%\NUL" mkdir "%LOG_DIR%"
if not exist "%LEGACY_HOME%\NUL" mkdir "%LEGACY_HOME%"
if not exist "%LEGACY_PROFILE_LINK%\NUL" (
    mklink /J "%LEGACY_PROFILE_LINK%" "%CONQUERD_HOME%" >nul
)

:: Seed a settings file on first run so Ollama is enabled without hunting
:: through the UI.  Does not overwrite an existing profile.
if not exist "%SETTINGS%" (
    echo Creating first-run Ollama debug settings at:
    echo   %SETTINGS%
    > "%SETTINGS%" (
        echo {
        echo   "notifications_enabled": true,
        echo   "auto_connect": false,
        echo   "direct_p2p_enabled": false,
        echo   "direct_p2p_port": 61055,
        echo   "start_minimized": false,
        echo   "minimize_to_tray": false,
        echo   "push_to_talk": false,
        echo   "noise_suppression": true,
        echo   "voice_activation": false,
        echo   "jitter_buffer_depth": 3,
        echo   "input_volume": 100,
        echo   "output_volume": 100,
        echo   "voice_bitrate": "ultra",
        echo   "ptt_key": "space",
        echo   "audio_input_device": "",
        echo   "audio_output_device": "",
        echo   "local_handle": "OllamaDebug",
        echo   "update_check_enabled": false,
        echo   "relay_port": 0,
        echo   "ollama_enabled": true,
        echo   "ollama_base_url": "http://127.0.0.1:11434",
        echo   "ollama_model": "llama3.2:latest",
        echo   "ollama_system_prompt": "You are a helpful ConquerD debug assistant. Keep answers short.",
        echo   "ollama_auto_respond_direct": false,
        echo   "ollama_auto_respond_room": false,
        echo   "noise_strength": "moderate",
        echo   "theme": "dark",
        echo   "relay_allow_gated": true,
        echo   "relay_auto_renew": true,
        echo   "upnp_enabled": true,
        echo   "attestation_policy": "warn",
        echo   "youtube_preview_enabled": false,
        echo   "youtube_inline_ack": false,
        echo   "onboarding_complete": true,
        echo   "window_width": 1200,
        echo   "window_height": 800,
        echo   "avatar_config_json": "",
        echo   "debug_logging": true
        echo }
    )
)

echo.
echo === ConquerD Ollama test client ===
echo Profile:  %CONQUERD_HOME%
echo Binary:   %BINARY%
echo Kind:     %BINARY_KIND%
echo Settings: %SETTINGS%
echo Log:      %LOG_FILE%
echo RUST_LOG: %RUST_LOG%
echo.

:: Preflight: can we reach Ollama's model list?
echo Checking Ollama at http://127.0.0.1:11434/api/tags ...
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "try {" ^
  "  $r = Invoke-RestMethod -Uri 'http://127.0.0.1:11434/api/tags' -TimeoutSec 4;" ^
  "  $n = @($r.models).Count;" ^
  "  Write-Host ('OK — Ollama reported ' + $n + ' model(s):');" ^
  "  @($r.models) | Select-Object -First 12 -ExpandProperty name | ForEach-Object { Write-Host ('  - ' + $_) };" ^
  "  if ($n -gt 12) { Write-Host ('  ... and ' + ($n - 12) + ' more') };" ^
  "  exit 0" ^
  "} catch {" ^
  "  Write-Host 'FAIL — cannot reach Ollama:';" ^
  "  Write-Host $_.Exception.Message;" ^
  "  Write-Host '';" ^
  "  Write-Host 'Start Ollama (ollama serve) and pull a model, e.g.:';" ^
  "  Write-Host '  ollama pull llama3.2';" ^
  "  exit 1" ^
  "}"
if errorlevel 1 (
    echo.
    echo Continuing to launch the client anyway so you can still debug the UI.
    echo.
) else (
    echo.
)

set "HOME=%LEGACY_HOME%"

echo Launching ConquerD...
echo Tips:
echo   - Settings → AI → Model combo should populate after refresh.
echo   - Chat AI needs ollama_enabled=true at startup ^(this profile seeds that^).
echo   - Watch the log for: [ollama] ListModels / [plugins] x.ollama.v1
echo.

"%BINARY%" %*
set "EXITCODE=%ERRORLEVEL%"

echo.
echo Client exited with code %EXITCODE%.
if exist "%LOG_FILE%" (
    echo.
    echo --- last Ollama-related log lines ---
    powershell -NoProfile -ExecutionPolicy Bypass -Command ^
      "Select-String -Path '%LOG_FILE%' -Pattern 'ollama|plugins|ListModels' -CaseSensitive:$false | Select-Object -Last 30 | ForEach-Object { $_.Line }"
)
exit /b %EXITCODE%
