# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Defines [TorrPlayServiceConfig], which sets and reads a service's preshutdown
# timeout through ChangeServiceConfig2 and QueryServiceConfig2, the service
# manager's documented calls for it, so the install sets it the way the service
# manager stores it and the test reads back what the service manager reports.
# Dot-source it:
#
#   . "$PSScriptRoot\windows-service-config.ps1"

if (-not ('TorrPlayServiceConfig' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;

public static class TorrPlayServiceConfig
{
    [StructLayout(LayoutKind.Sequential)]
    struct PreshutdownInfo { public uint Timeout; }

    const uint ManagerConnect = 0x0001;
    const uint QueryConfig = 0x0001;
    const uint ChangeConfig = 0x0002;
    const uint PreshutdownInfoLevel = 7;

    [DllImport("advapi32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern IntPtr OpenSCManagerW(string machine, string database, uint access);

    [DllImport("advapi32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern IntPtr OpenServiceW(IntPtr manager, string name, uint access);

    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool ChangeServiceConfig2W(IntPtr service, uint level, ref PreshutdownInfo info);

    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool QueryServiceConfig2W(
        IntPtr service, uint level, out PreshutdownInfo info, uint size, out uint needed);

    [DllImport("advapi32.dll")]
    static extern bool CloseServiceHandle(IntPtr handle);

    static T WithService<T>(string name, uint access, Func<IntPtr, T> use)
    {
        IntPtr manager = OpenSCManagerW(null, null, ManagerConnect);
        if (manager == IntPtr.Zero) throw new Win32Exception();
        try
        {
            IntPtr service = OpenServiceW(manager, name, access);
            if (service == IntPtr.Zero) throw new Win32Exception();
            try { return use(service); }
            finally { CloseServiceHandle(service); }
        }
        finally { CloseServiceHandle(manager); }
    }

    public static void SetPreshutdownTimeout(string name, uint milliseconds)
    {
        WithService(name, ChangeConfig, service =>
        {
            var info = new PreshutdownInfo { Timeout = milliseconds };
            if (!ChangeServiceConfig2W(service, PreshutdownInfoLevel, ref info))
                throw new Win32Exception();
            return true;
        });
    }

    public static uint GetPreshutdownTimeout(string name)
    {
        return WithService(name, QueryConfig, service =>
        {
            PreshutdownInfo info;
            uint needed;
            if (!QueryServiceConfig2W(service, PreshutdownInfoLevel, out info,
                    (uint)Marshal.SizeOf(typeof(PreshutdownInfo)), out needed))
                throw new Win32Exception();
            return info.Timeout;
        });
    }
}
'@
}
