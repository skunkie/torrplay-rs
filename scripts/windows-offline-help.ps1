# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Offline help checks for the Windows installer tests.

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

function Initialize-OfflineHelpInterop {
    if (-not ('TorrPlaySmoke.ShellLink' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
using System.Security;
using System.Security.Principal;
using System.Text;
using Microsoft.Win32.SafeHandles;

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

    public static class StandardUserHelp
    {
        [DllImport("advapi32.dll", CharSet = CharSet.Unicode, ExactSpelling = true, SetLastError = true)]
        private static extern bool LogonUserW(string user, string domain, IntPtr password,
            int logonType, int provider, out SafeAccessTokenHandle token);

        public static void CheckFiles(string user, string domain, SecureString password,
            string expectedSid, string shortcut, string helpFile)
        {
            const int interactiveLogon = 2;
            const int defaultProvider = 0;
            SafeAccessTokenHandle token;
            IntPtr secret = Marshal.SecureStringToGlobalAllocUnicode(password);
            try
            {
                if (!LogonUserW(user, domain, secret, interactiveLogon, defaultProvider, out token))
                {
                    int error = Marshal.GetLastWin32Error();
                    token?.Dispose();
                    throw new Win32Exception(error);
                }
            }
            finally
            {
                Marshal.ZeroFreeGlobalAllocUnicode(secret);
            }

            using (token)
            {
                WindowsIdentity.RunImpersonated(token, () =>
                {
                    using var identity = WindowsIdentity.GetCurrent();
                    if (identity.User?.Value != expectedSid ||
                        new WindowsPrincipal(identity).IsInRole(WindowsBuiltInRole.Administrator))
                    {
                        throw new InvalidOperationException("The help check must run as the standard test user.");
                    }

                    foreach (string path in new[] { shortcut, helpFile })
                    {
                        using var stream = File.OpenRead(path);
                        if (stream.ReadByte() < 0)
                        {
                            throw new IOException($"The installed file is empty at {path}.");
                        }
                    }
                });
            }
        }
    }
}
'@
    }
}

function Assert-ShortcutTarget([string] $Path, [string] $HelpFile) {
    Initialize-OfflineHelpInterop
    $target, $arguments = [TorrPlaySmoke.ShellLink]::Read($Path)
    if ($target -ne $HelpFile -or $arguments) {
        Fail "expected the Help shortcut to point directly to '$HelpFile', got target '$target' with arguments '$arguments'"
    }
}
