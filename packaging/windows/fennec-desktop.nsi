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
!define MUI_FINISHPAGE_LINK_COLOR "E9865D"
!define MUI_CUSTOMFUNCTION_GUIINIT FennecGuiInit
!define MUI_PAGE_CUSTOMFUNCTION_PRE InstFilesPre
!define MUI_WIZOWIDTH 900
!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"

Name "Fennec Desktop ${VERSION}"
OutFile "${DOUT}"
InstallDir "$LOCALAPPDATA\Programs\Fennec Desktop"
InstallDirRegKey HKCU "Software\Fennec Desktop" "InstallDir"
DirText "Choose the folder in which to install Fennec Desktop."
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
!include "nsDialogs.nsh"
!include "LogicLib.nsh"
!include "WinMessages.nsh"

Var Dialog
Var PathInput
Var BrowseButton
Var ShortcutCheck
Var CreateShortcut

Page custom WelcomePage
Page custom DirectoryPage
Page custom ComponentsPage
!insertmacro MUI_PAGE_INSTFILES
Page custom FinishPage
!insertmacro MUI_LANGUAGE "English"

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_UNLANGUAGE "English"

Function .onInit
    SetShellVarContext current
    StrCpy $CreateShortcut "1"
    InitPluginsDir
    SetOutPath $PLUGINSDIR
    File "${PAGE_BITMAPS}\welcome.bmp"
    File "${PAGE_BITMAPS}\directory.bmp"
    File "${PAGE_BITMAPS}\components.bmp"
    File "${PAGE_BITMAPS}\finish.bmp"
FunctionEnd

Function FennecGuiInit
    System::Call 'dwmapi::DwmSetWindowAttribute(p $HWNDPARENT, i 20, *i 1, i 4)'
    SetCtlColors $HWNDPARENT "EDEDED" "141414"
    GetDlgItem $0 $HWNDPARENT 1
    SetCtlColors $0 "141414" "EDEDED"
    GetDlgItem $0 $HWNDPARENT 2
    SetCtlColors $0 "EDEDED" "1F1F1F"
    GetDlgItem $0 $HWNDPARENT 3
    SetCtlColors $0 "EDEDED" "1F1F1F"
FunctionEnd

Function ColorizeChildren
    Pop $1
    StrCpy $2 0
children_loop:
    FindWindowEx $3 $1 "" "" $2
    StrCmp $3 0 children_done
    SetCtlColors $3 "EDEDED" "141414"
    StrCpy $2 $3
    Goto children_loop
children_done:
FunctionEnd

Function InstFilesPre
    SetCtlColors $HWNDPARENT "EDEDED" "141414"
    Push $HWNDPARENT
    Call ColorizeChildren
    GetDlgItem $0 $HWNDPARENT 1004
    SetCtlColors $0 "EDEDED" "141414"
    GetDlgItem $0 $HWNDPARENT 1005
    SetCtlColors $0 "E9865D" "141414"
FunctionEnd

Function WelcomePage
    nsDialogs::Create 1018
    Pop $Dialog
    SetCtlColors $Dialog "EDEDED" "141414"
    nsDialogs::CreateBitmap 0 0 900px 500px "$PLUGINSDIR\welcome.bmp"
    Pop $0
    GetDlgItem $1 $HWNDPARENT 1
    SendMessage $1 ${WM_SETTEXT} 0 "STR:Get started"
    SetCtlColors $1 "141414" "EDEDED"
FunctionEnd

Function DirectoryPage
    nsDialogs::Create 1018
    Pop $Dialog
    SetCtlColors $Dialog "EDEDED" "141414"
    nsDialogs::CreateBitmap 0 0 900px 500px "$PLUGINSDIR\directory.bmp"
    Pop $0
    nsDialogs::CreateText 250px 214px 460px 38px "$INSTDIR"
    Pop $PathInput
    SetCtlColors $PathInput "EDEDED" "1F1F1F"
    ${NSD_OnChange} $PathInput SavePath
    nsDialogs::CreateButton 730px 214px 100px 38px "Browse..."
    Pop $BrowseButton
    SetCtlColors $BrowseButton "141414" "EDEDED"
    ${NSD_OnClick} $BrowseButton BrowseFolder
    GetDlgItem $1 $HWNDPARENT 1
    SendMessage $1 ${WM_SETTEXT} 0 "STR:Next >"
FunctionEnd

Function SavePath
    ${NSD_GetText} $PathInput $INSTDIR
FunctionEnd

Function BrowseFolder
    ${NSD_GetText} $PathInput $0
    nsDialogs::SelectFolderDialog "Choose the Fennec Desktop install folder" $0
    Pop $0
    ${If} $0 != error
        ${NSD_SetText} $PathInput $0
        StrCpy $INSTDIR $0
    ${EndIf}
FunctionEnd

Function ComponentsPage
    ${NSD_GetText} $PathInput $INSTDIR
    nsDialogs::Create 1018
    Pop $Dialog
    SetCtlColors $Dialog "EDEDED" "141414"
    nsDialogs::CreateBitmap 0 0 900px 500px "$PLUGINSDIR\components.bmp"
    Pop $0
    nsDialogs::CreateCheckbox 250px 236px 560px 26px "Fennec Desktop (required)"
    Pop $0
    ${NSD_Check} $0
    EnableWindow $0 0
    SetCtlColors $0 "EDEDED" "25282D"
    nsDialogs::CreateCheckbox 250px 274px 560px 26px "Desktop shortcut"
    Pop $ShortcutCheck
    ${NSD_Check} $ShortcutCheck
    SetCtlColors $ShortcutCheck "EDEDED" "25282D"
    ${NSD_OnClick} $ShortcutCheck SaveShortcut
    GetDlgItem $1 $HWNDPARENT 1
    SendMessage $1 ${WM_SETTEXT} 0 "STR:Install"
    SetCtlColors $1 "141414" "EDEDED"
FunctionEnd

Function SaveShortcut
    ${NSD_GetState} $ShortcutCheck $CreateShortcut
FunctionEnd

Function FinishPage
    nsDialogs::Create 1018
    Pop $Dialog
    SetCtlColors $Dialog "EDEDED" "141414"
    nsDialogs::CreateBitmap 0 0 900px 500px "$PLUGINSDIR\finish.bmp"
    Pop $0
    GetDlgItem $1 $HWNDPARENT 1
    SendMessage $1 ${WM_SETTEXT} 0 "STR:Close"
    SetCtlColors $1 "141414" "EDEDED"
    GetDlgItem $1 $HWNDPARENT 3
    EnableWindow $1 0
FunctionEnd

Section "Fennec Desktop (required)" SEC_APP
    SectionIn RO
    SetOutPath "$INSTDIR"
    File /oname=Fennec.exe "${BINARY}"
    CreateDirectory "$SMPROGRAMS\Fennec Desktop"
    CreateShortCut "$SMPROGRAMS\Fennec Desktop\Fennec.lnk" "$INSTDIR\Fennec.exe" "" "$INSTDIR" 0
    ${If} $CreateShortcut == 1
        CreateShortCut "$DESKTOP\Fennec.lnk" "$INSTDIR\Fennec.exe" "" "$INSTDIR" 0
    ${EndIf}
    WriteUninstaller "$INSTDIR\Uninstall Fennec.exe"
    WriteRegStr HKCU "Software\Fennec Desktop" "InstallDir" "$INSTDIR"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FennecDesktop" "DisplayName" "Fennec Desktop"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FennecDesktop" "DisplayVersion" "${VERSION}"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FennecDesktop" "DisplayIcon" "$INSTDIR\Fennec.exe,0"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FennecDesktop" "UninstallString" '"$INSTDIR\Uninstall Fennec.exe"'
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FennecDesktop" "NoModify" 1
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FennecDesktop" "NoRepair" 1
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
