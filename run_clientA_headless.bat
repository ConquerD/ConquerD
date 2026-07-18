@echo off
setlocal EnableExtensions EnableDelayedExpansion

:: ---------------------------------------------------------------------------
:: ClientA headless bot (Bobert) — full peer + Ollama auto-reply
::
:: Build first (if needed):
::   build_headless.bat
::   build_headless.bat release
::
:: Identity is ClientA (.clientA), same as run_client.bat.
:: Your other-peer GUI on this machine is fine. Do not also run Bobert GUI.
:: ---------------------------------------------------------------------------

set "ROOT=%~dp0"
set "CONQUERD_HOME=%ROOT%.clientA"
set "CONQUERD_KEY_DIR=%CONQUERD_HOME%"
set "LEGACY_HOME=%ROOT%.clientA_home"
set "LEGACY_PROFILE_LINK=%LEGACY_HOME%\.conquerd"
set "SETTINGS=%CONQUERD_HOME%\settings.json"
set "LOG_DIR=%CONQUERD_HOME%\logs"
set "LOG_FILE=%LOG_DIR%\conquerd-client.log"
set "PASS_FILE=%CONQUERD_HOME%\passphrase.local"

set "HL_DEBUG=%ROOT%rust\target-headless\debug\conquerd-client.exe"
set "HL_RELEASE=%ROOT%rust\target-headless\release\conquerd-client.exe"
set "BINARY="
set "BINARY_KIND="

if /I "%~1"=="release" (
    if exist "%HL_RELEASE%" (
        set "BINARY=%HL_RELEASE%"
        set "BINARY_KIND=release"
    )
) else if exist "%HL_DEBUG%" (
    set "BINARY=%HL_DEBUG%"
    set "BINARY_KIND=debug"
) else if exist "%HL_RELEASE%" (
    set "BINARY=%HL_RELEASE%"
    set "BINARY_KIND=release"
)

if not defined RUST_LOG set "RUST_LOG=conquerd_client=info,warn"

if not exist "%CONQUERD_HOME%\NUL" mkdir "%CONQUERD_HOME%"
if not exist "%LOG_DIR%\NUL" mkdir "%LOG_DIR%"
if not exist "%LEGACY_HOME%\NUL" mkdir "%LEGACY_HOME%"
if not exist "%LEGACY_PROFILE_LINK%\NUL" (
    mklink /J "%LEGACY_PROFILE_LINK%" "%CONQUERD_HOME%" >nul 2>nul
)

tasklist /FI "IMAGENAME eq conquerd-client.exe" 2>nul | find /I "conquerd-client.exe" >nul
if not errorlevel 1 (
    echo.
    echo ERROR: conquerd-client.exe is already running.
    echo Stop the other headless instance first.
    echo.
    goto :fail
)

tasklist /FI "IMAGENAME eq ConquerD.exe" 2>nul | find /I "ConquerD.exe" >nul
if not errorlevel 1 (
    echo.
    echo NOTE: ConquerD.exe GUI is running.
    echo   OK if that is your normal/other profile ^(not Bobert / run_client.bat^).
    echo.
)

if not defined CONQUERD_PASSPHRASE (
    if exist "%PASS_FILE%" set "CONQUERD_PASSPHRASE_FILE=%PASS_FILE%"
)

if not exist "%SETTINGS%" (
    echo ERROR: No settings at %SETTINGS%
    echo Create the ClientA profile once with run_client.bat.
    goto :fail
)

powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "$p = '%SETTINGS%';" ^
  "$j = Get-Content -Raw $p | ConvertFrom-Json;" ^
  "$changed = $false;" ^
  "if (-not $j.PSObject.Properties['ollama_enabled'] -or -not $j.ollama_enabled) { $j.ollama_enabled = $true; $changed = $true };" ^
  "if (-not $j.PSObject.Properties['ollama_auto_respond_direct'] -or -not $j.ollama_auto_respond_direct) { $j.ollama_auto_respond_direct = $true; $changed = $true };" ^
  "if (-not $j.PSObject.Properties['ollama_auto_respond_room'] -or -not $j.ollama_auto_respond_room) { $j.ollama_auto_respond_room = $true; $changed = $true };" ^
  "if (-not $j.ollama_base_url) { $j.ollama_base_url = 'http://127.0.0.1:11434'; $changed = $true };" ^
  "if (-not $j.ollama_model) { $j.ollama_model = 'gemma3:latest'; $changed = $true };" ^
  "if ($changed) { ($j | ConvertTo-Json -Depth 8) | Set-Content -Encoding utf8 $p; Write-Host 'Updated Ollama flags in settings.json' };" ^
  "Write-Host ('Ollama: enabled=' + $j.ollama_enabled + ' model=' + $j.ollama_model + ' auto_direct=' + $j.ollama_auto_respond_direct + ' auto_room=' + $j.ollama_auto_respond_room)"

if not defined BINARY (
    echo.
    echo No headless binary under rust\target-headless\. Building...
    call "%ROOT%build_headless.bat" debug
    if errorlevel 1 goto :fail
    set "BINARY=%HL_DEBUG%"
    set "BINARY_KIND=debug (just built)"
)

echo.
echo === ClientA headless bot ^(Bobert^) ===
echo Profile:  %CONQUERD_HOME%
echo Binary:   %BINARY%  [%BINARY_KIND%]
echo Log:      %LOG_FILE%
if defined CONQUERD_PASSPHRASE (
    echo Unlock:   CONQUERD_PASSPHRASE
) else if defined CONQUERD_PASSPHRASE_FILE (
    echo Unlock:   %CONQUERD_PASSPHRASE_FILE%
) else (
    echo Unlock:   passphrase prompt / OS keyring
)
echo.
echo Chat from your other peer. Multi-turn chat memory is per room/DM until restart.
echo.

powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "try {" ^
  "  $u = (Get-Content -Raw '%SETTINGS%' | ConvertFrom-Json).ollama_base_url;" ^
  "  if (-not $u) { $u = 'http://127.0.0.1:11434' }; $u = $u.TrimEnd('/');" ^
  "  $r = Invoke-RestMethod -Uri ($u + '/api/tags') -TimeoutSec 4;" ^
  "  Write-Host ('Ollama OK - ' + @($r.models).Count + ' model(s) at ' + $u);" ^
  "} catch { Write-Host 'WARN: Ollama not reachable'; Write-Host $_.Exception.Message }"

set "HOME=%LEGACY_HOME%"
echo.
echo Launching... ^(Ctrl+C to stop^)
echo.

"%BINARY%" %*
set "EXITCODE=%ERRORLEVEL%"

echo.
echo Exited code %EXITCODE%.
if exist "%LOG_FILE%" (
    echo --- recent bot lines ---
    powershell -NoProfile -ExecutionPolicy Bypass -Command ^
      "Select-String -Path '%LOG_FILE%' -Pattern 'group-key|\[room |auto-reply|headless|E2E|Identity unlocked|ERROR' -CaseSensitive:$false | Select-Object -Last 20 | ForEach-Object { $_.Line }"
)
echo.
pause
exit /b %EXITCODE%

:fail
echo.
pause
exit /b 1
