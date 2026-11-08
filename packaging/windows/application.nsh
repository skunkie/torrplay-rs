; SPDX-FileCopyrightText: 2026 TorrPlay
;
; SPDX-License-Identifier: MIT

; The application's NSIS installer hooks, which desktop/tauri.bundle.conf.json
; names. One computer runs one edition, as ADR 13 decides, so the installer
; refuses while the service edition's MSI is installed for the computer.
;
; Windows Installer registers a product installed for the computer under its
; upgrade code, packed: each of the code's first three groups reversed, then
; the two characters of each byte after them swapped. This is the packed form
; of the upgradeCode in desktop/tauri.client.conf.json, which
; scripts/test-application-nsis.ps1 checks.
!define TORRPLAY_SERVICE_UPGRADE_KEY "SOFTWARE\Classes\Installer\UpgradeCodes\429B3AFCDCF29C7448334311C00CF8B9"

; Runs before any file is copied. The template has already set the 64-bit
; registry view on x64 and arm64, which the MSI writes to; an Abort here ends
; the install with exit code 2.
!macro NSIS_HOOK_PREINSTALL
  ; $0 is the template's too, so it is kept.
  Push $0
  EnumRegValue $0 HKLM "${TORRPLAY_SERVICE_UPGRADE_KEY}" 0
  ${If} $0 != ""
    MessageBox MB_ICONSTOP|MB_OK "The service edition of TorrPlay, TorrPlay Client, is installed on this computer, and a computer runs one edition. To install the application instead, an administrator must first uninstall TorrPlay Client in Settings, Apps." /SD IDOK
    Abort "TorrPlay Client, the service edition, is installed on this computer."
  ${EndIf}
  Pop $0
!macroend
