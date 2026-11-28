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
(pnpm build in client), with the Tauri CLI installed and Python 3.11 or
later available as python on PATH. Generating notices requires cargo-about.
Out is the MSI to write. A copy of its third-party notices is written beside
it with .notices.txt appended to the MSI's name. Stage the Windows service
reference at target\reference\msi\help\index.html or pass its path as HelpFile.
Version, when given, replaces the version the MSI carries, so a test can
build a later package to upgrade to. Stage help with that same version.
Notices, when given, supplies the notice file to bundle instead of
generating it. The bundler downloads
WiX itself and checks it against the hash it carries.

  scripts\build-service-msi.ps1 target\msi\torrplay-service.msi
#>

param(
    [Parameter(Mandatory)]
    [string] $Out,
    [string] $Version,
    [string] $Notices,
    [string] $HelpFile = 'target/reference/msi/help/index.html'
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
$checkHelp = @('scripts/check-reference.py', '--msi-help', $HelpFile)
if ($Version) { $checkHelp += @('--version', $Version) }
Invoke-Checked python $checkHelp
$helpPath = (Resolve-Path -LiteralPath $HelpFile).Path

$repository = (Get-Location).Path
# The variables this sets, which a script shares with the session that runs
# it, so they are put back as they were.
$variables = @('CARGO_TARGET_DIR', 'TAURI_TORRPLAY_SERVICE_EXE', 'TAURI_TORRPLAY_SETTINGS_TEMPLATE',
    'TAURI_TORRPLAY_SETTINGS_ACTION', 'TAURI_TORRPLAY_LICENSE', 'TAURI_TORRPLAY_LICENSE_RTF',
    'TAURI_TORRPLAY_NOTICES', 'TAURI_TORRPLAY_SERVICE_HELP')
$saved = @{}
foreach ($variable in $variables) {
    $saved[$variable] = [Environment]::GetEnvironmentVariable($variable)
}
$noticePath = (New-TemporaryFile).FullName
$licenseRtf = "$noticePath.rtf"
try {
    Remove-Item -Path Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
    Invoke-Checked cargo @('build', '--release', '--locked', '-p', 'torrplay', '-p', 'torrplay-msi')
    if ($Notices) {
        Copy-Item -LiteralPath $Notices -Destination $noticePath
    } else {
        Invoke-Checked python @('scripts/build-notices.py', $noticePath)
    }
    # The fragment's sources, which candle reads from the environment; the
    # bundler passes it only variables whose names start with TAURI.
    $env:TAURI_TORRPLAY_SERVICE_EXE = Join-Path $repository 'target/release/torrplay.exe'
    $env:TAURI_TORRPLAY_SETTINGS_ACTION = Join-Path $repository 'target/release/torrplay_msi.dll'
    $env:TAURI_TORRPLAY_LICENSE = Join-Path $repository 'LICENSES/MIT.txt'
    $env:TAURI_TORRPLAY_NOTICES = $noticePath
    $env:TAURI_TORRPLAY_SETTINGS_TEMPLATE = Join-Path $repository 'packaging/windows/torrplay.env'
    $env:TAURI_TORRPLAY_SERVICE_HELP = $helpPath

    # Give WiX the RTF directly so Tauri does not convert it as plain text.
    $license = [IO.File]::ReadAllText($env:TAURI_TORRPLAY_LICENSE)
    $paragraphs = ($license.Trim() -split '\r?\n[ \t]*\r?\n+') | ForEach-Object {
        ($_ -replace '\s+', ' ') -replace '[\\{}]', '\$0'
    }
    $rtf = '{\rtf1\ansi\deff0{\fonttbl{\f0\fswiss Segoe UI;}}\pard\ql\sa120\f0\fs18 ' +
        ($paragraphs -join "\par`r`n") + '}'
    [IO.File]::WriteAllText($licenseRtf, $rtf)
    $env:TAURI_TORRPLAY_LICENSE_RTF = $licenseRtf

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

    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Out) | Out-Null
    Copy-Item -Path $built[0].FullName -Destination $Out
    Copy-Item -LiteralPath $noticePath -Destination "$Out.notices.txt"
} finally {
    foreach ($variable in $variables) {
        [Environment]::SetEnvironmentVariable($variable, $saved[$variable])
    }
    Remove-Item -LiteralPath $noticePath
    if (Test-Path -LiteralPath $licenseRtf) { Remove-Item -LiteralPath $licenseRtf }
}

Write-Output $Out
