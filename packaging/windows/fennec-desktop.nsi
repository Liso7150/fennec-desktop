Unicode true
RequestExecutionLevel user
SilentInstall normal
SilentUnInstall normal
CRCCheck on
SetCompressor /FINAL /SOLID lzma

!define MUI_BGCOLOR "141414"
!define MUI_TEXTCOLOR "EDEDED"
!define MUI_HEADERIMAGE
!define MUI_HEADERIMAGE_BITMAP "${HEADER_BITMAP}"
!define MUI_WELCOMEFINISHPAGE_BITMAP "${WELCOME_BITMAP}"
!define MUI_UNWELCOMEFINISHPAGE_BITMAP "${WELCOME_BITMAP}"
!define MUI_FINISHPAGE_LINK_COLOR "E9865D"
!define MUI_CUSTOMFUNCTION_GUIINIT FennecGuiInit
!define MUI_ABORTWARNING
!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_WELCOMEPAGE_TITLE "Welcome to Fennec"
!define MUI_WELCOMEPAGE_TEXT "Set up the local engine, coding service, and model folder in one pass. Fennec stays on this machine; nothing is uploaded.$\r$\n$\r$\nThis installer places Fennec Desktop on your PC. On first launch, the setup wizard checks Python, Git, Rust, the built Fennec engine, core dependencies, and your model folder."
!define MUI_DIRECTORYPAGE_TEXT_TOP "Choose where Fennec should live.$\r$\nUse Browse to pick another folder."
!define MUI_COMPONENTSPAGE_TEXT_TOP "Fennec Desktop is required.$\r$\nAdd a desktop shortcut if you want one."
!define MUI_FINISHPAGE_TITLE "Fennec is ready"
!define MUI_FINISHPAGE_TEXT "Fennec Desktop is installed.$\r$\n$\r$\nLaunch it to run the local setup wizard."
!define MUI_FINISHPAGE_RUN "$INSTDIR\Fennec.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Launch Fennec setup"

Name "Fennec Desktop ${VERSION}"
OutFile "${DOUT}"
InstallDir "$LOCALAPPDATA\Programs\Fennec Desktop"
InstallDirRegKey HKCU "Software\Fennec Desktop" "InstallDir"
DirText "Choose the folder in which to install Fennec Desktop."
ComponentText "Choose which shortcuts to create."
BrandingText "Fennec Suite"
WindowIcon on
ShowInstDetails show

VIProductVersion "${VI_VERSION}"
VIAddVersionKey /LANG=1033 "ProductName" "Fennec Desktop"
VIAddVersionKey /LANG=1033 "FileDescription" "Fennec Desktop ${VERSION}"
VIAddVersionKey /LANG=1033 "FileVersion" "${VERSION}"
VIAddVersionKey /LANG=1033 "ProductVersion" "${VERSION}"
VIAddVersionKey /LANG=1033 "LegalCopyright" "MIT License"
VIAddVersionKey /LANG=1033 "Comments" "Local-first coding agent with native setup wizard"

!include "MUI2.nsh"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_LANGUAGE "English"

Function .onInit
    SetShellVarContext current
FunctionEnd

Function FennecGuiInit
    System::Call 'dwmapi::DwmSetWindowAttribute(p $HWNDPARENT, i 20, *i 1, i 4)'
    GetDlgItem $0 $HWNDPARENT 1
    SetCtlColors $0 "141414" "E9865D"
    GetDlgItem $0 $HWNDPARENT 3
    SetCtlColors $0 "EDEDED" "1F1F1F"
    GetDlgItem $0 $HWNDPARENT 2
    SetCtlColors $0 "EDEDED" "1F1F1F"
FunctionEnd

Section "Fennec Desktop (required)" SEC_APP
    SectionIn RO
    SetOutPath "$INSTDIR"
    File /oname=Fennec.exe "${BINARY}"
    CreateDirectory "$SMPROGRAMS\Fennec Desktop"
    CreateShortCut "$SMPROGRAMS\Fennec Desktop\Fennec.lnk" "$INSTDIR\Fennec.exe" "" "$INSTDIR" 0
    WriteUninstaller "$INSTDIR\Uninstall Fennec.exe"
    WriteRegStr HKCU "Software\Fennec Desktop" "InstallDir" "$INSTDIR"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FennecDesktop" "DisplayName" "Fennec Desktop"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FennecDesktop" "DisplayVersion" "${VERSION}"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FennecDesktop" "DisplayIcon" "$INSTDIR\Fennec.exe,0"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FennecDesktop" "UninstallString" '"$INSTDIR\Uninstall Fennec.exe"'
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FennecDesktop" "NoModify" 1
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FennecDesktop" "NoRepair" 1
SectionEnd

Section "Desktop shortcut" SEC_DESKTOP
    CreateShortCut "$DESKTOP\Fennec.lnk" "$INSTDIR\Fennec.exe" "" "$INSTDIR" 0
SectionEnd

Section "Uninstall"
    Delete "$INSTDIR\Fennec.exe"
    Delete "$INSTDIR\Uninstall Fennec.exe"
    Delete "$SMPROGRAMS\Fennec Desktop\Fennec.lnk"
    Delete "$DESKTOP\Fennec.lnk"
    RMDir "$SMPROGRAMS\Fennec Desktop"
    RMDir "$INSTDIR"
    DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FennecDesktop"
    DeleteRegKey HKCU "Software\Fennec Desktop"
SectionEnd
