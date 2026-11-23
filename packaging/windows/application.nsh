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

!include nsDialogs.nsh
!define TORRPLAY_SETTINGS_TIMEOUT 15000

Var TorrPlayDownloadDirectory
Var TorrPlayAllowedHosts
Var TorrPlayDownloadEdit
Var TorrPlayHostsEdit
Var TorrPlaySettingsAction
Var TorrPlaySettingsCreated
Var TorrPlayExistingInstall

!macro TorrPlayRunSettingsCheck arguments
  Delete "$PLUGINSDIR\torrplay-error.txt"
  ; Read errors separately so nsExec only has to pass ASCII status text.
  nsExec::ExecToStack /TIMEOUT=${TORRPLAY_SETTINGS_TIMEOUT} '"$PLUGINSDIR\torrplay-settings-check.exe" validate-installer-settings --nsis "$PLUGINSDIR\torrplay-error.txt" ${arguments}'
  Pop $0
  Pop $1
  ${If} $0 == "2"
    Push $2
    StrCpy $1 "The settings helper could not report the error."
    ClearErrors
    FileOpen $2 "$PLUGINSDIR\torrplay-error.txt" r
    ${IfNot} ${Errors}
      FileReadUTF16LE $2 $1
      FileClose $2
    ${EndIf}
    Pop $2
  ${EndIf}
!macroend

!define MUI_CUSTOMFUNCTION_GUIINIT TorrPlayDetectPreviousInstall

; Tauri's hook is included before its page list. Extend the Modern UI
; directory page declaration to insert the settings page immediately after it.
!macroundef MUI_PAGE_DIRECTORY
!macro MUI_PAGE_DIRECTORY
  !insertmacro MUI_PAGE_INIT
  !insertmacro MUI_PAGEDECLARATION_DIRECTORY
  !define TORRPLAY_DESKTOP_SETTINGS "$APPDATA\${BUNDLEID}\torrplay.env"
  !insertmacro TorrPlaySettingsFunctions
  Page custom TorrPlaySettingsPage TorrPlaySettingsLeave
!macroend

!macro TorrPlaySettingsFunctions
Function TorrPlayDetectPreviousInstall
  ; The reinstall page can run the old uninstaller before we get here.
  ReadRegStr $TorrPlayExistingInstall SHCTX "${UNINSTKEY}" "UninstallString"
FunctionEnd

Function .onGUIEnd
  ${If} $TorrPlaySettingsCreated == "saved"
    !insertmacro TorrPlayRunSettingsCheck '--rollback "$PLUGINSDIR\torrplay.env" "${TORRPLAY_DESKTOP_SETTINGS}"'
    ${If} $0 != 0
      ${If} $0 == "timeout"
        StrCpy $1 "The cleanup timed out."
      ${EndIf}
      MessageBox MB_OK|MB_ICONEXCLAMATION "Could not remove the incomplete TorrPlay settings.$\r$\n$1"
    ${EndIf}
  ${EndIf}
FunctionEnd

Function TorrPlaySettingsPage
  ${If} $PassiveMode = 1
  ${OrIf} $UpdateMode = 1
  ${OrIf} $TorrPlayExistingInstall != ""
    Abort
  ${EndIf}
  IfFileExists "${TORRPLAY_DESKTOP_SETTINGS}" 0 +2
    Abort

  !insertmacro MUI_HEADER_TEXT "TorrPlay settings" "Both fields are optional."
  nsDialogs::Create 1018
  Pop $0
  ${If} $0 == error
    Abort
  ${EndIf}

  ${NSD_CreateLabel} 0 0 100% 12u "Download directory"
  Pop $0
  ${NSD_CreateText} 0 14u 76% 12u "$TorrPlayDownloadDirectory"
  Pop $TorrPlayDownloadEdit
  SendMessage $TorrPlayDownloadEdit ${EM_LIMITTEXT} 1023 0
  ${NSD_CreateButton} 78% 13u 22% 14u "Browse..."
  Pop $0
  ${NSD_OnClick} $0 TorrPlayBrowseDownloads
  ${NSD_CreateLabel} 0 30u 100% 23u "Use a separate folder with no existing TorrPlay data. The default is downloads in the TorrPlay data directory."
  Pop $0

  ${NSD_CreateLabel} 0 56u 100% 12u "Allowed hostnames"
  Pop $0
  ${NSD_CreateText} 0 70u 100% 12u "$TorrPlayAllowedHosts"
  Pop $TorrPlayHostsEdit
  SendMessage $TorrPlayHostsEdit ${EM_LIMITTEXT} 1023 0
  ${NSD_CreateLabel} 0 87u 100% 28u "Enter the hostnames you use to open TorrPlay. Separate names with commas. Use *.home.arpa to allow names like nas.home.arpa."
  Pop $0
  nsDialogs::Show
FunctionEnd

Function TorrPlayBrowseDownloads
  Pop $0
  ${NSD_GetText} $TorrPlayDownloadEdit $0
  nsDialogs::SelectFolderDialog "Choose the TorrPlay download directory" "$0"
  Pop $0
  ${If} $0 != error
    ${NSD_SetText} $TorrPlayDownloadEdit "$0"
  ${EndIf}
FunctionEnd

Function TorrPlaySettingsLeave
  StrCpy $TorrPlaySettingsAction ""
  ${NSD_GetText} $TorrPlayDownloadEdit $TorrPlayDownloadDirectory
  ${NSD_GetText} $TorrPlayHostsEdit $TorrPlayAllowedHosts
  InitPluginsDir
  ClearErrors
  FileOpen $0 "$PLUGINSDIR\torrplay.env" w
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONEXCLAMATION "Could not prepare the TorrPlay settings."
    Abort
  ${EndIf}
  ; Distinguish these bytes from settings written by another setup attempt.
  System::Call 'kernel32::GetCurrentProcessId() i .r1'
  FileWriteUTF16LE /BOM $0 "# TorrPlay desktop startup settings from setup process $1.$\r$\n"
  ${If} $TorrPlayDownloadDirectory != ""
    FileWriteUTF16LE $0 "TORRPLAY_DOWNLOAD_DIR="
    FileWriteUTF16LE $0 "$TorrPlayDownloadDirectory"
    FileWriteUTF16LE $0 "$\r$\n"
  ${EndIf}
  ${If} $TorrPlayAllowedHosts != ""
    FileWriteUTF16LE $0 "TORRPLAY_ALLOWED_HOSTS="
    FileWriteUTF16LE $0 "$TorrPlayAllowedHosts"
    FileWriteUTF16LE $0 "$\r$\n"
  ${EndIf}
  FileClose $0
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONEXCLAMATION "Could not write the TorrPlay settings."
    Abort
  ${EndIf}
  File "/oname=$PLUGINSDIR\torrplay-settings-check.exe" "$%TAURI_TORRPLAY_SETTINGS_CHECK_EXE%"
  !insertmacro TorrPlayRunSettingsCheck '--desktop "$PLUGINSDIR\torrplay.env"'
  ${If} $0 != 0
    ${If} $0 == "timeout"
      StrCpy $1 "The download directory check timed out. Check that the folder is reachable and try again."
    ${EndIf}
    MessageBox MB_OK|MB_ICONEXCLAMATION "Check the TorrPlay settings.$\r$\n$1"
    Abort
  ${EndIf}
  ${If} $TorrPlayDownloadDirectory == ""
  ${AndIf} $TorrPlayAllowedHosts == ""
    StrCpy $TorrPlaySettingsAction "check"
  ${Else}
    StrCpy $TorrPlaySettingsAction "save"
  ${EndIf}
FunctionEnd
!macroend

; Runs before copying the application files or registering the installation.
; Tauri has already set the 64-bit registry view, which the MSI writes to.
!macro NSIS_HOOK_PREINSTALL
  Push $0
  Push $1
  EnumRegValue $0 HKLM "${TORRPLAY_SERVICE_UPGRADE_KEY}" 0
  ${If} $0 != ""
    MessageBox MB_ICONSTOP|MB_OK "The service edition of TorrPlay, TorrPlay Client, is installed on this computer, and a computer runs one edition. To install the application instead, an administrator must first uninstall TorrPlay Client in Settings, Apps." /SD IDOK
    Abort "TorrPlay Client, the service edition, is installed on this computer."
  ${EndIf}

  ; Keep a failed write retryable as a fresh install. The helper leaves an
  ; existing settings file alone, including one created by another setup.
  ${If} $TorrPlaySettingsAction != ""
    ${If} $TorrPlaySettingsAction == "save"
      ; A timed-out helper may have saved before reporting its result.
      StrCpy $TorrPlaySettingsCreated "saved"
      !insertmacro TorrPlayRunSettingsCheck '"$PLUGINSDIR\torrplay.env" "${TORRPLAY_DESKTOP_SETTINGS}"'
    ${Else}
      !insertmacro TorrPlayRunSettingsCheck '--desktop "$PLUGINSDIR\torrplay.env"'
    ${EndIf}
    ${If} $0 != 0
      ${If} $0 == "timeout"
        StrCpy $1 "Checking or saving the settings timed out. Check that the download folder and your profile are reachable."
      ${Else}
        StrCpy $TorrPlaySettingsCreated ""
      ${EndIf}
      ${If} $TorrPlaySettingsAction == "save"
        MessageBox MB_OK|MB_ICONEXCLAMATION "Could not save the TorrPlay settings.$\r$\n$1"
        Abort "Could not save the TorrPlay settings."
      ${EndIf}
      MessageBox MB_OK|MB_ICONEXCLAMATION "Check the TorrPlay settings.$\r$\n$1"
      Abort "Could not check the TorrPlay settings."
    ${EndIf}
    StrCpy $TorrPlaySettingsCreated $1
  ${EndIf}
  Pop $1
  Pop $0
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Files and registration are in place, so retain the settings when setup closes.
  StrCpy $TorrPlaySettingsCreated ""
!macroend
