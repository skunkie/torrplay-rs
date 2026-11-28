# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

<#
.SYNOPSIS
Installs the service edition's Windows service and tests it as the service
manager runs it: under its virtual account, with its settings file, restarts,
shutdown, installing again, and removal.

.DESCRIPTION
Given a torrplay build, it installs that with scripts\windows-service.ps1.
Given -Package, the service edition's MSI, it installs that instead, upgrades
it to -Upgrade, a later version's MSI, as installing again, and also checks
the license agreement, bundled notices, offline help, optional client,
firewall rules, and refusal to install beside the application.
Keep each MSI's .notices.txt file beside the MSI.

It changes the system it runs on, so CI runs it on a disposable runner. It needs
an elevated PowerShell 7 and nothing listening on ports 8642 and 8643. The
MSI tests also need Python 3.11 or later as python on PATH.

  scripts\test-windows-service.ps1 target\release\torrplay.exe
  scripts\test-windows-service.ps1 -Package target\msi\torrplay-service.msi -Upgrade target\msi\torrplay-service-next.msi

Add -Wizard to supply startup settings through MSI properties and verify that
the wizard displays them, and to verify F1 in the installed client. This mode
requires Node.js 24 and a logged-in English-language desktop with no retained
service data.
#>

[CmdletBinding(DefaultParameterSetName = 'Binary')]
param(
    [Parameter(Mandatory, Position = 0, ParameterSetName = 'Binary')]
    [string] $Binary,
    [Parameter(Mandatory, ParameterSetName = 'Package')]
    [string] $Package,
    [Parameter(Mandatory, ParameterSetName = 'Package')]
    [string] $Upgrade,
    [Parameter(ParameterSetName = 'Package')]
    [switch] $Wizard
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$name = 'torrplay'
$install = Join-Path $PSScriptRoot 'windows-service.ps1'
$dataDirectory = Join-Path $env:ProgramData 'TorrPlay'
$settings = Join-Path $dataDirectory 'torrplay.env'
$packaged = $PSCmdlet.ParameterSetName -eq 'Package'
# The MSI installs in a directory named for its product, the client.
$defaultProgramDirectory = Join-Path $env:ProgramFiles ($packaged ? 'TorrPlay Client' : 'TorrPlay')
$programDirectory = $defaultProgramDirectory
$executable = Join-Path $programDirectory 'torrplay.exe'
$helpName = 'TorrPlay Service Help.lnk'
$sharedHelpShortcut = Join-Path ([Environment]::GetFolderPath('CommonPrograms')) $helpName
$userHelpShortcut = Join-Path ([Environment]::GetFolderPath('Programs')) $helpName
$applicationKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\TorrPlay'
$holder = $null
$wizardDownloads = $null
if ($packaged) {
    $Package = (Resolve-Path -Path $Package).Path
    $Upgrade = (Resolve-Path -Path $Upgrade).Path
    foreach ($msi in @($Package, $Upgrade)) {
        if (-not (Test-Path -LiteralPath "$msi.notices.txt" -PathType Leaf)) {
            throw "Missing expected notices for ${msi}: $msi.notices.txt"
        }
    }
}

. (Join-Path $PSScriptRoot 'windows-service-config.ps1')
. (Join-Path $PSScriptRoot 'windows-service-edition.ps1')
. (Join-Path $PSScriptRoot 'windows-offline-help.ps1')

function Fail([string] $Message) {
    throw "FAIL: $Message"
}

function Assert-InstalledNotices([string] $Msi) {
    $sources = @{
        'LICENSE.txt' = Join-Path $PSScriptRoot '../LICENSES/MIT.txt'
        'THIRD-PARTY-NOTICES.txt' = "$Msi.notices.txt"
    }
    foreach ($file in $sources.Keys) {
        $installed = Join-Path $programDirectory $file
        if (-not (Test-Path -LiteralPath $installed -PathType Leaf)) { Fail "the MSI did not install $file" }
        $source = $sources[$file]
        if ((Get-FileHash -LiteralPath $installed).Hash -ne (Get-FileHash -LiteralPath $source).Hash) {
            Fail "the installed $file differs from $source"
        }
    }
}

function Assert-InstalledHelp([string] $Version) {
    $helpFile = Join-Path $programDirectory 'help\index.html'
    & python (Join-Path $PSScriptRoot 'check-reference.py') --msi-help $helpFile --version $Version
    if ($LASTEXITCODE -ne 0) { Fail "the installed service reference is invalid at $helpFile" }
    if (-not (Test-Path -LiteralPath $sharedHelpShortcut -PathType Leaf)) {
        Fail "the MSI did not install $sharedHelpShortcut"
    }
    if (Test-Path -LiteralPath $userHelpShortcut) { Fail "the MSI installed a per-user Help shortcut" }
    Assert-ShortcutTarget $sharedHelpShortcut $helpFile
    if ("Offline help: $helpFile" -cnotin (Read-CliHelp $executable $env:SystemRoot)) {
        Fail "the installed CLI did not print the absolute reference path $helpFile"
    }
}

function Assert-NoInstalledHelp {
    foreach ($path in @($sharedHelpShortcut, $userHelpShortcut, (Join-Path $programDirectory 'help\index.html'))) {
        if (Test-Path -LiteralPath $path) { Fail "the service help still exists at $path" }
    }
}

function Assert-ClientF1Help([string] $Client) {
    $log = Join-Path $env:LOCALAPPDATA 'org.torrplay.client\logs\client.log'
    $helpFile = Join-Path $programDirectory 'help\index.html'
    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $listener.Start()
    try {
        $port = $listener.LocalEndpoint.Port
    } finally {
        $listener.Stop()
    }

    # Elevated WebView2 ignores the environment override. Scope the machine
    # policy to the installed client and restore it after testing.
    $policyPath = 'SOFTWARE\Policies\Microsoft\Edge\WebView2\AdditionalBrowserArguments'
    $clientName = Split-Path -Path $Client -Leaf
    $machine = [Microsoft.Win32.Registry]::LocalMachine
    $policy = $machine.OpenSubKey($policyPath, $true)
    $createdPolicy = $null -eq $policy
    if ($createdPolicy) { $policy = $machine.CreateSubKey($policyPath) }
    $previous = $null
    $previousKind = $null
    if ($clientName -in $policy.GetValueNames()) {
        $previous = $policy.GetValue($clientName, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        $previousKind = $policy.GetValueKind($clientName)
    }

    try {
        $policy.SetValue($clientName, "--remote-debugging-address=127.0.0.1 --remote-debugging-port=$port",
            [Microsoft.Win32.RegistryValueKind]::String)
        $process = Start-Process -FilePath $Client -WorkingDirectory $env:SystemRoot -PassThru
        try {
            & node (Join-Path $PSScriptRoot 'press-webview-f1.mjs') $port 'http://127.0.0.1:8642'
            if ($LASTEXITCODE -ne 0 -or -not (Wait-Until {
                (Test-Path -LiteralPath $log) -and
                    (Select-String -LiteralPath $log -SimpleMatch -Pattern 'opened offline help' |
                        Where-Object { $_.Line.Contains("path=$helpFile") })
            } 15)) {
                $process.Refresh()
                $webviews = @(Get-CimInstance -ClassName Win32_Process -Filter "Name='msedgewebview2.exe'")
                $debugging = @($webviews | Where-Object { $_.CommandLine -like "*--remote-debugging-port=$port*" })
                Write-Host "Client exited: $($process.HasExited). WebView2 processes: $($webviews.Count). On the test debugger port: $($debugging.Count)."
                if (Test-Path -LiteralPath $log) { Get-Content -LiteralPath $log -Tail 12 | Write-Host }
                Fail "pressing F1 in the MSI client did not open $helpFile"
            }
            Write-Host "Verified MSI client F1 opens $helpFile."
        } finally {
            if (-not $process.HasExited) {
                $process.Kill()
                if (-not $process.WaitForExit(10000)) { Fail 'the MSI client did not exit after the F1 check' }
            }
            $process.Dispose()
        }
    } finally {
        $removePolicy = $false
        try {
            if ($null -eq $previousKind) {
                $policy.DeleteValue($clientName, $false)
            } else {
                $policy.SetValue($clientName, $previous, $previousKind)
            }
            $removePolicy = $createdPolicy -and $policy.ValueCount -eq 0 -and $policy.SubKeyCount -eq 0
        } finally {
            $policy.Dispose()
        }
        if ($removePolicy) { $machine.DeleteSubKey($policyPath, $false) }
    }
}

function Assert-HelpReadableByStandardUser {
    $password = ConvertTo-SecureString "Tp!7$([guid]::NewGuid().ToString('N'))" -AsPlainText -Force
    try {
        $account = New-LocalUser -Name "tphelp-$PID" -Password $password
        try {
            $users = [System.Security.Principal.SecurityIdentifier]::new('S-1-5-32-545')
            $members = @(Get-LocalGroupMember -SID $users | ForEach-Object { $_.SID })
            if ($account.SID -notin $members) {
                Add-LocalGroupMember -SID $users -Member $account
            }
            Initialize-OfflineHelpInterop
            [TorrPlaySmoke.StandardUserHelp]::CheckFiles($account.Name, $env:COMPUTERNAME, $password,
                $account.SID.Value, $sharedHelpShortcut, (Join-Path $programDirectory 'help\index.html'))
        } finally {
            Remove-LocalUser -SID $account.SID
        }
    } finally {
        $password.Dispose()
    }
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

function Get-SettingsHash {
    if (Test-Path -LiteralPath $settings) {
        (Get-FileHash -LiteralPath $settings).Hash
    }
}

function Assert-SilentSettings([string] $PreviousHash) {
    if ($PreviousHash) {
        if ((Get-SettingsHash) -ne $PreviousHash) { Fail 'the silent installer changed the retained settings' }
    } elseif (Select-String -LiteralPath $settings -Pattern '^TORRPLAY_(DOWNLOAD_DIR|ALLOWED_HOSTS)=' -Quiet) {
        Fail 'the silent installer wrote settings that should come from the interactive wizard'
    }
}

# How many log records hold $Text.
function Get-Logged([string] $Text) {
    $files = @(Get-ChildItem -Path (Join-Path $dataDirectory 'logs') -Recurse -File)
    @($files | Select-String -Pattern $Text -SimpleMatch).Count
}

function Test-DownloadDirectory([string] $Directory) {
    $files = @(Get-ChildItem -LiteralPath (Join-Path $dataDirectory 'logs') -Filter 'torrplay*.jsonl' -File)
    foreach ($match in ($files | Select-String -SimpleMatch '"message":"the download directory"')) {
        try {
            $record = ConvertFrom-Json -InputObject $match.Line -AsHashtable
        } catch {
            # A record may still be getting written. The caller polls again.
            continue
        }
        if ($record['message'] -ceq 'the download directory' -and
            ([string] $record['fields']['path']).TrimEnd('\') -eq $Directory.TrimEnd('\')) {
            return $true
        }
    }
    return $false
}

function Assert-DownloadDirectory([string] $Directory) {
    if (-not (Wait-Until { Test-DownloadDirectory $Directory } 5)) {
        Fail "the service runtime did not log $Directory as its download directory"
    }
}

function Assert-Hostname([int] $Port, [string] $Hostname, [string] $Expected) {
    $status = & curl.exe -sS -o NUL -w '%{http_code}' --max-time 2 `
        -H "Host: ${Hostname}:$Port" "http://127.0.0.1:$Port/api/v1"
    if ($LASTEXITCODE -ne 0 -or $status -ne $Expected) {
        Fail "the service returned $status for $Hostname, expected $Expected"
    }
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

# Runs msiexec quietly with a verbose log and returns its exit code. Unless
# the code is the one expected, it prints what the log says of the action
# that failed, which Windows Installer marks "Return value 3", and of the
# errors it reports, since the log ends with its properties.
function Invoke-Msi([string[]] $Arguments, [int] $Expected = 0) {
    $log = New-TemporaryFile
    $process = Start-Process -FilePath msiexec.exe -Wait -PassThru `
        -ArgumentList ($Arguments + @('/qn', '/norestart', '/l*v', "`"$($log.FullName)`""))
    if ($process.ExitCode -notin @($Expected, 3010)) {
        Write-Host "--- msiexec $Arguments exited with $($process.ExitCode); its log says:"
        Select-String -Path $log -Pattern 'Return value 3' -Context 30, 0 | ForEach-Object {
            $_.Context.PreContext + $_.Line
        } | Write-Host
        Select-String -Path $log -Pattern '^Error \d+\.' | ForEach-Object { $_.Line } | Write-Host
    }
    Remove-Item -Path $log
    $process.ExitCode
}

function Install-TorrPlay([string] $From, [string] $InstallDirectory) {
    if ($packaged) {
        $arguments = @('/i', "`"$From`"")
        if ($InstallDirectory) { $arguments += "INSTALLDIR=`"$InstallDirectory`"" }
        $code = Invoke-Msi $arguments
        if ($code -notin @(0, 3010)) { Fail "installing $From exited with $code" }
    } else {
        & $install install $From
    }
}

# The firewall rules for the service's executable, with their profiles.
function Get-FirewallProfiles {
    @(Get-NetFirewallApplicationFilter -Program $executable -ErrorAction SilentlyContinue |
        Get-NetFirewallRule | Where-Object { $_.Direction -eq 'Inbound' -and $_.Action -eq 'Allow' } |
        ForEach-Object { $_.Profile.ToString() -split ', ' } | Sort-Object -Unique)
}

function Assert-FailedWizardInstall {
    if (Get-Service -Name $name -ErrorAction SilentlyContinue) { Fail 'the failed MSI left the service registered' }
    if (Test-Path -LiteralPath $programDirectory) { Fail 'the failed MSI left the program directory' }
    Assert-NoInstalledHelp
    if (Test-Path -LiteralPath $settings) { Fail 'the failed MSI left service settings' }
    if (Test-Path -Path (Get-ServiceEditionKey)) { Fail 'the failed MSI left the upgrade code registered' }
    if (@(Get-FirewallProfiles).Count -ne 0) { Fail 'the failed MSI left firewall rules' }
    if (Get-ItemProperty -Path 'HKLM:\Software\TorrPlay\Setup' -Name ServiceInstalled -ErrorAction SilentlyContinue) {
        Fail 'the failed MSI left its completed-install marker'
    }
}

function Assert-RejectedDirectoryEvent([string] $Directory, [datetime] $Started, [string] $Reason = '.+') {
    # The MSI folder picker may preserve a trailing backslash.
    $expectedMessage = '^TorrPlay setup could not use the download directory ' +
        [regex]::Escape($Directory) + '\\?: ' + $Reason
    if (-not (Wait-Until {
        $events = @(Get-WinEvent -FilterHashtable @{
            LogName = 'Application'; Id = 1001; StartTime = $Started
        } -ErrorAction SilentlyContinue | Where-Object {
            $_.ProviderName -eq 'TorrPlay' -and $_.Properties.Count -gt 0 -and
            $_.Properties[0].Value -match $expectedMessage
        })
        $events.Count -gt 0
    } 10)) {
        Fail 'the service did not record the failed download directory probe in the Application event log'
    }
}

function Read-MsiValue([string] $Msi, [string] $Query) {
    $installer = New-Object -ComObject WindowsInstaller.Installer
    $database = $null
    $view = $null
    $record = $null
    try {
        $database = $installer.OpenDatabase($Msi, 0)
        $view = $database.OpenView($Query)
        [void] $view.Execute()
        $record = $view.Fetch()
        [void] $view.Close()
        if (-not $record) { Fail "the MSI at $Msi returned no row for $Query" }
        $record.StringData(1)
    } finally {
        # Released now, not when the collector runs, since msiexec opens the
        # same file next.
        foreach ($object in @($record, $view, $database, $installer)) {
            if ($object) { [void] [System.Runtime.InteropServices.Marshal]::ReleaseComObject($object) }
        }
    }
}

# The license paragraphs stored in the MSI dialog, separated by RTF \par.
function Get-LicenseParagraphs([string] $Msi) {
    $text = Read-MsiValue $Msi `
        "SELECT ``Text`` FROM ``Control`` WHERE ``Dialog_`` = 'LicenseAgreementDlg' AND ``Type`` = 'ScrollableText'"
    $text = $text -replace '(?s)^.*?\\fs\d+ ', '' -replace '}\s*$', ''
    ($text -split '\\par\b') | ForEach-Object { $_.Trim() -replace '\\([\\{}])', '$1' }
}

try {
    if ($packaged) {
        Assert-NoInstalledHelp
        $versionQuery = "SELECT ``Value`` FROM ``Property`` WHERE ``Property`` = 'ProductVersion'"
        $packageVersion = [version](Read-MsiValue $Package $versionQuery)
        $upgradeVersion = [version](Read-MsiValue $Upgrade $versionQuery)
        if ($upgradeVersion -le $packageVersion) { Fail 'the upgrade MSI must have a later version' }
        Write-Host '== the license agreement'
        $license = Get-Content -Raw -Path (Join-Path $PSScriptRoot '../LICENSES/MIT.txt')
        $expected = @($license.Trim() -split '(?:\r?\n){2,}' | ForEach-Object { $_ -replace '\s+', ' ' })
        $shown = @(Get-LicenseParagraphs $Package)
        if ($shown.Count -ne $expected.Count) {
            Fail "the MSI's license agreement has $($shown.Count) paragraphs, expected $($expected.Count)"
        }
        for ($index = 0; $index -lt $expected.Count; $index++) {
            if ($shown[$index] -cne $expected[$index]) {
                Fail "the MSI's license agreement differs from LICENSES/MIT.txt at paragraph $($index + 1)"
            }
        }

        Write-Host '== refusal while the application is installed'
        if (Test-Path -Path $applicationKey) { Fail 'the application is installed for this user' }
        try {
            New-Item -Path $applicationKey -Force | Out-Null
            New-ItemProperty -Path $applicationKey -Name DisplayName -Value 'TorrPlay' | Out-Null
            $code = Invoke-Msi @('/i', "`"$Package`"") -Expected 1603
            if ($code -ne 1603) { Fail "the MSI exited with $code beside the application, not 1603" }
            if (Get-Service -Name $name -ErrorAction SilentlyContinue) { Fail 'a refused install registered the service' }
            if (Test-Path -Path $programDirectory) { Fail "a refused install made $programDirectory" }
            Assert-NoInstalledHelp
        } finally {
            Remove-Item -Path $applicationKey -Recurse -Force -ErrorAction SilentlyContinue
        }

        Write-Host '== the service alone, which an upgrade keeps alone'
        $programDirectory = Join-Path $env:ProgramFiles "TorrPlay Média 雪 Service $PID"
        if (Test-Path -LiteralPath $programDirectory) { Fail "the custom program directory already exists at $programDirectory" }
        $executable = Join-Path $programDirectory 'torrplay.exe'
        $client = Join-Path $programDirectory 'torrplay-client.exe'
        if ($Wizard) {
            if (Test-Path -LiteralPath $dataDirectory) { Fail "the wizard requires a fresh install without $dataDirectory" }
            $directory = Join-Path $env:ProgramData "TorrPlay Test Downloads $PID"
            $wizardDownloads = (New-Item -ItemType Directory -Path $directory).FullName
            # The virtual account's SID is available before the service is registered.
            $identity = [System.Security.Principal.SecurityIdentifier]::new(
                'S-1-5-80-2828507108-2845588903-360128926-4130653692-64400780')
            $rule = [System.Security.AccessControl.FileSystemAccessRule]::new(
                $identity, 'Modify', 'ContainerInherit, ObjectInherit', 'None', 'Allow')
            $acl = Get-Acl -LiteralPath $wizardDownloads
            $acl.AddAccessRule($rule)
            Set-Acl -LiteralPath $wizardDownloads -AclObject $acl

            $wizardScript = Join-Path $PSScriptRoot 'test-windows-installer-wizard.ps1'
            $wizardArguments = @{
                Edition = 'Service'; Installer = $Package; AllowedHosts = 'torrplay,*.home.arpa'
                InstallDirectory = $programDirectory
            }
            Write-Host '== a fresh MSI rolls back when the service cannot write to the chosen folder'
            $lockedDownloads = (New-Item -ItemType Directory -Path (Join-Path $wizardDownloads 'Read Only')).FullName
            # Deny files in the root only. Inherited Modify still allows
            # creating torrent directories and writing files inside them.
            $denyWrite = [System.Security.AccessControl.FileSystemAccessRule]::new($identity, 'CreateFiles', 'Deny')
            $acl = Get-Acl -LiteralPath $lockedDownloads
            $acl.AddAccessRule($denyWrite)
            Set-Acl -LiteralPath $lockedDownloads -AclObject $acl
            $probeStarted = Get-Date
            & $wizardScript @wizardArguments -DownloadDirectory $lockedDownloads -ExpectServiceStartFailure
            Assert-FailedWizardInstall
            if (Test-Path -LiteralPath $dataDirectory) { Fail 'the failed folder probe left the service data directory' }
            Assert-RejectedDirectoryEvent $lockedDownloads $probeStarted

            Write-Host '== blank wizard fields preserve existing TorrPlay downloads in the default directory'
            $otherDownloads = (New-Item -ItemType Directory -Path (Join-Path $dataDirectory 'downloads')).FullName
            $acl = Get-Acl -LiteralPath $otherDownloads
            $acl.AddAccessRule($rule)
            Set-Acl -LiteralPath $otherDownloads -AclObject $acl
            $otherTorrent = (New-Item -ItemType Directory -Path (Join-Path $otherDownloads 'trr_aaaaaaaaaaaaaaaaaaaaaaaaaa')).FullName
            $otherPayload = Join-Path $otherTorrent 'example.txt'
            Set-Content -LiteralPath $otherPayload -Value 'existing torrent data'
            $probeStarted = Get-Date
            & $wizardScript @wizardArguments -DownloadDirectory $otherDownloads -UseDefaults -ExpectServiceStartFailure
            Assert-FailedWizardInstall
            Assert-RejectedDirectoryEvent $otherDownloads $probeStarted 'The download directory contains existing TorrPlay data\.'
            if ((Get-Content -LiteralPath $otherPayload) -cne 'existing torrent data') {
                Fail 'setup changed existing torrent data'
            }
            Remove-Item -LiteralPath $otherPayload
            [IO.Directory]::Delete($otherTorrent)
            [IO.Directory]::Delete($otherDownloads)
            [IO.Directory]::Delete($dataDirectory)

            Write-Host '== a fresh MSI rolls back when the API port is occupied'
            $bindDownloads = (New-Item -ItemType Directory -Path (Join-Path $wizardDownloads 'Bind Failure')).FullName
            $holder = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::IPv6Any, 8642)
            $holder.Server.DualMode = $true
            $holder.Start()
            try {
                & $wizardScript @wizardArguments -DownloadDirectory $bindDownloads -ExpectServiceStartFailure
                Assert-FailedWizardInstall
                if ((Get-Logged 'binding the API listener') -eq 0) { Fail 'the service did not report the bind failure' }
            } finally {
                $holder.Stop()
                $holder = $null
            }

            Write-Host '== retry with a folder writable by the service and unreadable by the installer user'
            $serviceDownloads = (New-Item -ItemType Directory -Path (Join-Path $wizardDownloads 'Média 雪 Service Only')).FullName
            $originalAcl = Get-Acl -LiteralPath $serviceDownloads
            $acl = Get-Acl -LiteralPath $serviceDownloads
            $installerUser = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
            $denyRead = [System.Security.AccessControl.FileSystemAccessRule]::new($installerUser, 'ReadAndExecute', 'Deny')
            $acl.AddAccessRule($denyRead)
            Set-Acl -LiteralPath $serviceDownloads -AclObject $acl
            try {
                & $wizardScript @wizardArguments -DownloadDirectory $serviceDownloads
            } finally {
                Set-Acl -LiteralPath $serviceDownloads -AclObject $originalAcl
            }
            if (-not (Test-Path -LiteralPath $settings)) { Fail 'the wizard did not write service startup settings' }
            $settingsLines = [IO.File]::ReadAllLines($settings)
            foreach ($line in $settingsLines) {
                if ($line -cnotmatch '^\s*(?:$|#|[A-Z_][A-Z0-9_]*=)') {
                    Fail "unexpected line in the wizard's settings file: $line"
                }
            }
            $wizardSettingsHash = (Get-FileHash -LiteralPath $settings).Hash
        } else {
            $previousSettingsHash = Get-SettingsHash
            $code = Invoke-Msi @('/i', "`"$Package`"", 'ADDLOCAL=Service', "INSTALLDIR=`"$programDirectory`"")
            if ($code -notin @(0, 3010)) { Fail "installing the service alone exited with $code" }
            Assert-SilentSettings $previousSettingsHash
        }
        if (-not (Wait-Until { Test-Ready 8642 } 60)) { Fail 'the service alone did not become ready' }
        if (Test-Path -Path $client) { Fail 'the service alone came with the client' }
        Assert-InstalledNotices $Package
        Assert-InstalledHelp $packageVersion
        Assert-HelpReadableByStandardUser
        Write-Host 'Verified that a standard user can read the shared Help shortcut and guide.'
        if ($Wizard) {
            Assert-DownloadDirectory $serviceDownloads
            if ((Get-Logged 'the service can write to the setup download directory') -eq 0) {
                Fail 'the MSI did not request a write-access check from the service'
            }
            Assert-Hostname 8642 'torrplay' '200'
            Assert-Hostname 8642 'nas.home.arpa' '200'
            Assert-Hostname 8642 'other.example' '421'
        }
        Install-TorrPlay $Upgrade -InstallDirectory $programDirectory
        if (-not (Wait-Until { Test-Ready 8642 } 60)) { Fail 'the upgraded service did not become ready' }
        if (Test-Path -Path $client) { Fail 'the upgrade added the client left out' }
        Assert-InstalledNotices $Upgrade
        Assert-InstalledHelp $upgradeVersion
        if ($Wizard -and (Get-FileHash -LiteralPath $settings).Hash -ne $wizardSettingsHash) {
            Fail 'the upgrade changed the wizard settings'
        }
        $code = Invoke-Msi @('/x', "`"$Upgrade`"")
        if ($code -notin @(0, 3010)) { Fail "removing the service alone exited with $code" }
        if (Get-Service -Name $name -ErrorAction SilentlyContinue) { Fail 'removal left the service registered' }
        Assert-NoInstalledHelp
        if (Test-Path -LiteralPath $programDirectory) { Fail "removal left $programDirectory" }
        $programDirectory = $defaultProgramDirectory
        $executable = Join-Path $programDirectory 'torrplay.exe'
        $client = Join-Path $programDirectory 'torrplay-client.exe'
    }

    Write-Host '== install'
    $previousSettingsHash = Get-SettingsHash
    Install-TorrPlay ($packaged ? $Package : $Binary)
    if (-not (Wait-Until { Test-Ready 8642 } 60)) { Fail 'the service did not become ready' }
    Assert-SilentSettings ($Wizard ? $wizardSettingsHash : $previousSettingsHash)
    if ($packaged) {
        if (-not (Test-Path -Path $client)) { Fail 'the MSI installed no client by default' }
        Assert-InstalledNotices $Package
        Assert-InstalledHelp $packageVersion
        if ($Wizard) {
            Write-Host '== F1 in the installed client'
            Assert-ClientF1Help $client
        }
        if ((Get-TorrPlay).PathName -ne "`"$executable`" service") {
            Fail "the service runs $((Get-TorrPlay).PathName)"
        }
        # Where the application's installer looks for it, to refuse beside it.
        if (-not (Test-Path -Path (Get-ServiceEditionKey))) {
            Fail "the MSI did not register its upgrade code at $(Get-ServiceEditionKey)"
        }
        $profiles = @(Get-FirewallProfiles)
        if (($profiles -join ',') -ne 'Domain,Private') {
            Fail "the firewall lets the service listen on $($profiles -join ', ') networks, not domain and private ones"
        }
    }
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
    if ($serviceSid -notin $writers) { Fail 'the service may not change its data directory' }
    $readers = (Get-Acl -Path $settings).Access | ForEach-Object {
        $_.IdentityReference.Translate([System.Security.Principal.SecurityIdentifier]).Value
    }
    if ('S-1-5-32-545' -notin $readers) { Fail 'users may not read the settings file' }

    Write-Host '== installing again keeps the settings and runs the new build'
    $before = (Get-TorrPlay).ProcessId
    Set-Listen '[::]:8643'
    Add-Content -Path $settings -Value 'TORRPLAY_ALLOWED_HOSTS=torrplay,*.home.arpa' -Encoding utf8NoBOM
    $downloadDir = Join-Path $dataDirectory 'Media Downloads'
    Add-Content -Path $settings -Value "TORRPLAY_DOWNLOAD_DIR=`"$downloadDir`"" -Encoding utf8NoBOM
    Install-TorrPlay ($packaged ? $Upgrade : $Binary)
    if (-not (Wait-Until { Test-Ready 8643 } 60)) { Fail 'the service did not listen at the kept address' }
    if ((Get-TorrPlay).ProcessId -eq $before) { Fail 'installing again did not restart the service' }
    if ($packaged) {
        Assert-InstalledNotices $Upgrade
        Assert-InstalledHelp $upgradeVersion
    }
    Assert-DownloadDirectory $downloadDir
    Assert-Hostname 8643 'torrplay' '200'
    Assert-Hostname 8643 'nas.home.arpa' '200'
    Assert-Hostname 8643 'other.example' '421'
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
    if ($packaged) {
        # By the upgrade's package, which is the one installed now.
        $code = Invoke-Msi @('/x', "`"$Upgrade`"")
        if ($code -notin @(0, 3010)) { Fail "removing the MSI exited with $code" }
        Assert-NoInstalledHelp
        # In @(), since PowerShell returns no rules as $null.
        if (@(Get-FirewallProfiles).Count -ne 0) { Fail 'removal left the firewall rules' }
        if (Test-Path -Path (Get-ServiceEditionKey)) { Fail 'removal left the upgrade code registered' }
    } else {
        & $install remove
    }
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
    if ($wizardDownloads) {
        Remove-Item -LiteralPath $wizardDownloads -Recurse -Force
    }
}
