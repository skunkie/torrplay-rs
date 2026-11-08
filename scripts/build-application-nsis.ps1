# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

<#
.SYNOPSIS
Builds the application's Windows installer, as ADR 13 decides.

.DESCRIPTION
The Tauri bundler makes an NSIS installer for the current user that installs
torrplay-desktop.exe with torrplay.exe beside it, from desktop\tauri.conf.json
with desktop\tauri.bundle.conf.json merged over it. Run it from the repository
root once the web client is built (pnpm build in client), with the Tauri CLI
installed; Out is the installer to write. The bundler downloads NSIS itself and
checks it against the hash it carries.

  scripts\build-application-nsis.ps1 target\nsis\torrplay-setup.exe
#>

param(
    [Parameter(Mandatory)]
    [string] $Out
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

# The bundler takes an extra executable named for the target, from beside the
# configuration, and installs it without that suffix.
Invoke-Checked cargo @('build', '--release', '--locked', '-p', 'torrplay')
$target = (rustc -vV | Select-String -Pattern '^host: (.+)$').Matches[0].Groups[1].Value
New-Item -ItemType Directory -Force -Path 'desktop/binaries' | Out-Null
Copy-Item -Path 'target/release/torrplay.exe' -Destination "desktop/binaries/torrplay-$target.exe"

$bundles = 'desktop/target/release/bundle/nsis'
Remove-Item -Recurse -Force -Path $bundles -ErrorAction SilentlyContinue
Push-Location desktop
try {
    Invoke-Checked cargo @('tauri', 'build', '--bundles', 'nsis', '--config', 'tauri.bundle.conf.json', '--', '--locked')
} finally {
    Pop-Location
}
$built = @(Get-ChildItem -Path $bundles -Filter '*.exe' -File)
if ($built.Count -ne 1) { throw "the bundler did not make one installer in ${bundles}: $built" }

New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Out) | Out-Null
Copy-Item -Path $built[0].FullName -Destination $Out
Write-Output $Out
