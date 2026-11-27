# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

param(
    [Parameter(Mandatory)]
    [ValidateSet('Application', 'Service')]
    [string] $Edition,
    [Parameter(Mandatory)]
    [string] $Installer,
    [Parameter(Mandatory)]
    [string] $DownloadDirectory,
    [Parameter(Mandatory)]
    [string] $AllowedHosts,
    [string] $InstallDirectory,
    [ValidateSet('Settings', 'Copy')]
    [string] $ApplicationFailure,
    [string] $UnwritableDownloadDirectory,
    [string] $DownloadCollision,
    [switch] $ExpectServiceStartFailure,
    [switch] $UseDefaults
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not [Environment]::UserInteractive) {
    throw 'The installer wizard test requires a logged-in Windows desktop.'
}
if ($PSVersionTable.PSEdition -eq 'Core') {
    # Windows PowerShell loads the .NET Framework UI Automation assemblies.
    $arguments = @('-NoProfile', '-NonInteractive', '-MTA', '-ExecutionPolicy', 'Bypass',
        '-File', "`"$PSCommandPath`"", '-Edition', $Edition, '-Installer', "`"$Installer`"",
        '-DownloadDirectory', "`"$DownloadDirectory`"", '-AllowedHosts', "`"$AllowedHosts`"")
    if ($ApplicationFailure) { $arguments += @('-ApplicationFailure', $ApplicationFailure) }
    if ($InstallDirectory) { $arguments += @('-InstallDirectory', "`"$InstallDirectory`"") }
    if ($ExpectServiceStartFailure) { $arguments += '-ExpectServiceStartFailure' }
    if ($UseDefaults) { $arguments += '-UseDefaults' }
    if ($DownloadCollision) {
        $arguments += @('-DownloadCollision', "`"$DownloadCollision`"")
    }
    if ($UnwritableDownloadDirectory) {
        $arguments += @('-UnwritableDownloadDirectory', "`"$UnwritableDownloadDirectory`"")
    }
    $automation = Start-Process -NoNewWindow -PassThru `
        -FilePath "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" `
        -ArgumentList $arguments
    if (-not $automation.WaitForExit(240000)) {
        $automation.Kill($true)
        throw "The $Edition installer wizard test timed out."
    }
    if ($automation.ExitCode -ne 0) {
        throw "The $Edition installer wizard test failed."
    }
    return
}

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, WindowsBase
# Invoke on a worker because a button that opens a modal dialog can block
# until the dialog closes. UI Automation works without an active button window.
$automationAssemblies = @(
    [System.Windows.Automation.AutomationElement].Assembly.Location
    [System.Windows.Automation.InvokePatternIdentifiers].Assembly.Location
    [System.Windows.Rect].Assembly.Location
    [ComponentModel.Win32Exception].Assembly.Location
)
Add-Type -ReferencedAssemblies $automationAssemblies -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Threading.Tasks;
using System.Windows.Automation;

namespace TorrPlaySmoke {
    public static class Buttons {
        public static Task InvokeAsync(AutomationElement button) {
            return Task.Run(() => ((InvokePattern)button.GetCurrentPattern(InvokePattern.Pattern)).Invoke());
        }
    }

    public static class Windows {
        private delegate bool WindowCallback(IntPtr window, IntPtr data);
        [DllImport("user32.dll")]
        private static extern bool EnumWindows(WindowCallback callback, IntPtr data);
        [DllImport("user32.dll")]
        private static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
        [DllImport("user32.dll")]
        private static extern bool IsWindowVisible(IntPtr window);
        [DllImport("user32.dll")]
        private static extern IntPtr GetParent(IntPtr window);
        [DllImport("user32.dll")]
        private static extern int GetDlgCtrlID(IntPtr window);
        [DllImport("user32.dll", SetLastError = true)]
        private static extern bool PostMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);

        public static IntPtr[] VisibleWindows(uint processId) {
            var matches = new List<IntPtr>();
            EnumWindows((window, data) => {
                uint owner;
                GetWindowThreadProcessId(window, out owner);
                if (owner == processId && IsWindowVisible(window)) matches.Add(window);
                return true;
            }, IntPtr.Zero);
            return matches.ToArray();
        }

        public static void PostClick(IntPtr button) {
            // NSIS handles WM_COMMAND with BN_CLICKED in the parent dialog.
            const uint command = 0x0111;
            var parent = GetParent(button);
            var id = GetDlgCtrlID(button);
            if (parent == IntPtr.Zero || id == 0) throw new InvalidOperationException("Button has no dialog control ID.");
            if (!PostMessage(parent, command, new IntPtr(id & 0xffff), button)) throw new Win32Exception();
        }

    }
}
'@
$pendingClicks = [Collections.Generic.List[Threading.Tasks.Task]]::new()

function Get-InstallerWindows {
    $owner = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ProcessIdProperty, $setup.Id)
    $seen = [Collections.Generic.HashSet[IntPtr]]::new()
    foreach ($window in [System.Windows.Automation.AutomationElement]::RootElement.FindAll(
            [System.Windows.Automation.TreeScope]::Children, $owner)) {
        if (-not $window.Current.IsOffscreen) {
            [void] $seen.Add([IntPtr] $window.Current.NativeWindowHandle)
            $window
        }
    }
    # Discover owned dialogs the desktop provider leaves out. Prefer that
    # provider's elements when available, since they expose child controls.
    foreach ($handle in [TorrPlaySmoke.Windows]::VisibleWindows($setup.Id)) {
        if (-not $seen.Add($handle)) { continue }
        try {
            [System.Windows.Automation.AutomationElement]::FromHandle($handle)
        } catch [System.Windows.Automation.ElementNotAvailableException] {
            # The window closed between enumeration and lookup.
        }
    }
}

function Find-Controls($Window, $Type, [string] $Name = '') {
    $typeFilter = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $Type)
    $Window.FindAll([System.Windows.Automation.TreeScope]::Descendants, $typeFilter) |
        Where-Object {
            -not $_.Current.IsOffscreen -and $_.Current.IsEnabled -and
            (-not $Name -or ($_.Current.Name -replace '&', '') -match $Name)
        }
}

function Wait-Control($Type, [string] $Name, [string] $Page = '', [int] $Seconds = 30) {
    $timeout = [Diagnostics.Stopwatch]::StartNew()
    do {
        for ($index = $pendingClicks.Count - 1; $index -ge 0; $index--) {
            if ($pendingClicks[$index].IsCompleted) {
                $pendingClicks[$index].GetAwaiter().GetResult()
                $pendingClicks.RemoveAt($index)
            }
        }
        if ($setup.HasExited) {
            throw "The installer exited with $($setup.ExitCode) while waiting for '$Name'."
        }
        try {
            foreach ($window in Get-InstallerWindows) {
                if ($Page -and $window.Current.Name -notmatch $Page -and
                    -not (Find-Controls $window ([System.Windows.Automation.ControlType]::Text) $Page)) {
                    continue
                }
                $control = Find-Controls $window $Type $Name | Select-Object -First 1
                if ($control) { return $control }
            }
        } catch [System.Windows.Automation.ElementNotAvailableException] {
            # The installer can replace a dialog while it is being enumerated.
        }
        Start-Sleep -Milliseconds 200
    } while ($timeout.Elapsed.TotalSeconds -lt $Seconds)
    foreach ($window in Get-InstallerWindows) {
        Write-Host "Installer window: $($window.Current.Name)"
        foreach ($typeToReport in @([System.Windows.Automation.ControlType]::Text, [System.Windows.Automation.ControlType]::Button)) {
            foreach ($control in (Find-Controls $window $typeToReport | Select-Object -First 12)) {
                Write-Host "  $($typeToReport.ProgrammaticName): $($control.Current.Name)"
            }
        }
    }
    throw "Could not find '$Name' on the installer page '$Page'."
}

function Click-Button([string] $Name, [string] $Page = '', [int] $Seconds = 30) {
    $button = Wait-Control ([System.Windows.Automation.ControlType]::Button) $Name $Page $Seconds
    if ($Edition -eq 'Application' -and $button.Current.NativeWindowHandle -ne 0) {
        [TorrPlaySmoke.Windows]::PostClick([IntPtr] $button.Current.NativeWindowHandle)
    } else {
        $button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    }
}

function Set-Checkbox([string] $Name, [bool] $Checked) {
    $box = Wait-Control ([System.Windows.Automation.ControlType]::CheckBox) $Name
    $toggle = $box.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
    if (($toggle.Current.ToggleState -eq [System.Windows.Automation.ToggleState]::On) -ne $Checked) {
        $toggle.Toggle()
    }
}

function Get-SettingsFields([string] $Page) {
    $browse = Wait-Control ([System.Windows.Automation.ControlType]::Button) '^Browse' $Page
    foreach ($window in Get-InstallerWindows) {
        if (Find-Controls $window ([System.Windows.Automation.ControlType]::Text) $Page) {
            $fields = @(Find-Controls $window ([System.Windows.Automation.ControlType]::Edit) |
                Sort-Object { $_.Current.BoundingRectangle.Top })
            if ($fields.Count -ne 2) { throw "Expected two startup settings fields on '$Page'." }
            return @{
                Directory = $fields[0]
                Hosts = $fields[1]
                Browse = $browse
            }
        }
    }
    throw "Could not find the settings fields on '$Page'."
}

function Set-Field($Control, [string] $Value) {
    $Control.SetFocus()
    $pattern = $Control.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
    $pattern.SetValue($Value)
    if ($pattern.Current.Value -cne $Value) { throw "The field did not accept the value '$Value'." }
}

function Post-ButtonClick($Button) {
    if ($Edition -eq 'Application') {
        [TorrPlaySmoke.Windows]::PostClick([IntPtr] $Button.Current.NativeWindowHandle)
    } else {
        $pendingClicks.Add([TorrPlaySmoke.Buttons]::InvokeAsync($Button))
    }
}

$log = $null
$setup = $null
try {
    $next = '^Next\s*>?$'
    $buttonType = [System.Windows.Automation.ControlType]::Button
    if ($Edition -eq 'Application') {
        $installerArguments = @{ FilePath = $Installer; PassThru = $true }
        $setup = Start-Process @installerArguments
        Click-Button $next '^Welcome to .* Setup$'
        Click-Button '^I Agree$'
        Click-Button $next '^Choose Install Location$'
        $page = '^TorrPlay settings$'
        if (-not $UseDefaults) {
            $fields = Get-SettingsFields $page
            if ($UnwritableDownloadDirectory) {
                # Verify that Browse opens the shell picker. The runner's
                # shell tree may omit the test account's temporary folders.
                Post-ButtonClick $fields.Browse
                Click-Button '^Cancel$' '^(Choose the TorrPlay download directory|Browse For Folder)$'
                $fields = Get-SettingsFields $page
            }
            Set-Field $fields.Directory $DownloadDirectory
        }
    } else {
        $log = [IO.Path]::GetTempFileName()
        $msiArguments = @(
            '/i', "`"$Installer`"", 'ADDLOCAL=Service', '/qf', '/norestart', '/l*v!', "`"$log`"")
        if ($InstallDirectory) { $msiArguments += "INSTALLDIR=`"$InstallDirectory`"" }
        # Seed the existing MSI properties because its Edit controls only
        # commit on focus loss, which is unreliable in unattended sessions.
        if (-not $UseDefaults) {
            $directory = $DownloadDirectory.TrimEnd('\')
            $msiArguments += @("TORRPLAY_SETUP_DOWNLOAD_DIR=`"$directory`"", "TORRPLAY_SETUP_ALLOWED_HOSTS=`"$AllowedHosts`"")
        }
        $setup = Start-Process -FilePath msiexec.exe -PassThru -ArgumentList $msiArguments
        Click-Button $next '^Welcome to the .* Setup Wizard$'
        Set-Checkbox '^I accept the terms in the License Agreement$' $true
        Click-Button $next '^End-User License Agreement$'
        Click-Button $next '^Custom Setup$'
        $page = '^TorrPlay service settings$'
        if (-not $UseDefaults -and $ExpectServiceStartFailure) {
            Click-Button '^Browse' $page
            Click-Button '^Cancel$' '^(Choose Download Directory|Look in)$'
        }
    }
    $fields = Get-SettingsFields $page
    $selected = $fields.Directory.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value
    $expectedDirectory = $DownloadDirectory
    if ($UseDefaults) { $expectedDirectory = '' }
    if ($selected.TrimEnd('\') -ne $expectedDirectory.TrimEnd('\')) {
        throw "Expected '$expectedDirectory' in the download directory field, got '$selected'."
    }
    $expectedHosts = $AllowedHosts
    if ($UseDefaults) {
        $expectedHosts = ''
    } elseif ($Edition -eq 'Application') {
        Set-Field $fields.Hosts $AllowedHosts
    }
    $shownHosts = $fields.Hosts.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value
    if ($shownHosts -cne $expectedHosts) { throw "Expected '$expectedHosts' in the allowed hostnames field, got '$shownHosts'." }

    if ($Edition -eq 'Application') {
        if ($DownloadCollision) {
            $original = [Convert]::ToBase64String([IO.File]::ReadAllBytes($DownloadCollision))
            $advance = Wait-Control $buttonType '^(Next\s*>?|Install)$' $page
            Post-ButtonClick $advance
            Click-Button '^OK$' '(?s)^Check the TorrPlay settings\..*contains existing TorrPlay data\.'
            if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($DownloadCollision)) -cne $original) {
                throw 'Setup changed existing data in the download directory.'
            }
            Remove-Item -LiteralPath $DownloadCollision
        } elseif ($UnwritableDownloadDirectory) {
            Set-Field $fields.Directory $UnwritableDownloadDirectory
            $advance = Wait-Control $buttonType '^(Next\s*>?|Install)$' $page
            Post-ButtonClick $advance
            $rejectedPath = [regex]::Escape($UnwritableDownloadDirectory)
            Click-Button '^OK$' "(?s)^Check the TorrPlay settings\..*The download directory $rejectedPath cannot be used"
            $fields = Get-SettingsFields $page
            Set-Field $fields.Directory $DownloadDirectory
        }
        Click-Button '^(Next\s*>?|Install)$' $page
        if ($ApplicationFailure) {
            if ($ApplicationFailure -eq 'Settings') {
                Click-Button '^OK$' '^Could not save the TorrPlay settings\.'
            } else {
                Click-Button '^Abort$' '^Error opening file for writing'
            }
            Click-Button '^(Close|Cancel)$' '^Installation Aborted$'
            if (-not $setup.WaitForExit(30000) -or $setup.ExitCode -eq 0) {
                throw "The installer did not report the expected $ApplicationFailure failure."
            }
            return
        }
        # Tauri disables auto-advance from the completed installation page.
        Click-Button $next '^Installation Complete$' 90
        $finishPage = '^Completing TorrPlay Setup$'
        # The smoke test starts the app with an isolated data directory.
        Wait-Control $buttonType '^Finish$' $finishPage 90 | Out-Null
        Set-Checkbox '^Run TorrPlay$' $false
    } else {
        Click-Button $next $page
        $readyPage = '^Ready to install TorrPlay Service$'
        if ($ExpectServiceStartFailure) {
            $installButton = Wait-Control $buttonType '^Install$' $readyPage
            Post-ButtonClick $installButton
            $startupError = "Service 'TorrPlay Service' \(torrplay\) failed to start"
            $failedPage = '^TorrPlay Service Setup Wizard (ended prematurely|was interrupted)$'
            $failure = Wait-Control $buttonType '^(Abort|Cancel|OK|Finish)$' "$startupError|$failedPage" 90
            if (($failure.Current.Name -replace '&', '') -ne 'Finish') {
                $failure.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
                $failure = Wait-Control $buttonType '^Finish$' $failedPage 90
            }
            $failure.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
            if (-not $setup.WaitForExit(30000) -or $setup.ExitCode -notin @(1602, 1603)) {
                throw 'The MSI did not fail after it could not start the service.'
            }
            return
        }
        Click-Button '^Install$' $readyPage
        $finishPage = '^Completed the TorrPlay Service Setup Wizard$'
    }
    Click-Button '^Finish$' $finishPage 90
    if (-not $setup.WaitForExit(30000)) { throw 'The installer did not exit after Finish.' }
    if ($setup.ExitCode -notin @(0, 3010)) { throw "The installer exited with $($setup.ExitCode)." }
} catch {
    if ($log) { Get-Content -LiteralPath $log -Tail 80 | Write-Host }
    throw
} finally {
    if ($setup -and -not $setup.HasExited) { $setup.Kill() }
    if ($log) { Remove-Item -LiteralPath $log -ErrorAction SilentlyContinue }
}
