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
!define TORRPLAY_HELP_SHORTCUT_EN "TorrPlay Help"
!define TORRPLAY_HELP_SHORTCUT_RU "Справка TorrPlay"
!define TORRPLAY_HELP_SHORTCUT_KEY "Software\${BUNDLEID}\OfflineHelp"
!define TORRPLAY_HELP_UNREADABLE "unreadable"

Var TorrPlayDownloadDirectory
Var TorrPlayAllowedHosts
Var TorrPlayDownloadEdit
Var TorrPlayHostsEdit
Var TorrPlaySettingsAction
Var TorrPlaySettingsCreated
Var TorrPlayExistingInstall
Var TorrPlayEnglishHelpShortcutId
Var TorrPlayRussianHelpShortcutId
Var TorrPlayEnglishHelpShortcutTarget
Var TorrPlayRussianHelpShortcutTarget
Var TorrPlayEnglishHelpShortcutState
Var TorrPlayRussianHelpShortcutState

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
    MessageBox MB_ICONSTOP|MB_OK "TorrPlay Service is installed on this computer. To install the desktop application, an administrator must first uninstall TorrPlay Service in Settings, Apps." /SD IDOK
    Abort "TorrPlay Service is installed on this computer."
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

; Read the file identity without following or modifying the shortcut target.
; Keep the volume serial number and the full FILE_ID_128 together.
!macro TorrPlayReadHelpShortcutId path output
  Push $0
  Push $1
  Push $2
  Push $3
  Push $4
  StrCpy ${output} "${TORRPLAY_HELP_UNREADABLE}"
  ; FILE_READ_ATTRIBUTES, share read/write/delete, OPEN_EXISTING,
  ; FILE_FLAG_OPEN_REPARSE_POINT. Identify the link entry itself.
  System::Call 'kernel32::CreateFileW(w "${path}", i 0x80, i 7, p 0, i 3, i 0x00200000, p 0) p .r0 ?e'
  Pop $1
  ${If} $0 P<> -1
    System::Alloc 52
    Pop $1
    ${If} $1 P<> 0
      ; BY_HANDLE_FILE_INFORMATION has nNumberOfLinks at byte offset 40.
      System::Call 'kernel32::GetFileInformationByHandle(p r0, p r1) i .r2'
      ${If} $2 <> 0
        System::Call '*$1(i,i,i,i,i,i,i,i,i,i,i .r2)'
        ${If} $2 = 1
          ; FileIdInfo = 18. FILE_ID_INFO is three 64-bit fields.
          System::Call 'kernel32::GetFileInformationByHandleEx(p r0, i 18, p r1, i 24) i .r2'
          ${If} $2 <> 0
            System::Call '*$1(l .r2, l .r3, l .r4)'
            StrCpy ${output} "$2:$3:$4"
          ${EndIf}
        ${Else}
          StrCpy ${output} ""
        ${EndIf}
      ${EndIf}
      System::Free $1
    ${EndIf}
    System::Call 'kernel32::CloseHandle(p r0)'
  ${ElseIf} $1 = 2
  ${OrIf} $1 = 3
    ; ERROR_FILE_NOT_FOUND or ERROR_PATH_NOT_FOUND.
    StrCpy ${output} ""
  ${EndIf}
  Pop $4
  Pop $3
  Pop $2
  Pop $1
  Pop $0
!macroend

!macro TorrPlayMatchHelpShortcutTarget path target output
  Push $0
  Push $1
  Push $2
  Push $3
  StrCpy ${output} "${TORRPLAY_HELP_UNREADABLE}"
  !insertmacro ComHlpr_CreateInProcInstance ${CLSID_ShellLink} ${IID_IShellLink} r0 ""
  ${If} $0 P<> 0
    ${IUnknown::QueryInterface} $0 '("${IID_IPersistFile}", .r1)'
    ${If} $1 P<> 0
      ${IPersistFile::Load} $1 '("${path}", ${STGM_READ}).r3'
      ${If} $3 = 0
        ${IShellLink::GetPath} $0 '(.r2, ${NSIS_MAX_STRLEN}, 0, ${SLGP_RAWPATH}).r3'
        ${If} $3 >= 0
          StrCpy ${output} 0
          ${If} $3 = 0
          ${AndIf} $2 == "${target}"
            StrCpy ${output} 1
          ${EndIf}
        ${EndIf}
      ${EndIf}
      ${IUnknown::Release} $1 ""
    ${EndIf}
    ${IUnknown::Release} $0 ""
  ${EndIf}
  Pop $3
  Pop $2
  Pop $1
  Pop $0
!macroend

!macro TorrPlayDeleteHelpShortcut name id target retire
  Push $R0
  Push $R1
  ${If} ${id} != ""
  ${AndIf} ${target} != ""
    StrCpy $R0 "$SMPROGRAMS\${name}.lnk"
    !insertmacro TorrPlayReadHelpShortcutId "$R0" $R1
    ${If} $R1 == ${id}
      !insertmacro TorrPlayMatchHelpShortcutTarget "$R0" "${target}" $R1
      ${If} $R1 == "1"
        ClearErrors
        Delete "$R0"
        ${If} ${Errors}
          StrCpy $R1 "${TORRPLAY_HELP_UNREADABLE}"
        ${EndIf}
      ${EndIf}
    ${EndIf}
    ${If} $R1 != "${TORRPLAY_HELP_UNREADABLE}"
      DeleteRegValue SHCTX "${TORRPLAY_HELP_SHORTCUT_KEY}" "${name}"
    ${ElseIf} ${retire} = 1
      WriteRegStr SHCTX "${TORRPLAY_HELP_SHORTCUT_KEY}" "${name}" "${id}|${target}|retired"
    ${EndIf}
  ${EndIf}
  Pop $R1
  Pop $R0
!macroend

!macro TorrPlayReadHelpShortcut name id target state
  Push $0
  StrCpy ${id} ""
  StrCpy ${target} ""
  StrCpy ${state} ""
  ReadRegStr $0 SHCTX "${TORRPLAY_HELP_SHORTCUT_KEY}" "${name}"
  ${If} $0 != ""
    ${WordFind} "$0" "|" "+1" ${id}
    ${WordFind} "$0" "|" "+2" ${target}
    ${WordFind} "$0" "|" "+3" ${state}
  ${EndIf}
  Pop $0
!macroend

!macro TorrPlayReadHelpShortcuts
  !insertmacro TorrPlayReadHelpShortcut "${TORRPLAY_HELP_SHORTCUT_EN}" $TorrPlayEnglishHelpShortcutId $TorrPlayEnglishHelpShortcutTarget $TorrPlayEnglishHelpShortcutState
  !insertmacro TorrPlayReadHelpShortcut "${TORRPLAY_HELP_SHORTCUT_RU}" $TorrPlayRussianHelpShortcutId $TorrPlayRussianHelpShortcutTarget $TorrPlayRussianHelpShortcutState
!macroend

!macro TorrPlayCreateHelpShortcut name id target output
  Push $R0
  Push $R1
  Push $R3
  StrCpy ${output} 0
  StrCpy $R3 0
  StrCpy $R0 "$SMPROGRAMS\${name}.lnk"
  ${If} ${FileExists} "$R0"
    !insertmacro TorrPlayReadHelpShortcutId "$R0" $R1
    ${If} $R1 != ""
    ${AndIf} $R1 == ${id}
    ${AndIf} ${target} != ""
      !insertmacro TorrPlayMatchHelpShortcutTarget "$R0" "${target}" $R1
      ${If} $R1 == "1"
        ${If} ${target} == "$INSTDIR\help\index.html"
          WriteRegStr SHCTX "${TORRPLAY_HELP_SHORTCUT_KEY}" "${name}" "${id}|${target}|installed"
          StrCpy ${output} 1
        ${Else}
          StrCpy $R3 1
        ${EndIf}
      ${EndIf}
    ${EndIf}
  ${Else}
    StrCpy $R3 1
  ${EndIf}
  ${If} $R3 = 1
    ClearErrors
    CreateShortCut "$R0" "$INSTDIR\help\index.html" "" "$INSTDIR\${MAINBINARYNAME}.exe"
    ${IfNot} ${Errors}
      !insertmacro TorrPlayReadHelpShortcutId "$R0" $R1
      ${If} $R1 != ""
      ${AndIf} $R1 != "${TORRPLAY_HELP_UNREADABLE}"
        ClearErrors
        ; A pipe cannot occur in either the identity or a Windows file path.
        WriteRegStr SHCTX "${TORRPLAY_HELP_SHORTCUT_KEY}" "${name}" "$R1|$INSTDIR\help\index.html|installed"
        ${If} ${Errors}
          Delete "$R0"
        ${Else}
          StrCpy ${output} 1
        ${EndIf}
      ${Else}
        Delete "$R0"
      ${EndIf}
    ${EndIf}
  ${EndIf}
  Pop $R3
  Pop $R1
  Pop $R0
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Files and registration are in place, so retain the settings when setup closes.
  StrCpy $TorrPlaySettingsCreated ""

  ${If} $NoShortcutMode <> 1
    Push $0
    Push $R2
    !insertmacro TorrPlayReadHelpShortcuts
    ; Use the current user's display language independently of the wizard.
    System::Call 'kernel32::GetUserDefaultUILanguage() i .r0'
    IntOp $0 $0 & 0x03ff
    ; Primary language ID for Russian.
    ${If} $0 = 0x19
      !insertmacro TorrPlayCreateHelpShortcut "${TORRPLAY_HELP_SHORTCUT_RU}" $TorrPlayRussianHelpShortcutId $TorrPlayRussianHelpShortcutTarget $R2
      ${If} $R2 = 1
        !insertmacro TorrPlayDeleteHelpShortcut "${TORRPLAY_HELP_SHORTCUT_EN}" $TorrPlayEnglishHelpShortcutId $TorrPlayEnglishHelpShortcutTarget 0
      ${Else}
        !insertmacro TorrPlayCreateHelpShortcut "${TORRPLAY_HELP_SHORTCUT_EN}" $TorrPlayEnglishHelpShortcutId $TorrPlayEnglishHelpShortcutTarget $R2
      ${EndIf}
    ${Else}
      !insertmacro TorrPlayCreateHelpShortcut "${TORRPLAY_HELP_SHORTCUT_EN}" $TorrPlayEnglishHelpShortcutId $TorrPlayEnglishHelpShortcutTarget $R2
      ${If} $R2 = 1
        !insertmacro TorrPlayDeleteHelpShortcut "${TORRPLAY_HELP_SHORTCUT_RU}" $TorrPlayRussianHelpShortcutId $TorrPlayRussianHelpShortcutTarget 0
      ${Else}
        !insertmacro TorrPlayCreateHelpShortcut "${TORRPLAY_HELP_SHORTCUT_RU}" $TorrPlayRussianHelpShortcutId $TorrPlayRussianHelpShortcutTarget $R2
      ${EndIf}
    ${EndIf}
    Pop $R2
    Pop $0
  ${EndIf}
!macroend

!macro TorrPlayUninstallHelpShortcut name id target state
  ${If} ${target} == "$INSTDIR\help\index.html"
    !insertmacro TorrPlayDeleteHelpShortcut "${name}" ${id} ${target} 1
  ${ElseIf} ${state} == "retired"
  ${AndIfNot} ${FileExists} "${target}"
    ; A previous uninstall could not remove this link. A reinstall at its
    ; recorded target makes the link live again.
    !insertmacro TorrPlayDeleteHelpShortcut "${name}" ${id} ${target} 1
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Tauri's uninstall registration is shared by installs in different
  ; directories. Match the saved target to this uninstaller's directory.
  !insertmacro TorrPlayReadHelpShortcuts
  !insertmacro TorrPlayUninstallHelpShortcut "${TORRPLAY_HELP_SHORTCUT_EN}" $TorrPlayEnglishHelpShortcutId $TorrPlayEnglishHelpShortcutTarget $TorrPlayEnglishHelpShortcutState
  !insertmacro TorrPlayUninstallHelpShortcut "${TORRPLAY_HELP_SHORTCUT_RU}" $TorrPlayRussianHelpShortcutId $TorrPlayRussianHelpShortcutTarget $TorrPlayRussianHelpShortcutState
  DeleteRegKey /ifempty SHCTX "${TORRPLAY_HELP_SHORTCUT_KEY}"
  DeleteRegKey /ifempty SHCTX "Software\${BUNDLEID}"
!macroend
