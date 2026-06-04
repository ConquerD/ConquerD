@echo off
setlocal

set "ROOT=%~dp0"
set "BINARY=%ROOT%rust\target\debug\conquerd-client.exe"

:: HiDPI display scaling.  ConquerD sets QT_SCALE_FACTOR=0.75 automatically
:: at runtime when Windows DPI > 96 (i.e. display scaling > 100%), so Material
:: controls stay desktop-compact on 4K/HiDPI monitors.
:: Override here if you want a different value, e.g.:
::   set QT_SCALE_FACTOR=1.0    -- full OS DPI (largest controls)
::   set QT_SCALE_FACTOR=0.85   -- lighter reduction

if not exist "%BINARY%" (
    echo ConquerD client binary not found at:
    echo   %BINARY%
    echo.
    echo Build it first:
    echo   cd rust\conquerd-client
    echo   cargo build --features qt-ui
    exit /b 1
)

"%BINARY%" %*
