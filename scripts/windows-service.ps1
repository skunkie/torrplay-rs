# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

<#
.SYNOPSIS
Installs a torrplay build as the service edition's Windows service, or removes
it, by hand, until the installer does it. Run it in an elevated PowerShell:

  scripts\windows-service.ps1 install target\release\torrplay.exe
  scripts\windows-service.ps1 remove

.DESCRIPTION
`install` copies the build to %ProgramFiles%\TorrPlay and registers the
torrplay service, which starts at boot as its virtual account,
NT SERVICE\torrplay, with `torrplay service`. Its data and settings file are in
%ProgramData%\TorrPlay, which only that account, SYSTEM, and administrators may
change; users may read the settings file, as the client does. The settings
file is installed only when there is none, so installing again keeps its
settings, and the service is restarted, so installing again runs the new
build. The service manager restarts it after a crash, after 5 seconds, then 30,
then 5 minutes, but not after a failure it reports, such as an invalid setting
or a taken port. The system waits up to 45 seconds for it at shutdown.

`remove` stops and deletes the service and the build, and keeps
%ProgramData%\TorrPlay, with the settings and the data: the settings file is in
that directory, unlike /etc/torrplay on Linux, which removal there deletes.
#>

param(
    [Parameter(Mandatory)]
    [ValidateSet('install', 'remove')]
    [string] $Action,
    [string] $Binary
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$name = 'torrplay'
$account = "NT SERVICE\$name"
$programDirectory = Join-Path $env:ProgramFiles 'TorrPlay'
$executable = Join-Path $programDirectory 'torrplay.exe'
$dataDirectory = Join-Path $env:ProgramData 'TorrPlay'
$settings = Join-Path $dataDirectory 'torrplay.env'
$template = Join-Path $PSScriptRoot '..\packaging\windows\torrplay.env'

. (Join-Path $PSScriptRoot 'windows-service-config.ps1')

# Runs a native command, which does not stop the script on failure by itself.
function Invoke-Native {
    param([string] $Command, [string[]] $Arguments)
    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Command $($Arguments -join ' ') failed with exit code $LASTEXITCODE"
    }
}

function Stop-TorrPlay {
    $service = Get-Service -Name $name -ErrorAction SilentlyContinue
    if ($service -and $service.Status -ne 'Stopped') {
        Stop-Service -Name $name
        $service.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(60))
    }
}

switch ($Action) {
    'install' {
        if (-not $Binary) {
            throw 'install takes the build to install'
        }
        Stop-TorrPlay
        New-Item -ItemType Directory -Force -Path $programDirectory | Out-Null
        Copy-Item -Path $Binary -Destination $executable -Force
        $binaryPath = "`"$executable`" service"
        if (Get-Service -Name $name -ErrorAction SilentlyContinue) {
            Invoke-Native sc.exe @('config', $name, 'binPath=', $binaryPath, 'obj=', $account, 'start=', 'auto')
        } else {
            Invoke-Native sc.exe @('create', $name, 'binPath=', $binaryPath, 'obj=', $account,
                'start=', 'auto')
        }
        # Not TorrPlay: the service manager keeps a display name that differs
        # from the service's name in case alone.
        Set-Service -Name $name -DisplayName 'TorrPlay Service' `
            -Description 'Streams torrents through the TorrPlay REST API.'
        # Crashes only: a failure the service reports as its exit code is not one.
        Invoke-Native sc.exe @('failure', $name, 'reset=', '86400',
            'actions=', 'restart/5000/restart/30000/restart/300000')
        Invoke-Native sc.exe @('failureflag', $name, '0')
        # Longer than the shutdown sequence's 30-second limit.
        [TorrPlayServiceConfig]::SetPreshutdownTimeout($name, 45000)

        # The virtual account exists once the service does.
        New-Item -ItemType Directory -Force -Path $dataDirectory | Out-Null
        # SYSTEM and administrators by their well-known SIDs, which every language
        # names differently.
        Invoke-Native icacls.exe @($dataDirectory, '/inheritance:r', '/grant:r',
            '*S-1-5-18:(OI)(CI)F', '*S-1-5-32-544:(OI)(CI)F', "${account}:(OI)(CI)M")
        if (-not (Test-Path -Path $settings)) {
            Copy-Item -Path $template -Destination $settings
        }
        # Users, for the client, which reads the listen address there.
        Invoke-Native icacls.exe @($settings, '/grant', '*S-1-5-32-545:R')
        Start-Service -Name $name
    }
    'remove' {
        if (Get-Service -Name $name -ErrorAction SilentlyContinue) {
            Stop-TorrPlay
            Invoke-Native sc.exe @('delete', $name)
        }
        if (Test-Path -Path $programDirectory) {
            Remove-Item -Path $programDirectory -Recurse -Force
        }
    }
}
