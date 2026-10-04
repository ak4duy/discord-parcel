; Linux wrapper supplies absolute paths for all four path definitions.
; INSTALL_FILES must delete every staged file, then RMDir staged directories
; deepest-first, using quoted $INSTDIR-relative paths and no recursive removal.
Unicode true
RequestExecutionLevel user
SetCompressor /SOLID lzma
SetCompressorDictSize 64
SetDatablockOptimize on

!ifndef APP_VERSION
  !error "APP_VERSION is required"
!endif
!ifndef BUNDLE_DIR
  !error "BUNDLE_DIR is required"
!endif
!ifndef OUTPUT_FILE
  !error "OUTPUT_FILE is required"
!endif
!ifndef APP_ICON
  !error "APP_ICON is required"
!endif
!ifndef INSTALL_FILES
  !error "INSTALL_FILES is required (generated uninstall manifest)"
!endif

!include "MUI2.nsh"
!include "x64.nsh"
!include "LogicLib.nsh"

!define APP_NAME "Discord Parcel"
!define APP_PUBLISHER "Discord Parcel"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\DiscordParcel"

Name "${APP_NAME}"
Caption "${APP_NAME} ${APP_VERSION} Setup"
OutFile "${OUTPUT_FILE}"
InstallDir "$LOCALAPPDATA\Programs\DiscordParcel"
ShowInstDetails show
ShowUninstDetails show

!define MUI_ICON "${APP_ICON}"
!define MUI_UNICON "${APP_ICON}"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\discord-parcel.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Launch ${APP_NAME}"
!define MUI_FINISHPAGE_NOAUTOCLOSE

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_UNPAGE_FINISH
!insertmacro MUI_LANGUAGE "English"

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_OK|MB_ICONSTOP "${APP_NAME} requires 64-bit Windows." /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
  SetShellVarContext current
  SetRegView 64
  ; Read after selecting the registry view so upgrades reuse the same directory.
  ReadRegStr $0 HKCU "${UNINSTALL_KEY}" "InstallLocation"
  ${If} $0 != ""
    StrCpy $INSTDIR "$0"
  ${EndIf}
FunctionEnd

Function un.onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_OK|MB_ICONSTOP "${APP_NAME} requires 64-bit Windows." /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
  SetShellVarContext current
  SetRegView 64
FunctionEnd

Section "${APP_NAME}" SEC_APP
  SectionIn RO
  ; Run the previous ownership-based uninstaller so removed DLLs cannot linger
  ; across upgrades. It preserves transfer data and unknown user files.
  IfFileExists "$INSTDIR\uninstall.exe" 0 previous_removed
    SetOutPath "$TEMP"
    ClearErrors
    ExecWait '"$INSTDIR\uninstall.exe" /S _?=$INSTDIR' $0
    IfErrors previous_failed
    IntCmp $0 0 previous_removed previous_failed previous_failed
  previous_failed:
    MessageBox MB_OK|MB_ICONSTOP "Could not remove the previous version. Close Discord Parcel, uninstall it, then run Setup again." /SD IDOK
    Abort
  previous_removed:
  SetOutPath "$INSTDIR"
  SetOverwrite on
  ; Include the entire staging tree, including extensionless files and subdirs.
  File /r "${BUNDLE_DIR}/*"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateShortcut "$SMPROGRAMS\Discord Parcel.lnk" "$INSTDIR\discord-parcel.exe" "" "$INSTDIR\discord-parcel.exe" 0

  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${APP_NAME}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${APP_VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "${APP_PUBLISHER}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" '$\"$INSTDIR\discord-parcel.exe$\",0'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '$\"$INSTDIR\uninstall.exe$\"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '$\"$INSTDIR\uninstall.exe$\" /S'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  ; Do not hold the installation directory open while removing it.
  SetOutPath "$TEMP"
  !include "${INSTALL_FILES}"

  Delete "$SMPROGRAMS\Discord Parcel.lnk"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  Delete "$INSTDIR\uninstall.exe"
  ; Preserve personal files and directories not listed in the manifest.
  RMDir "$INSTDIR"
SectionEnd
