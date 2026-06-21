@echo off
setlocal

set "ROOT=%~dp0"
set "CONQUERD_HOME=%ROOT%.clientA"
set "CONQUERD_KEY_DIR=%CONQUERD_HOME%"
set "LEGACY_HOME=%ROOT%.clientA_home"
set "LEGACY_PROFILE_LINK=%LEGACY_HOME%\.conquerd"
set "BINARY=%ROOT%dist\ConquerD\ConquerD.exe"

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
    echo Build or package it first so dist\ConquerD\ConquerD.exe exists.
    exit /b 1
)

if not exist "%CONQUERD_HOME%\NUL" mkdir "%CONQUERD_HOME%"
if not exist "%LEGACY_HOME%\NUL" mkdir "%LEGACY_HOME%"
if not exist "%LEGACY_PROFILE_LINK%\NUL" (
    mklink /J "%LEGACY_PROFILE_LINK%" "%CONQUERD_HOME%" >nul
)

set "HOME=%LEGACY_HOME%"

"%BINARY%" %*
