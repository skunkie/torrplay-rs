# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

<#
.SYNOPSIS
Installs the application with its Windows installer and checks what it
installs, that it runs, and that it refuses while the service edition is
installed, then uninstalls it.

.DESCRIPTION
It changes the system it runs on, so CI runs it on a disposable runner. It
needs an elevated PowerShell 7, to stand in for the service edition's MSI in
the machine's registry, and nothing listening on port 18642:

  scripts\test-application-nsis.ps1 target\nsis\torrplay-setup.exe
#>

param(
    [Parameter(Mandatory)]
    [string] $Setup
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Setup = (Resolve-Path -Path $Setup).Path
$installDirectory = Join-Path $env:LOCALAPPDATA 'TorrPlay'
$application = Join-Path $installDirectory 'torrplay-desktop.exe'
$torrplay = Join-Path $installDirectory 'torrplay.exe'
$uninstaller = Join-Path $installDirectory 'uninstall.exe'
$shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'TorrPlay.lnk'
# A silent install also puts one on the desktop, since it skips the page
# that offers it.
$desktopShortcut = Join-Path ([Environment]::GetFolderPath('Desktop')) 'TorrPlay.lnk'
$installed = @($application, $torrplay, $uninstaller, $shortcut, $desktopShortcut)
$uninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\TorrPlay'
$port = 18642
$running = $null
$data = $null

function Fail([string] $Message) {
    throw "FAIL: $Message"
}

function Wait-Until([scriptblock] $Condition, [int] $Seconds) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while (-not (& $Condition)) {
        if ((Get-Date) -gt $deadline) {
            return $false
        }
        Start-Sleep -Milliseconds 500
    }
    return $true
}

function Test-Ready {
    try {
        Invoke-WebRequest -Uri "http://127.0.0.1:$port/readyz" -UseBasicParsing -TimeoutSec 2 | Out-Null
        return $true
    } catch {
        return $false
    }
}

# Runs the installer silently and returns its exit code.
function Invoke-Setup {
    (Start-Process -FilePath $Setup -ArgumentList '/S' -Wait -PassThru).ExitCode
}

# Windows Installer's packed form of a GUID, under which it registers the
# products installed for the computer by upgrade code: the first three groups
# reversed, then the two characters of each later byte swapped.
function ConvertTo-PackedGuid([string] $Guid) {
    $hex = ([guid] $Guid).ToString('N').ToUpperInvariant()
    $reverse = { param($Text) -join $Text[($Text.Length - 1)..0] }
    $packed = (& $reverse $hex.Substring(0, 8)) + (& $reverse $hex.Substring(8, 4)) + (& $reverse $hex.Substring(12, 4))
    for ($index = 16; $index -lt 32; $index += 2) {
        $packed += & $reverse $hex.Substring($index, 2)
    }
    $packed
}

function Assert-NotInstalled([string] $Why) {
    if (Test-Path -Path $uninstallKey) { Fail "the application is registered $Why" }
    foreach ($path in $installed) {
        if (Test-Path -Path $path) { Fail "$path exists $Why" }
    }
}

try {
    Assert-NotInstalled 'before the test'

    Write-Host '== refusal while the service edition is installed'
    # Its upgrade code comes from the client's configuration, so the hook's
    # packed copy of it cannot drift from what the MSI registers.
    $client = Get-Content -Raw -Path (Join-Path $PSScriptRoot '../desktop/tauri.client.conf.json') | ConvertFrom-Json
    $serviceKey = 'HKLM:\SOFTWARE\Classes\Installer\UpgradeCodes\' + (ConvertTo-PackedGuid $client.bundle.windows.wix.upgradeCode)
    if (Test-Path -Path $serviceKey) { Fail 'the service edition is installed on this computer' }
    # The installer's WebView2 step, which runs before the hook, also aborts
    # with exit code 2 when it cannot install the runtime, so the refusal
    # counts only where the runtime is already installed.
    $webView = 'Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
    $runtime = @("HKLM:\SOFTWARE\WOW6432Node\$webView", "HKLM:\SOFTWARE\$webView", "HKCU:\SOFTWARE\$webView") |
        Where-Object {
            # Strict mode makes a missing key or value an error, not $null.
            try { [bool](Get-ItemPropertyValue -Path $_ -Name pv) } catch { $false }
        }
    if (-not $runtime) { Fail 'the WebView2 runtime is not installed, so a refusal would not show the hook ran' }
    try {
        # A packed product code, as Windows Installer records under it.
        New-Item -Path $serviceKey -Force | Out-Null
        New-ItemProperty -Path $serviceKey -Name '00000000000000000000000000000001' -Value '' -PropertyType String | Out-Null
        $code = Invoke-Setup
        if ($code -ne 2) { Fail "the installer exited with $code beside the service edition, not 2" }
        Assert-NotInstalled 'after a refused install'
    } finally {
        Remove-Item -Path $serviceKey -Recurse -Force -ErrorAction SilentlyContinue
    }

    Write-Host '== install'
    $code = Invoke-Setup
    if ($code -ne 0) { Fail "the installer exited with $code" }
    foreach ($path in $installed) {
        if (-not (Test-Path -Path $path)) { Fail "the installer installed no $path" }
    }
    $registered = Get-ItemProperty -Path $uninstallKey
    if ($registered.DisplayName -cne 'TorrPlay') { Fail "the application is registered as $($registered.DisplayName)" }
    $version = $registered.DisplayVersion
    $printed = & $torrplay --version
    if ($LASTEXITCODE -ne 0 -or $printed -cne "torrplay $version") { Fail "torrplay.exe is not version ${version}: $printed" }
    $product = (Get-Item -Path $application).VersionInfo.ProductVersion
    if ($product -notlike "$version*") { Fail "torrplay-desktop.exe is version $product, not $version" }

    Write-Host '== the installed application serves'
    $data = Join-Path ([System.IO.Path]::GetTempPath()) "torrplay-nsis-$PID"
    New-Item -ItemType Directory -Path $data | Out-Null
    $running = Start-Process -FilePath $application -ArgumentList "--data-dir `"$data`" --listen 127.0.0.1:$port" -PassThru
    if (-not (Wait-Until { Test-Ready } 60)) { Fail 'the installed application did not become ready' }
    $listeners = @(Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue |
            Select-Object -ExpandProperty OwningProcess -Unique)
    if ($listeners.Count -ne 1 -or $listeners[0] -ne $running.Id) {
        Fail "the API's listener is not the installed application's process: $listeners"
    }
    Stop-Process -Id $running.Id
    $running.WaitForExit()
    $running = $null

    Write-Host '== uninstall'
    # The uninstaller copies itself elsewhere and returns at once, so what it
    # leaves behind is waited for rather than its process.
    & $uninstaller /S
    if (-not (Wait-Until { -not (Test-Path -Path $uninstallKey) -and -not (Test-Path -Path $application) } 60)) {
        Fail 'the uninstaller did not remove the application'
    }
    if (-not (Wait-Until { -not (Test-Path -Path $installDirectory) } 30)) {
        Fail "the uninstaller left $installDirectory"
    }
    Assert-NotInstalled 'after the uninstall'

    Write-Host 'all installer tests passed'
} finally {
    if ($running) {
        Stop-Process -Id $running.Id -ErrorAction SilentlyContinue
    }
    if ($data) {
        Remove-Item -Recurse -Force -Path $data -ErrorAction SilentlyContinue
    }
}
