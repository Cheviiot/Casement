; Casement installer for Windows. Built by scripts/package-windows with makensis
; (mingw32-nsis) from the collected program in SOURCE:
;   makensis -DVERSION=0.1.0 -DSOURCE=... -DOUTFILE=... -DLICENSE=... -DICON=... casement.nsi
Unicode true
!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"

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

; Running copies hold their files: stop the service and the programs before replacing or
; removing them.
!macro StopPrograms
  nsExec::Exec 'sc stop Casement'
  Sleep 1500
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

  ; The system service accepts connections from Windows start, before anyone logs in, and
  ; shows the login screen and UAC prompts. It replaces the per-user autostart of 0.1.0.
  DeleteRegValue HKCU "${RUN_KEY}" "Casement host"
  ; The machine's key, password and trusted devices: SYSTEM and administrators only
  ; (well-known SIDs, so it works in every language).
  ReadEnvStr $1 PROGRAMDATA
  CreateDirectory "$1\Casement"
  nsExec::Exec 'icacls "$1\Casement" /inheritance:r /grant:r *S-1-5-18:(OI)(CI)F *S-1-5-32-544:(OI)(CI)F'
  ; The service's summary for the application (key fingerprint, password set, number of
  ; trusted devices): readable by every user.
  CreateDirectory "$1\Casement\public"
  nsExec::Exec 'icacls "$1\Casement\public" /grant *S-1-5-32-545:(OI)(CI)RX'
  ; Scan the GStreamer plugins once for every program and user (a minute on first use).
  nsExec::Exec '"$INSTDIR\bin\casement-host.exe" --prepare'
  nsExec::Exec 'sc query Casement'
  Pop $0
  ${If} $0 == 0
    nsExec::Exec 'sc config Casement binPath= "\"$INSTDIR\bin\casement-host.exe\" --service" start= auto'
  ${Else}
    nsExec::Exec 'sc create Casement binPath= "\"$INSTDIR\bin\casement-host.exe\" --service" start= auto DisplayName= "Casement"'
  ${EndIf}
  nsExec::Exec 'sc description Casement "Accepts Casement remote desktop connections, also at the login screen."'
  nsExec::Exec 'sc failure Casement reset= 86400 actions= restart/5000/restart/5000/restart/30000'
  nsExec::Exec 'sc start Casement'

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
  nsExec::Exec 'sc delete Casement'
  ; "Принимать подключения" in the application adds this for its user.
  DeleteRegValue HKCU "${RUN_KEY}" "Casement host"
  nsExec::Exec 'netsh advfirewall firewall delete rule name="Casement"'
  SetShellVarContext all
  Delete "$SMPROGRAMS\Casement.lnk"
  Delete "$DESKTOP\Casement.lnk"
  RMDir /r "$INSTDIR"
  DeleteRegKey HKLM "${UNINSTALL_KEY}"
  DeleteRegKey HKLM "Software\Casement"
  ; The machine's settings in %ProgramData%\Casement and each user's in
  ; %APPDATA%\casement (keys, password, trusted devices) stay for a later install.
SectionEnd
