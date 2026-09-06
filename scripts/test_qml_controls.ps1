param(
    [string]$QtBin = "C:\Qt\6.8.3\msvc2022_64\bin"
)

$ErrorActionPreference = "Stop"
$clientRoot = Join-Path $PSScriptRoot "../rust/conquerd-client"
$temporaryRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("conquerd-qml-" + [guid]::NewGuid())
$moduleRoot = Join-Path $temporaryRoot "ConquerD/Client"
try {
    New-Item -ItemType Directory -Path $moduleRoot -Force | Out-Null
    Copy-Item (Join-Path $clientRoot "tests/qml/qmldir") $moduleRoot
    foreach ($component in @("Theme", "StyledButton", "StyledTextField")) {
        Copy-Item (Join-Path $clientRoot "qml/$component.qml") $moduleRoot
    }
    & (Join-Path $QtBin "qmltestrunner.exe") -input (Join-Path $clientRoot "tests/qml") -import $temporaryRoot
    if ($LASTEXITCODE -ne 0) {
        throw "Qt Quick control tests failed (exit $LASTEXITCODE)."
    }
} finally {
    Remove-Item -Recurse -Force $temporaryRoot
}