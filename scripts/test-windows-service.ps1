# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

<#
.SYNOPSIS
Installs a torrplay build as the service edition's Windows service with
scripts\windows-service.ps1 and tests it as the service manager runs it: under
its virtual account, with its settings file, restarts, shutdown, and removal.

.DESCRIPTION
It changes the system it runs on, so CI runs it on a disposable runner. It needs
an elevated PowerShell 7 and nothing listening on ports 8642 and 8643:

  scripts\test-windows-service.ps1 target\release\torrplay.exe
#>

param(
    [Parameter(Mandatory)]
    [string] $Binary
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$name = 'torrplay'
$install = Join-Path $PSScriptRoot 'windows-service.ps1'
$dataDirectory = Join-Path $env:ProgramData 'TorrPlay'
$settings = Join-Path $dataDirectory 'torrplay.env'
$programDirectory = Join-Path $env:ProgramFiles 'TorrPlay'
$holder = $null

. (Join-Path $PSScriptRoot 'windows-service-config.ps1')

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

function Test-Ready([int] $Port) {
    try {
        Invoke-WebRequest -Uri "http://127.0.0.1:$Port/readyz" -UseBasicParsing -TimeoutSec 2 | Out-Null
        return $true
    } catch {
        return $false
    }
}

function Get-TorrPlay {
    Get-CimInstance -ClassName Win32_Service -Filter "Name='$name'"
}

function Get-Listeners([int] $Port) {
    @(Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue |
        Select-Object -ExpandProperty OwningProcess -Unique)
}

function Set-Listen([string] $Address) {
    Set-Content -Path $settings -Value "TORRPLAY_LISTEN=$Address" -Encoding utf8NoBOM
}

# How many log records hold $Text.
function Get-Logged([string] $Text) {
    $files = @(Get-ChildItem -Path (Join-Path $dataDirectory 'logs') -Recurse -File)
    @($files | Select-String -Pattern $Text -SimpleMatch).Count
}

# Starts the service, which is to fail with the service-specific exit code
# $Code, and checks that it stays stopped.
function Assert-StartFails([int] $Code, [string] $Why) {
    $started = $true
    try {
        Start-Service -Name $name
    } catch {
        $started = $false
    }
    if ($started) { Fail "the service started $Why" }
    $service = Get-TorrPlay
    if ($service.State -ne 'Stopped' -or $service.ServiceSpecificExitCode -ne $Code) {
        Fail "the service is $($service.State) with exit code $($service.ServiceSpecificExitCode) $Why, not stopped with $Code"
    }
    Start-Sleep -Seconds 10
    if ((Get-TorrPlay).State -ne 'Stopped') {
        Fail "the service manager restarted the service $Why"
    }
}

try {
    Write-Host '== install'
    & $install install $Binary
    if (-not (Wait-Until { Test-Ready 8642 } 60)) { Fail 'the service did not become ready' }
    $service = Get-TorrPlay
    if ($service.DisplayName -cne 'TorrPlay Service') { Fail "the service is named $($service.DisplayName)" }
    if ($service.StartName -ne "NT SERVICE\$name") { Fail "the service runs as $($service.StartName)" }
    if ($service.StartMode -ne 'Auto') { Fail "the service starts $($service.StartMode), not at boot" }
    $preshutdown = [TorrPlayServiceConfig]::GetPreshutdownTimeout($name)
    if ($preshutdown -ne 45000) { Fail "the system waits $preshutdown ms for the service at shutdown, not 45000" }
    $process = Get-CimInstance -ClassName Win32_Process -Filter "ProcessId=$($service.ProcessId)"
    $owner = Invoke-CimMethod -InputObject $process -MethodName GetOwner
    if ("$($owner.Domain)\$($owner.User)" -ne "NT SERVICE\$name") {
        Fail "the service's process is $($owner.Domain)\$($owner.User)'s"
    }
    # In @(), since PowerShell unwraps a list of one that a function returns.
    $listeners = @(Get-Listeners 8642)
    if ($listeners.Count -ne 1 -or $listeners[0] -ne $service.ProcessId) {
        Fail "the API's listener is not the service's process: $listeners"
    }
    if (-not (Test-Path -Path (Join-Path $dataDirectory 'torrplay.db'))) { Fail 'no database in the data directory' }
    $rules = (Get-Acl -Path $dataDirectory).Access
    $writers = @($rules | Where-Object {
            $_.AccessControlType -eq 'Allow' -and
            ($_.FileSystemRights -band [System.Security.AccessControl.FileSystemRights]::Write)
        } | ForEach-Object { $_.IdentityReference.Translate([System.Security.Principal.SecurityIdentifier]).Value })
    $serviceSid = (New-Object System.Security.Principal.NTAccount("NT SERVICE\$name")).Translate(
        [System.Security.Principal.SecurityIdentifier]).Value
    foreach ($writer in $writers) {
        if ($writer -notin @('S-1-5-18', 'S-1-5-32-544', $serviceSid)) {
            Fail "$writer may change the data directory"
        }
    }
    $readers = (Get-Acl -Path $settings).Access | ForEach-Object {
        $_.IdentityReference.Translate([System.Security.Principal.SecurityIdentifier]).Value
    }
    if ('S-1-5-32-545' -notin $readers) { Fail 'users may not read the settings file' }

    Write-Host '== installing again keeps the settings and runs the new build'
    $before = (Get-TorrPlay).ProcessId
    Set-Listen '[::]:8643'
    & $install install $Binary
    if (-not (Wait-Until { Test-Ready 8643 } 60)) { Fail 'the service did not listen at the kept address' }
    if ((Get-TorrPlay).ProcessId -eq $before) { Fail 'installing again did not restart the service' }
    Set-Listen '[::]:8642'
    Restart-Service -Name $name
    if (-not (Wait-Until { Test-Ready 8642 } 60)) { Fail 'the service did not return to port 8642' }

    Write-Host '== the service manager restarts it after a crash'
    $crashed = (Get-TorrPlay).ProcessId
    Stop-Process -Id $crashed -Force
    if (-not (Wait-Until { $id = (Get-TorrPlay).ProcessId; $id -and $id -ne $crashed -and (Test-Ready 8642) } 60)) {
        Fail 'the service was not restarted after a crash'
    }

    Write-Host '== stopping is a clean shutdown'
    $clean = Get-Logged 'recording the clean shutdown'
    $began = Get-Date
    Stop-Service -Name $name
    $took = ((Get-Date) - $began).TotalSeconds
    if ($took -ge 45) { Fail "stopping took $took seconds" }
    $service = Get-TorrPlay
    if ($service.ExitCode -ne 0 -or $service.ServiceSpecificExitCode -ne 0) {
        Fail "the service stopped with $($service.ExitCode) and $($service.ServiceSpecificExitCode)"
    }
    if ((Get-Logged 'recording the clean shutdown') -le $clean) { Fail 'the stop recorded no clean shutdown' }

    Write-Host '== a usage error stops it without a restart'
    Set-Listen 'every interface'
    Assert-StartFails 2 'with an invalid TORRPLAY_LISTEN'
    if ((Get-Logged 'settings are not valid') -eq 0 -or (Get-Logged 'every interface') -eq 0) {
        Fail 'the usage error and its reason were not logged'
    }
    Set-Listen '[::]:8642'

    Write-Host '== a taken port stops it without a restart'
    $holder = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::IPv6Any, 8642)
    $holder.Server.DualMode = $true
    $holder.Start()
    Assert-StartFails 1 'with its port taken'
    $holder.Stop()
    $holder = $null
    Start-Service -Name $name
    if (-not (Wait-Until { Test-Ready 8642 } 60)) { Fail 'the service did not start once the port was free' }

    Write-Host '== removal'
    & $install remove
    if (Get-Service -Name $name -ErrorAction SilentlyContinue) { Fail 'the service is still registered' }
    if (Test-Path -Path $programDirectory) { Fail "$programDirectory is still there" }
    if (-not (Test-Path -Path $settings)) { Fail 'removal deleted the settings' }
    if (-not (Test-Path -Path (Join-Path $dataDirectory 'torrplay.db'))) { Fail 'removal deleted the data' }
    if (@(Get-Listeners 8642).Count -ne 0) { Fail 'something still listens on port 8642' }

    Write-Host 'all service tests passed'
} catch {
    Write-Host "--- the service's logs"
    Get-ChildItem -Path (Join-Path $dataDirectory 'logs') -Recurse -File -ErrorAction SilentlyContinue |
        ForEach-Object { Get-Content -Path $_.FullName -Tail 100 }
    Write-Host '--- the service'
    sc.exe queryex $name
    sc.exe qc $name
    throw
} finally {
    if ($holder) {
        $holder.Stop()
    }
}
