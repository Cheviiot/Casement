; Casement installer for Windows. Built by scripts/package-windows with makensis
; (mingw32-nsis) from the collected program in SOURCE:
;   makensis -DVERSION=0.1.0 -DSOURCE=... -DOUTFILE=... -DLICENSE=... -DICON=... casement.nsi
Unicode true
!include "MUI2.nsh"
!include "FileFunc.nsh"

!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Casement"
!define RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"

Name "Casement"
OutFile "${OUTFILE}"
InstallDir "$PROGRAMFILES64\Casement"
InstallDirRegKey HKLM "Software\Casement" "InstallDir"
RequestExecutionLevel admin
SetCompressor /SOLID lzma
BrandingText "Casement ${VERSION}"

VIProductVersion "${VERSION}.0"
VIAddVersionKey /LANG=0 "ProductName" "Casement"
VIAddVersionKey /LANG=0 "FileDescription" "Casement Setup"
VIAddVersionKey /LANG=0 "FileVersion" "${VERSION}"
VIAddVersionKey /LANG=0 "ProductVersion" "${VERSION}"
VIAddVersionKey /LANG=0 "CompanyName" "Cheviiot"
VIAddVersionKey /LANG=0 "LegalCopyright" "MIT License"

!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_ABORTWARNING
!define MUI_COMPONENTSPAGE_NODESC
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_TEXT "$(RunText)"
!define MUI_FINISHPAGE_RUN_FUNCTION LaunchAsUser

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${LICENSE}"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "Russian"
!insertmacro MUI_LANGUAGE "English"

LangString RunText ${LANG_RUSSIAN} "Запустить Casement"
LangString RunText ${LANG_ENGLISH} "Launch Casement"
LangString MainSection ${LANG_RUSSIAN} "Casement"
LangString MainSection ${LANG_ENGLISH} "Casement"
LangString DesktopSection ${LANG_RUSSIAN} "Ярлык на рабочем столе"
LangString DesktopSection ${LANG_ENGLISH} "Desktop shortcut"

; Running copies hold their files: stop them before replacing or removing.
!macro StopPrograms
  nsExec::Exec 'taskkill /F /IM casement.exe'
  nsExec::Exec 'taskkill /F /IM casement-host.exe'
  nsExec::Exec 'taskkill /F /IM casement-cli.exe'
  Sleep 500
!macroend

; The installer runs elevated; started through Explorer the application runs as the
; user instead, and starts casement-host the same way.
Function LaunchAsUser
  Exec '"$WINDIR\explorer.exe" "$INSTDIR\bin\casement.exe"'
FunctionEnd

Function .onInit
  SetRegView 64
FunctionEnd

Function un.onInit
  SetRegView 64
FunctionEnd

Section "!$(MainSection)" SecMain
  SectionIn RO
  !insertmacro StopPrograms
  ; An update must not keep DLLs and plugins of the previous version.
  RMDir /r "$INSTDIR\bin"
  RMDir /r "$INSTDIR\lib"
  RMDir /r "$INSTDIR\libexec"
  RMDir /r "$INSTDIR\share"
  SetOutPath "$INSTDIR"
  File /r "${SOURCE}\*"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  SetShellVarContext all
  CreateShortcut "$SMPROGRAMS\Casement.lnk" "$INSTDIR\bin\casement.exe" "" "$INSTDIR\casement.ico"

  ; Accepting connections is on by default: casement-host starts at this user's login.
  ; "Принимать подключения" in the application switches it.
  WriteRegStr HKCU "${RUN_KEY}" "Casement host" '"$INSTDIR\bin\casement-host.exe"'

  WriteRegStr HKLM "Software\Casement" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayName" "Casement"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "Publisher" "Cheviiot"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\casement.ico"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "URLInfoAbout" "https://github.com/Cheviiot/Casement"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKLM "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "EstimatedSize" $0

  ; Incoming connections on private networks for the programs themselves, so Windows
  ; does not ask (a dismissed question would leave blocking rules).
  nsExec::Exec 'netsh advfirewall firewall delete rule name="Casement"'
  nsExec::Exec 'netsh advfirewall firewall add rule name="Casement" dir=in action=allow profile=private enable=yes program="$INSTDIR\bin\casement.exe"'
  nsExec::Exec 'netsh advfirewall firewall add rule name="Casement" dir=in action=allow profile=private enable=yes program="$INSTDIR\bin\casement-host.exe"'
  nsExec::Exec 'netsh advfirewall firewall add rule name="Casement" dir=in action=allow profile=private enable=yes program="$INSTDIR\bin\casement-cli.exe"'
SectionEnd

Section /o "$(DesktopSection)" SecDesktop
  SetShellVarContext all
  CreateShortcut "$DESKTOP\Casement.lnk" "$INSTDIR\bin\casement.exe" "" "$INSTDIR\casement.ico"
SectionEnd

Section "Uninstall"
  !insertmacro StopPrograms
  ; "Принимать подключения" in the application adds this for its user.
  DeleteRegValue HKCU "${RUN_KEY}" "Casement host"
  nsExec::Exec 'netsh advfirewall firewall delete rule name="Casement"'
  SetShellVarContext all
  Delete "$SMPROGRAMS\Casement.lnk"
  Delete "$DESKTOP\Casement.lnk"
  RMDir /r "$INSTDIR"
  DeleteRegKey HKLM "${UNINSTALL_KEY}"
  DeleteRegKey HKLM "Software\Casement"
  ; Settings in %APPDATA%\casement (device key, password, trusted devices) stay.
SectionEnd
