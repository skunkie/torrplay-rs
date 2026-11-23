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

Add -Wizard to enter the startup settings in the installer on a logged-in
English-language desktop.
#>

param(
    [Parameter(Mandatory)]
    [string] $Setup,
    [switch] $Wizard
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
$license = Join-Path $installDirectory 'LICENSE.txt'
$notices = Join-Path $installDirectory 'THIRD-PARTY-NOTICES.txt'
$installed = @($application, $torrplay, $uninstaller, $shortcut, $desktopShortcut, $license, $notices)
$uninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\TorrPlay'
$port = 18642
$running = $null
$data = $null
$settings = Join-Path ([Environment]::GetFolderPath('ApplicationData')) 'org.torrplay.desktop\torrplay.env'
$settingsDirectory = Split-Path -Parent $settings
$settingsDirectoryExisted = Test-Path -LiteralPath $settingsDirectory
$wroteSettings = $false

. (Join-Path $PSScriptRoot 'windows-service-edition.ps1')

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

function Assert-DownloadDirectory([string] $Log, [string] $Directory) {
    $downloadPathField = '"path":' + (ConvertTo-Json -InputObject $Directory -Compress)
    if (-not (Wait-Until {
        (Test-Path -LiteralPath $Log) -and (Select-String -LiteralPath $Log -Pattern $downloadPathField -SimpleMatch -Quiet)
    } 5)) {
        Fail "the installed application did not log $Directory as its download directory"
    }
}

# Runs the installer silently and returns its exit code.
function Invoke-Setup {
    (Start-Process -FilePath $Setup -ArgumentList '/S' -Wait -PassThru).ExitCode
}

function Assert-NotInstalled([string] $Why) {
    if (Test-Path -Path $uninstallKey) { Fail "the application is registered $Why" }
    foreach ($path in $installed) {
        if (Test-Path -Path $path) { Fail "$path exists $Why" }
    }
}

function Uninstall-TestApplication {
    # The uninstaller copies itself elsewhere and returns at once.
    & $uninstaller /S
    if (-not (Wait-Until { -not (Test-Path -Path $uninstallKey) -and -not (Test-Path -Path $application) } 60)) {
        Fail 'the uninstaller did not remove the application'
    }
    if (-not (Wait-Until { -not (Test-Path -Path $installDirectory) } 30)) {
        Fail "the uninstaller left $installDirectory"
    }
    Assert-NotInstalled 'after the uninstall'
}

$previousDownloadDirectory = $env:TORRPLAY_DOWNLOAD_DIR
try {
    $env:TORRPLAY_DOWNLOAD_DIR = $null
    Assert-NotInstalled 'before the test'
    if (Test-Path -LiteralPath $settings) { Fail 'desktop startup settings already exist before the test' }

    Write-Host '== refusal while the service edition is installed'
    # From the client's configuration, so the hook's packed copy of its upgrade
    # code cannot drift from what the MSI registers.
    $serviceKey = Get-ServiceEditionKey
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
    if (Test-Path -LiteralPath $settings) { Fail 'the silent installer wrote desktop startup settings' }
    foreach ($path in $installed) {
        if (-not (Test-Path -Path $path)) { Fail "the installer installed no $path" }
    }
    if ((Get-FileHash -Path $license).Hash -ne (Get-FileHash -Path (Join-Path $PSScriptRoot '../LICENSES/MIT.txt')).Hash) {
        Fail "the installer's LICENSE.txt is not TorrPlay's license"
    }
    if (-not (Select-String -Path $notices -Pattern '^GNU Lesser General Public License, version 2\.1$' -Quiet)) {
        Fail 'the third-party notices lack FFmpeg''s license'
    }
    $registered = Get-ItemProperty -Path $uninstallKey
    if ($registered.DisplayName -cne 'TorrPlay') { Fail "the application is registered as $($registered.DisplayName)" }
    $version = $registered.DisplayVersion
    $printed = & $torrplay --version
    if ($LASTEXITCODE -ne 0 -or $printed -cne "torrplay $version") { Fail "torrplay.exe is not version ${version}: $printed" }
    $product = (Get-Item -Path $application).VersionInfo.ProductVersion
    if ($product -notlike "$version*") { Fail "torrplay-desktop.exe is version $product, not $version" }

    $data = Join-Path ([System.IO.Path]::GetTempPath()) "torrplay-nsis-$PID"
    New-Item -ItemType Directory -Path $data | Out-Null
    $downloads = Join-Path $data 'Média 雪 Downloads'
    New-Item -ItemType Directory -Path $downloads | Out-Null
    $relative = 'Relative Downloads'
    $hostname = 'torrplay.home.arpa'
    $wroteSettings = $true
    if ($Wizard) {
        Uninstall-TestApplication
        $wizardScript = Join-Path $PSScriptRoot 'test-windows-installer-wizard.ps1'
        $wizardArguments = @{
            Edition = 'Application'
            Installer = $Setup
            DownloadDirectory = $downloads
            AllowedHosts = $hostname
        }
        Write-Host '== blank wizard fields check the default download folder without saving settings'
        $defaultData = Join-Path ([Environment]::GetFolderPath('ApplicationData')) 'torrplay\data'
        $defaultDownloads = Join-Path $defaultData 'downloads'
        $newDirectories = @($defaultDownloads, $defaultData, (Split-Path -Parent $defaultData)) |
            Where-Object { -not (Test-Path -LiteralPath $_) }
        $collision = Join-Path $defaultDownloads 'trr_aaaaaaaaaaaaaaaaaaaaaaaaaa.have'
        $collisionCreated = $false
        try {
            New-Item -ItemType Directory -Path $defaultDownloads -Force | Out-Null
            New-Item -ItemType File -Path $collision -Value 'existing TorrPlay resume data' | Out-Null
            $collisionCreated = $true
            & $wizardScript @wizardArguments -UseDefaults -DownloadCollision $collision
            if (Test-Path -LiteralPath $settings) { Fail 'the blank wizard wrote desktop startup settings' }
            Uninstall-TestApplication
        } finally {
            if ($collisionCreated -and (Test-Path -LiteralPath $collision)) {
                Remove-Item -LiteralPath $collision
            }
            foreach ($directory in $newDirectories) {
                if ((Test-Path -LiteralPath $directory -PathType Container) -and
                    -not (Get-ChildItem -LiteralPath $directory -Force | Select-Object -First 1)) {
                    [IO.Directory]::Delete($directory)
                }
            }
        }
        Write-Host '== blank wizard fields check the environment override without saving settings'
        $overrideDownloads = (New-Item -ItemType Directory -Path (Join-Path $data 'Environment Downloads')).FullName
        $collision = Join-Path $overrideDownloads 'trr_aaaaaaaaaaaaaaaaaaaaaaaaaa.have'
        try {
            New-Item -ItemType File -Path $collision -Value 'existing TorrPlay resume data' | Out-Null
            $env:TORRPLAY_DOWNLOAD_DIR = $overrideDownloads
            & $wizardScript @wizardArguments -UseDefaults -DownloadCollision $collision
            if (Test-Path -LiteralPath $settings) { Fail 'the environment override wizard wrote desktop startup settings' }
        } finally {
            $env:TORRPLAY_DOWNLOAD_DIR = $null
            if (Test-Path -LiteralPath $collision) { Remove-Item -LiteralPath $collision }
        }
        Uninstall-TestApplication

        if ($settingsDirectoryExisted) {
            Write-Host '== skipping the settings path collision because the settings directory already exists'
        } else {
            Write-Host '== a failed settings save leaves the installation retryable'
            New-Item -ItemType File -Path $settingsDirectory | Out-Null
            try {
                & $wizardScript @wizardArguments -ApplicationFailure Settings
                Assert-NotInstalled 'after a failed settings save'
            } finally {
                Remove-Item -LiteralPath $settingsDirectory
            }
        }
        Write-Host '== a failed application copy rolls back the initial settings'
        New-Item -ItemType Directory -Path $application | Out-Null
        try {
            & $wizardScript @wizardArguments -ApplicationFailure Copy
            if (Test-Path -LiteralPath $settings) { Fail 'a failed copy left behind the wizard settings' }
        } finally {
            Remove-Item -LiteralPath $application
        }
        Assert-NotInstalled 'after a failed application copy'
        Write-Host '== reject an unwritable download folder and finish setup with a writable one'
        $unwritable = (New-Item -ItemType Directory -Path (Join-Path $data 'Média 雪 Read Only')).FullName
        $identity = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
        $denyWrite = [System.Security.AccessControl.FileSystemAccessRule]::new($identity, 'Write', 'Deny')
        $acl = Get-Acl -LiteralPath $unwritable
        $acl.AddAccessRule($denyWrite)
        Set-Acl -LiteralPath $unwritable -AclObject $acl
        & $wizardScript @wizardArguments -UnwritableDownloadDirectory $unwritable
        if (-not (Test-Path -LiteralPath $settings)) { Fail 'the wizard did not write desktop startup settings' }
    } else {
        $setupSettings = Join-Path $data 'setup.env'
        Set-Content -LiteralPath $setupSettings -Encoding unicode -Value @(
            "TORRPLAY_DOWNLOAD_DIR=$downloads", "TORRPLAY_ALLOWED_HOSTS=$hostname")
        & $torrplay validate-installer-settings $setupSettings $settings
        if ($LASTEXITCODE -ne 0) { Fail 'the installed settings helper refused valid settings' }
    }

    Write-Host '== the installed application uses both startup settings'
    $start = [System.Diagnostics.ProcessStartInfo]::new($application)
    $start.UseShellExecute = $false
    foreach ($argument in @('--data-dir', $data, '--listen', "127.0.0.1:$port", '--log-level', 'info')) {
        $start.ArgumentList.Add($argument)
    }
    [void] $start.Environment.Remove('TORRPLAY_ALLOWED_HOSTS')
    $running = [System.Diagnostics.Process]::Start($start)
    if (-not (Wait-Until { Test-Ready } 60)) { Fail 'the installed application did not become ready' }
    $listeners = @(Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue |
            Select-Object -ExpandProperty OwningProcess -Unique)
    if ($listeners.Count -ne 1 -or $listeners[0] -ne $running.Id) {
        Fail "the API's listener is not the installed application's process: $listeners"
    }
    $log = Join-Path $data 'logs\torrplay.jsonl'
    Assert-DownloadDirectory $log $downloads
    foreach ($requestHost in @($hostname, 'other.example')) {
        $status = & curl.exe -sS -o NUL -w '%{http_code}' --max-time 2 `
            -H "Host: ${requestHost}:$port" "http://127.0.0.1:$port/api/v1"
        $expected = $requestHost -eq $hostname ? '200' : '421'
        if ($LASTEXITCODE -ne 0 -or $status -ne $expected) {
            Fail "the installed application returned $status for $requestHost, expected $expected"
        }
    }
    Stop-Process -Id $running.Id
    $running.WaitForExit()
    $running = $null

    Write-Host '== relative environment overrides stay relative to the working directory'
    $start.WorkingDirectory = $data
    $start.Environment['TORRPLAY_DOWNLOAD_DIR'] = $relative
    $running = [System.Diagnostics.Process]::Start($start)
    if (-not (Wait-Until { Test-Ready } 60)) { Fail 'the application did not start with a relative download directory' }
    Assert-DownloadDirectory $log $relative
    Stop-Process -Id $running.Id
    $running.WaitForExit()
    $running = $null

    Write-Host '== uninstall'
    Uninstall-TestApplication

    Write-Host 'all installer tests passed'
} finally {
    $env:TORRPLAY_DOWNLOAD_DIR = $previousDownloadDirectory
    if ($running) {
        Stop-Process -Id $running.Id -ErrorAction SilentlyContinue
    }
    if ($data) {
        Remove-Item -Recurse -Force -Path $data -ErrorAction SilentlyContinue
    }
    if ($wroteSettings) {
        Remove-Item -LiteralPath $settings -ErrorAction SilentlyContinue
        if (-not $settingsDirectoryExisted -and (Test-Path -LiteralPath $settingsDirectory -PathType Container)) {
            try {
                [IO.Directory]::Delete($settingsDirectory)
            } catch [IO.IOException] {
                Write-Warning "Could not remove the test settings directory $settingsDirectory"
            }
        }
    }
}
