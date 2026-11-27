# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

<#
.SYNOPSIS
Installs the application with its Windows installer and checks what it
installs, including offline help and its shortcuts. Checks that the
application runs, setup refuses while the service edition is installed,
and the uninstaller removes the application and its own shortcuts.

.DESCRIPTION
It changes the system it runs on, so CI runs it on a disposable runner. It
needs an elevated PowerShell 7, to stand in for the service edition's MSI in
the machine's registry, Python 3.11 or later, and nothing listening on port 18642:

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
$helpShortcutNames = @('TorrPlay Help.lnk', 'Справка TorrPlay.lnk')
$helpShortcuts = $helpShortcutNames | ForEach-Object {
    Join-Path ([Environment]::GetFolderPath('Programs')) $_
}
$sharedHelpShortcuts = $helpShortcutNames | ForEach-Object {
    Join-Path ([Environment]::GetFolderPath('CommonPrograms')) $_
}
# A silent install also puts one on the desktop, since it skips the page
# that offers it.
$desktopShortcut = Join-Path ([Environment]::GetFolderPath('Desktop')) 'TorrPlay.lnk'
$license = Join-Path $installDirectory 'LICENSE.txt'
$notices = Join-Path $installDirectory 'THIRD-PARTY-NOTICES.txt'
$installed = @($application, $torrplay, $uninstaller, $shortcut, $desktopShortcut, $license, $notices,
    (Join-Path $installDirectory 'help\index.html'))
$uninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\TorrPlay'
$helpShortcutSubKey = 'Software\org.torrplay.desktop\OfflineHelp'
$helpShortcutKey = "HKCU:\$helpShortcutSubKey"
# Tauri remembers the directory in the default value under its publisher and product name.
$installLocationKey = 'Software\TorrPlay\TorrPlay'
$savedInstallLocation = $null
$restoreInstallLocation = $false
$port = 18642
$running = $null
$data = $null
$testInstallDirectory = $null
$customDirectory = $null
$oldDirectory = $null
$shortcutFixtureDirectory = $null
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
function Invoke-Setup([string] $Directory = '', [switch] $NoShortcuts) {
    # NSIS requires /D= last and takes the rest of the command line literally.
    $arguments = '/S'
    if ($NoShortcuts) { $arguments += ' /NS' }
    if ($Directory) { $arguments += " /D=$Directory" }
    (Start-Process -FilePath $Setup -ArgumentList $arguments -Wait -PassThru).ExitCode
}

function Get-SavedInstallLocation {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($installLocationKey)
    if ($null -eq $key) { return }
    try {
        if ($key.GetValueNames() -contains '') {
            return [pscustomobject]@{
                Value = $key.GetValue('', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
                Kind = $key.GetValueKind('')
            }
        }
    } finally {
        $key.Dispose()
    }
}

function Set-SavedInstallLocation($Saved) {
    $key = if ($null -eq $Saved) {
        [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($installLocationKey, $true)
    } else {
        [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($installLocationKey)
    }
    if ($null -eq $key) { return }
    try {
        if ($null -eq $Saved) {
            $key.DeleteValue('', $false)
        } else {
            $key.SetValue('', $Saved.Value, $Saved.Kind)
        }
    } finally {
        $key.Dispose()
    }
}

function Assert-NotInstalled([string] $Why, [switch] $KeepHelpShortcuts) {
    if (Test-Path -Path $uninstallKey) { Fail "the application is registered $Why" }
    if (Test-Path -LiteralPath $helpShortcutKey) { Fail "the Help shortcut registry key exists $Why" }
    $paths = $installed + $sharedHelpShortcuts
    if (-not $KeepHelpShortcuts) { $paths += $helpShortcuts }
    foreach ($path in $paths) {
        if (Test-Path -LiteralPath $path) { Fail "$path exists $Why" }
    }
}

function Read-CliHelp([string] $Executable, [string] $WorkingDirectory) {
    $start = [System.Diagnostics.ProcessStartInfo]::new($Executable)
    $start.UseShellExecute = $false
    $start.WorkingDirectory = $WorkingDirectory
    $start.RedirectStandardOutput = $true
    $start.StandardOutputEncoding = [System.Text.Encoding]::UTF8
    $start.ArgumentList.Add('--help')
    $cli = [System.Diagnostics.Process]::Start($start)
    try {
        $output = $cli.StandardOutput.ReadToEndAsync()
        if (-not $cli.WaitForExit(10000)) {
            $cli.Kill()
            $cli.WaitForExit()
            Fail 'torrplay.exe --help did not exit'
        }
        if ($cli.ExitCode -ne 0) { Fail "torrplay.exe --help exited with $($cli.ExitCode)" }
        $output.GetAwaiter().GetResult() -split '\r?\n'
    } finally {
        $cli.Dispose()
    }
}

function Initialize-ShortcutInterop {
    # Use the Unicode Windows shell interface for shortcut paths and targets.
    if (-not ('TorrPlaySmoke.ShellLink' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
using System.Text;

namespace TorrPlaySmoke
{
    [ComImport, Guid("000214F9-0000-0000-C000-000000000046")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    internal interface IShellLinkW
    {
        // Keep the intervening methods in native vtable order through SetPath.
        [PreserveSig]
        int GetPath([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder path,
                    int size, IntPtr findData, uint flags);
        void GetIDList(out IntPtr idList);
        void SetIDList(IntPtr idList);
        void GetDescription([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder description, int size);
        void SetDescription([MarshalAs(UnmanagedType.LPWStr)] string description);
        void GetWorkingDirectory([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder directory, int size);
        void SetWorkingDirectory([MarshalAs(UnmanagedType.LPWStr)] string directory);
        void GetArguments([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder arguments, int size);
        void SetArguments([MarshalAs(UnmanagedType.LPWStr)] string arguments);
        void GetHotkey(out short hotkey);
        void SetHotkey(short hotkey);
        void GetShowCmd(out int command);
        void SetShowCmd(int command);
        void GetIconLocation([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder icon, int size, out int index);
        void SetIconLocation([MarshalAs(UnmanagedType.LPWStr)] string icon, int index);
        void SetRelativePath([MarshalAs(UnmanagedType.LPWStr)] string path, uint reserved);
        void Resolve(IntPtr window, uint flags);
        void SetPath([MarshalAs(UnmanagedType.LPWStr)] string path);
    }

    public static class ShellLink
    {
        [DllImport("kernel32.dll")]
        public static extern ushort GetUserDefaultUILanguage();

        private static object Create()
        {
            return Activator.CreateInstance(Type.GetTypeFromCLSID(
                new Guid("00021401-0000-0000-C000-000000000046")));
        }

        public static string[] Read(string path)
        {
            const uint rawPath = 4;
            var link = Create();
            try
            {
                ((IPersistFile)link).Load(path, 0);
                var target = new StringBuilder(32768);
                var arguments = new StringBuilder(32768);
                var shortcut = (IShellLinkW)link;
                int result = shortcut.GetPath(target, target.Capacity, IntPtr.Zero, rawPath);
                Marshal.ThrowExceptionForHR(result);
                shortcut.GetArguments(arguments, arguments.Capacity);
                return new[] { target.ToString(), arguments.ToString() };
            }
            finally
            {
                Marshal.FinalReleaseComObject(link);
            }
        }

        public static void Write(string path, string target)
        {
            var link = Create();
            try
            {
                ((IShellLinkW)link).SetPath(target);
                ((IPersistFile)link).Save(path, true);
            }
            finally
            {
                Marshal.FinalReleaseComObject(link);
            }
        }
    }
}
'@
    }
}

function Assert-ShortcutTarget([string] $Path, [string] $HelpFile) {
    Initialize-ShortcutInterop
    $target, $arguments = [TorrPlaySmoke.ShellLink]::Read($Path)
    if ($target -ne $HelpFile -or $arguments) {
        Fail "expected the Help shortcut to point directly to '$HelpFile', got target '$target' with arguments '$arguments'"
    }
}

function Get-PreferredHelpShortcut {
    Initialize-ShortcutInterop
    $primaryLanguage = [TorrPlaySmoke.ShellLink]::GetUserDefaultUILanguage() -band 0x03ff
    if ($primaryLanguage -eq 0x19) { return $helpShortcuts[1] }
    $helpShortcuts[0]
}

function Assert-OfflineHelp([string] $Directory, [string] $Version, [string] $WorkingDirectory, [switch] $NoShortcuts) {
    $helpFile = Join-Path $Directory 'help\index.html'
    & python (Join-Path $PSScriptRoot 'check-reference.py') --nsis-help $helpFile --version $Version
    if ($LASTEXITCODE -ne 0) { Fail "the installed offline reference is invalid at $helpFile" }
    foreach ($path in $sharedHelpShortcuts) {
        if (Test-Path -LiteralPath $path) {
            Fail "the per-user installer added the shared Start menu shortcut $path"
        }
    }
    if ($NoShortcuts) {
        foreach ($path in (@($shortcut, $desktopShortcut) + $helpShortcuts)) {
            if (Test-Path -LiteralPath $path) { Fail "the /NS install created $path" }
        }
    } else {
        $links = @($helpShortcuts | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf })
        if ($links.Count -ne 1) { Fail "expected one per-user Help shortcut, found $($links.Count)" }
        $expected = Get-PreferredHelpShortcut
        if ($links[0] -ne $expected) { Fail "expected the localized Help shortcut $expected, got $($links[0])" }
        Assert-ShortcutTarget $links[0] $helpFile
    }

    $output = Read-CliHelp (Join-Path $Directory 'torrplay.exe') $WorkingDirectory
    if ("Offline help: $helpFile" -cnotin $output) {
        Fail "the installed CLI did not print the absolute reference path $helpFile"
    }
}

function Invoke-Uninstaller([string] $Directory) {
    $remove = Join-Path $Directory 'uninstall.exe'
    # Wait for the copied uninstaller too, without blocking the test on a hang.
    $job = Start-Job -ArgumentList $remove -ScriptBlock {
        $ErrorActionPreference = 'Stop'
        Start-Process -FilePath $args[0] -ArgumentList '/S' -Wait
    }
    try {
        if (-not (Wait-Job -Job $job -Timeout 60)) { Fail 'the uninstaller did not exit' }
        Receive-Job -Job $job
    } finally {
        Remove-Job -Job $job -Force
    }
}

function Uninstall-TestApplication([string] $Directory = $installDirectory, [switch] $KeepHelpShortcuts) {
    $executable = Join-Path $Directory 'torrplay-desktop.exe'
    Invoke-Uninstaller $Directory
    if (-not (Wait-Until { -not (Test-Path -Path $uninstallKey) -and -not (Test-Path -LiteralPath $executable) } 60)) {
        Fail 'the uninstaller did not remove the application'
    }
    $removedShortcuts = $sharedHelpShortcuts
    if (-not $KeepHelpShortcuts) { $removedShortcuts += $helpShortcuts }
    if (-not (Wait-Until {
        -not (Test-Path -LiteralPath $Directory) -and
            -not ($removedShortcuts | Where-Object { Test-Path -LiteralPath $_ })
    } 30)) {
        Fail "the uninstaller left $Directory or a Help shortcut"
    }
    Assert-NotInstalled 'after the uninstall' -KeepHelpShortcuts:$KeepHelpShortcuts
}

function Remove-TestShortcuts {
    # Preflight verified these paths were empty before setup ran. Remove links
    # left by a failed test even if their stored targets are invalid.
    foreach ($path in (@($shortcut, $desktopShortcut) + $helpShortcuts + $sharedHelpShortcuts)) {
        try {
            if (Test-Path -LiteralPath $path -PathType Leaf) {
                Remove-Item -LiteralPath $path
            }
        } catch {
            Write-Warning "Could not clean up the test shortcut at ${path}: $_"
        }
    }
}

$previousDownloadDirectory = $env:TORRPLAY_DOWNLOAD_DIR
try {
    $env:TORRPLAY_DOWNLOAD_DIR = $null
    Assert-NotInstalled 'before the test'
    if (Test-Path -LiteralPath $settings) { Fail 'desktop startup settings already exist before the test' }
    $savedInstallLocation = Get-SavedInstallLocation
    $restoreInstallLocation = $true
    Set-SavedInstallLocation $null

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
        $testInstallDirectory = $installDirectory
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

    $data = (New-Item -ItemType Directory -Path (Join-Path ([System.IO.Path]::GetTempPath()) "torrplay-nsis-$PID")).FullName
    Assert-OfflineHelp $installDirectory $version $data
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

    Write-Host '== offline help in a custom installation'
    $customDirectory = Join-Path $data 'TorrPlay Média 雪'
    $testInstallDirectory = $customDirectory
    $code = Invoke-Setup $customDirectory
    if ($code -ne 0) { Fail "the custom install exited with $code" }
    Assert-OfflineHelp $customDirectory $version $data

    $loose = Join-Path $data 'loose'
    New-Item -ItemType Directory -Path $loose | Out-Null
    Copy-Item -LiteralPath (Join-Path $customDirectory 'torrplay.exe') -Destination $loose
    $output = Read-CliHelp (Join-Path $loose 'torrplay.exe') $customDirectory
    if ($output -clike 'Offline help:*') { Fail 'a loose CLI used the help file in its working directory' }

    Write-Host '== update the Help shortcut after reinstalling in a new directory'
    $oldDirectory = $customDirectory
    $customDirectory = Join-Path $data 'TorrPlay moved 雪'
    $testInstallDirectory = $customDirectory
    $code = Invoke-Setup $customDirectory
    if ($code -ne 0) { Fail "the setup in the new directory exited with $code" }
    Assert-OfflineHelp $customDirectory $version $data

    Invoke-Uninstaller $oldDirectory
    if (Test-Path -LiteralPath $oldDirectory) { Fail "the old uninstaller left $oldDirectory" }
    $oldDirectory = $null
    Assert-OfflineHelp $customDirectory $version $data
    Uninstall-TestApplication $customDirectory

    Write-Host '== relocate the owned shortcut when the preferred name is occupied'
    $code = Invoke-Setup $customDirectory
    if ($code -ne 0) { Fail "the install for the language change test exited with $code" }
    $preferredLink = Get-PreferredHelpShortcut
    $alternateLink = $helpShortcuts | Where-Object { $_ -ne $preferredLink }
    $preferredName = [IO.Path]::GetFileNameWithoutExtension($preferredLink)
    $alternateName = [IO.Path]::GetFileNameWithoutExtension($alternateLink)

    # Model an installation created under the other display language without
    # changing the Windows runner's settings. The rename keeps the file ID.
    Move-Item -LiteralPath $preferredLink -Destination $alternateLink
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($helpShortcutSubKey, $true)
    if ($null -eq $key) { Fail 'the installed application is missing its Help shortcut registration' }
    try {
        $key.SetValue($alternateName, $key.GetValue($preferredName))
        $key.DeleteValue($preferredName)
    } finally {
        $key.Dispose()
    }
    $otherTarget = Join-Path $env:SystemRoot 'explorer.exe'
    [TorrPlaySmoke.ShellLink]::Write($preferredLink, $otherTarget)
    $preferredHash = (Get-FileHash -LiteralPath $preferredLink).Hash

    $newDirectory = Join-Path $data 'TorrPlay alternate 雪'
    Move-Item -LiteralPath $customDirectory -Destination $newDirectory
    $customDirectory = $newDirectory
    $testInstallDirectory = $customDirectory
    $code = Invoke-Setup $customDirectory
    if ($code -ne 0) { Fail "the setup with an occupied shortcut name exited with $code" }
    Assert-ShortcutTarget $alternateLink (Join-Path $customDirectory 'help\index.html')
    Uninstall-TestApplication $customDirectory -KeepHelpShortcuts
    if (Test-Path -LiteralPath $alternateLink) { Fail "uninstall left the owned shortcut $alternateLink" }
    if ((Get-FileHash -LiteralPath $preferredLink).Hash -ne $preferredHash) { Fail "setup or uninstall changed $preferredLink" }
    Remove-Item -LiteralPath $preferredLink

    $programs = [Environment]::GetFolderPath('Programs')
    $fixture = New-Item -ItemType Directory -Path (Join-Path $programs "torrplay-nsis-$PID")
    $shortcutFixtureDirectory = $fixture.FullName
    foreach ($replacement in 'copy', 'symbolic link', 'hard link') {
        Write-Host "== preserve a $replacement used to replace the installed Help shortcut"
        $code = Invoke-Setup $customDirectory
        if ($code -ne 0) { Fail "the install for the replacement test exited with $code" }
        $replacedLink = $helpShortcuts | Where-Object { Test-Path -LiteralPath $_ }
        $originalLink = Join-Path $shortcutFixtureDirectory "original-help-$replacement.lnk"
        Move-Item -LiteralPath $replacedLink -Destination $originalLink
        if ($replacement -eq 'symbolic link') {
            New-Item -ItemType SymbolicLink -Path $replacedLink -Target $originalLink | Out-Null
        } elseif ($replacement -eq 'hard link') {
            New-Item -ItemType HardLink -Path $replacedLink -Target $originalLink | Out-Null
        } else {
            Copy-Item -LiteralPath $originalLink -Destination $replacedLink
        }
        Uninstall-TestApplication $customDirectory -KeepHelpShortcuts
        if ((Get-FileHash -LiteralPath $replacedLink).Hash -ne (Get-FileHash -LiteralPath $originalLink).Hash) {
            Fail "uninstall changed the replacement shortcut $replacedLink"
        }
        Remove-Item -LiteralPath $replacedLink
    }

    Write-Host '== use the alternate Start menu shortcut when the preferred name is occupied'
    [TorrPlaySmoke.ShellLink]::Write($preferredLink, $otherTarget)
    $preferredHash = (Get-FileHash -LiteralPath $preferredLink).Hash
    $code = Invoke-Setup $customDirectory
    if ($code -ne 0) { Fail "the install with an existing shortcut exited with $code" }
    Assert-ShortcutTarget $alternateLink (Join-Path $customDirectory 'help\index.html')
    Uninstall-TestApplication $customDirectory -KeepHelpShortcuts
    if (Test-Path -LiteralPath $alternateLink) { Fail "uninstall left the owned shortcut $alternateLink" }
    if ((Get-FileHash -LiteralPath $preferredLink).Hash -ne $preferredHash) { Fail "setup or uninstall changed $preferredLink" }
    Remove-Item -LiteralPath $preferredLink

    Write-Host '== offline help without shortcuts'
    $code = Invoke-Setup $customDirectory -NoShortcuts
    if ($code -ne 0) { Fail "the /NS install exited with $code" }
    Assert-OfflineHelp $customDirectory $version $data -NoShortcuts
    $originalLinks = @{}
    foreach ($path in $helpShortcuts) {
        [TorrPlaySmoke.ShellLink]::Write($path, (Join-Path $customDirectory 'help\index.html'))
        $originalLinks[$path] = (Get-FileHash -LiteralPath $path).Hash
    }
    Uninstall-TestApplication $customDirectory -KeepHelpShortcuts
    foreach ($path in $helpShortcuts) {
        if ((Get-FileHash -LiteralPath $path).Hash -ne $originalLinks[$path]) { Fail "uninstall changed $path" }
        Remove-Item -LiteralPath $path
    }

    Write-Host '== retry cleanup of an owned shortcut that was locked during uninstall'
    $code = Invoke-Setup $customDirectory
    if ($code -ne 0) { Fail "the install for the locked shortcut test exited with $code" }
    $lockedLink = [IO.File]::Open($preferredLink, 'Open', 'Read', 'Read')
    try {
        Invoke-Uninstaller $customDirectory
        if (Test-Path -LiteralPath $customDirectory) { Fail "uninstall left $customDirectory" }
        if (-not (Test-Path -LiteralPath $preferredLink)) { Fail 'the test lock did not prevent shortcut deletion' }
        $customDirectory = Join-Path $data 'TorrPlay after locked shortcut 雪'
        $testInstallDirectory = $customDirectory
        $code = Invoke-Setup $customDirectory
        if ($code -ne 0) { Fail "the install while the shortcut was locked exited with $code" }
        Assert-ShortcutTarget $alternateLink (Join-Path $customDirectory 'help\index.html')
    } finally {
        $lockedLink.Dispose()
    }
    Uninstall-TestApplication $customDirectory

    Write-Host 'all installer tests passed'
} finally {
    $env:TORRPLAY_DOWNLOAD_DIR = $previousDownloadDirectory
    if ($running) {
        Stop-Process -Id $running.Id -ErrorAction SilentlyContinue
    }
    if ($testInstallDirectory) {
        Remove-TestShortcuts
        foreach ($directory in @($oldDirectory, $testInstallDirectory)) {
            if (-not $directory -or -not (Test-Path -LiteralPath (Join-Path $directory 'uninstall.exe'))) { continue }
            try {
                Invoke-Uninstaller $directory
            } catch {
                Write-Warning "Could not uninstall the test installation at ${directory}: $_"
            }
        }
        Remove-TestShortcuts
        if (Test-Path -LiteralPath $helpShortcutKey) {
            Remove-Item -LiteralPath $helpShortcutKey -ErrorAction SilentlyContinue
        }
    }
    if ($restoreInstallLocation) { Set-SavedInstallLocation $savedInstallLocation }
    if ($shortcutFixtureDirectory) {
        Remove-Item -LiteralPath $shortcutFixtureDirectory -Recurse -Force -ErrorAction SilentlyContinue
    }
    $keepTestData = ($customDirectory -and (Test-Path -LiteralPath $customDirectory)) -or
        ($oldDirectory -and (Test-Path -LiteralPath $oldDirectory))
    if ($data -and -not $keepTestData) {
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
