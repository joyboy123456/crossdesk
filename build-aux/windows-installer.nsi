; CrossDesk Windows installer.
;
; Per-user install: it needs no administrator rights, and the "start with
; Windows" entry it writes lives in HKCU anyway - an elevated install could
; land that entry in a different account's hive than the one that will
; actually use it.
;
; Build with:
;   makensis /DVERSION=0.11.0 /DSOURCE_DIR=... /DOUTFILE=... windows-installer.nsi

Unicode true

!include "MUI2.nsh"

!define APPNAME "CrossDesk"
!define PUBLISHER "CrossDesk"
!define RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}"

!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!ifndef SOURCE_DIR
  !define SOURCE_DIR "."
!endif
!ifndef OUTFILE
  !define OUTFILE "CrossDesk-Setup.exe"
!endif

Name "${APPNAME} ${VERSION}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\${APPNAME}"
InstallDirRegKey HKCU "Software\${APPNAME}" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APPNAME}"
VIAddVersionKey "FileDescription" "${APPNAME} installer"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" ""

!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\CrossDesk.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Start ${APPNAME} now"

!insertmacro MUI_PAGE_LICENSE "${SOURCE_DIR}\LICENSE"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "SimpChinese"

Section "${APPNAME} (required)" SecCore
  SectionIn RO
  SetOutPath "$INSTDIR"
  ; cargo names the binary lowercase; ship it under the display name. Renaming
  ; after the fact would be a no-op on a case-insensitive filesystem.
  File /oname=CrossDesk.exe "${SOURCE_DIR}\target\release\crossdesk.exe"
  File "${SOURCE_DIR}\target\release\lan-mouse.exe"
  File "${SOURCE_DIR}\LICENSE"

  CreateDirectory "$SMPROGRAMS\${APPNAME}"
  CreateShortcut "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk" "$INSTDIR\CrossDesk.exe"

  WriteRegStr HKCU "Software\${APPNAME}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${APPNAME}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
  WriteUninstaller "$INSTDIR\uninstall.exe"
SectionEnd

Section "Start with Windows" SecAutostart
  WriteRegStr HKCU "${RUN_KEY}" "${APPNAME}" '"$INSTDIR\CrossDesk.exe"'
SectionEnd

Section "Desktop shortcut" SecDesktop
  CreateShortcut "$DESKTOP\${APPNAME}.lnk" "$INSTDIR\CrossDesk.exe"
SectionEnd

LangString DESC_SecCore ${LANG_ENGLISH} "The ${APPNAME} application and command line tool."
LangString DESC_SecCore ${LANG_SIMPCHINESE} "${APPNAME} 应用程序与命令行工具。"
LangString DESC_SecAutostart ${LANG_ENGLISH} "Launch ${APPNAME} when you sign in, so the other machine can reach this one without starting it by hand."
LangString DESC_SecAutostart ${LANG_SIMPCHINESE} "登录时自动启动 ${APPNAME},这样另一台机器无需手动启动即可连接本机。"
LangString DESC_SecDesktop ${LANG_ENGLISH} "Create a shortcut on the desktop."
LangString DESC_SecDesktop ${LANG_SIMPCHINESE} "在桌面创建快捷方式。"

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecCore} $(DESC_SecCore)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecAutostart} $(DESC_SecAutostart)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop} $(DESC_SecDesktop)
!insertmacro MUI_FUNCTION_DESCRIPTION_END

Section "Uninstall"
  Delete "$INSTDIR\CrossDesk.exe"
  Delete "$INSTDIR\lan-mouse.exe"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk"
  RMDir "$SMPROGRAMS\${APPNAME}"
  Delete "$DESKTOP\${APPNAME}.lnk"

  DeleteRegValue HKCU "${RUN_KEY}" "${APPNAME}"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  DeleteRegKey HKCU "Software\${APPNAME}"
SectionEnd
