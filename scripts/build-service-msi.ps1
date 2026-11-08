# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

<#
.SYNOPSIS
Builds the service edition's MSI, as ADR 13 decides.

.DESCRIPTION
The Tauri bundler makes an MSI for the computer from the client, the desktop
crate built with its client feature, with desktop\tauri.client.conf.json and
desktop\tauri.service.conf.json merged over desktop\tauri.conf.json. The
latter adds packaging\windows\service.wxs, which installs torrplay.exe as the
torrplay service. Run it from the repository root once the web client is built
(pnpm build in client), with the Tauri CLI installed; Out is the MSI to write.
Version, when given, replaces the version the MSI carries, so a test can
build a later package to upgrade to. The bundler downloads WiX itself and
checks it against the hash it carries.

  scripts\build-service-msi.ps1 target\msi\torrplay-service.msi
#>

param(
    [Parameter(Mandatory)]
    [string] $Out,
    [string] $Version
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Invoke-Checked([string] $Command, [string[]] $Arguments) {
    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Command $Arguments exited with $LASTEXITCODE" }
}

if (-not (Test-Path -Path 'client/dist/index.html')) {
    throw 'the web client is not built: run pnpm build in client'
}
$repository = (Get-Location).Path
# The variables this sets, which a script shares with the session that runs
# it, so they are put back as they were.
$variables = @('CARGO_TARGET_DIR', 'TORRPLAY_SERVICE_EXE', 'TORRPLAY_SETTINGS_TEMPLATE')
$saved = @{}
foreach ($variable in $variables) {
    $saved[$variable] = [Environment]::GetEnvironmentVariable($variable)
}
try {
    Remove-Item -Path Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
    Invoke-Checked cargo @('build', '--release', '--locked', '-p', 'torrplay')
    # The fragment's sources, which candle reads from the environment.
    $env:TORRPLAY_SERVICE_EXE = Join-Path $repository 'target/release/torrplay.exe'
    $env:TORRPLAY_SETTINGS_TEMPLATE = Join-Path $repository 'packaging/windows/torrplay.env'

    # Beside the application's build, which writes torrplay-desktop too.
    $env:CARGO_TARGET_DIR = Join-Path $repository 'desktop/target/client'
    $bundles = Join-Path $env:CARGO_TARGET_DIR 'release/bundle/msi'
    Remove-Item -Recurse -Force -Path $bundles -ErrorAction SilentlyContinue

    $configs = @('--config', 'tauri.client.conf.json', '--config', 'tauri.service.conf.json')
    $versionConfig = $null
    if ($Version) {
        # A file rather than an argument, which PowerShell would requote.
        $versionConfig = New-TemporaryFile
        Set-Content -Path $versionConfig -Value (@{ version = $Version } | ConvertTo-Json) -Encoding utf8NoBOM
        $configs += @('--config', $versionConfig.FullName)
    }
    Push-Location desktop
    try {
        Invoke-Checked cargo (@('tauri', 'build', '--bundles', 'msi') + $configs +
            @('--', '--locked', '--no-default-features', '--features', 'client'))
    } finally {
        Pop-Location
        if ($versionConfig) { Remove-Item -Path $versionConfig }
    }
    $built = @(Get-ChildItem -Path $bundles -Filter '*.msi' -File)
    if ($built.Count -ne 1) { throw "the bundler did not make one MSI in ${bundles}: $built" }
} finally {
    foreach ($variable in $variables) {
        [Environment]::SetEnvironmentVariable($variable, $saved[$variable])
    }
}

New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Out) | Out-Null
Copy-Item -Path $built[0].FullName -Destination $Out
Write-Output $Out
