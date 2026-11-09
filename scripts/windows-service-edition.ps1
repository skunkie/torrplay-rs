# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Defines Get-ServiceEditionKey, the registry key under which Windows Installer
# records the service edition's MSI once it is installed for the computer: its
# upgrade code, from desktop/tauri.client.conf.json, in Windows Installer's
# packed form, which packaging/windows/application.nsh looks up to refuse
# beside it. Dot-source it:
#
#   . "$PSScriptRoot\windows-service-edition.ps1"

function Get-ServiceEditionKey {
    $client = Get-Content -Raw -Path (Join-Path $PSScriptRoot '../desktop/tauri.client.conf.json') |
        ConvertFrom-Json
    # The first three groups reversed, then the two characters of each later
    # byte swapped.
    $hex = ([guid] $client.bundle.windows.wix.upgradeCode).ToString('N').ToUpperInvariant()
    $reverse = { param($Text) -join $Text[($Text.Length - 1)..0] }
    $packed = (& $reverse $hex.Substring(0, 8)) + (& $reverse $hex.Substring(8, 4)) +
        (& $reverse $hex.Substring(12, 4))
    for ($index = 16; $index -lt 32; $index += 2) {
        $packed += & $reverse $hex.Substring($index, 2)
    }
    "HKLM:\SOFTWARE\Classes\Installer\UpgradeCodes\$packed"
}
