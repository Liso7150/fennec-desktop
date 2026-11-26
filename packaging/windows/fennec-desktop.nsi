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
!define MUI_PAGE_CUSTOMFUNCTION_SHOW HiddenInstFilesShow
!define MUI_PAGE_CUSTOMFUNCTION_LEAVE HiddenInstFilesLeave
SetFont "Segoe UI" 9
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
Var ImageCtrl
Var BmpHandle
Var WizardWidth
Var WizardHeight
Var WizardScale
Var ScaleParent
Var ScalePrevious
Var ScaleRect
Var ScaleLeft
Var ScaleRight
Var ScaleBottom
Var NavFont
Var ControlFont
Var HeaderFont
Var SubHeaderFont
Var InstallOverlayCtrl
Var InstallOverlayBmp

Page custom WelcomePage
Page custom DirectoryPage
Page custom ComponentsPage
Page custom InstallPage
!insertmacro MUI_PAGE_INSTFILES
Page custom FinishPage
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Function .onInit
    SetShellVarContext current
    StrCpy $CreateShortcut "1"
    InitPluginsDir
    SetOutPath $PLUGINSDIR
    File "${PAGE_BITMAPS}\welcome.bmp"
    File "${PAGE_BITMAPS}\directory.bmp"
    File "${PAGE_BITMAPS}\components.bmp"
    File "${PAGE_BITMAPS}\install.bmp"
    File "${PAGE_BITMAPS}\finish.bmp"
FunctionEnd

Function ScaleChildrenWidth
    Pop $ScaleParent
    StrCpy $ScalePrevious 0
scale_children_loop:
    System::Call 'user32::FindWindowEx(p $ScaleParent, p $ScalePrevious, w 0, w 0) p .R9'
    StrCmp $R9 0 scale_children_done
    System::Alloc 16
    Pop $ScaleRect
    System::Call 'user32::GetWindowRect(p $R9, p $ScaleRect)'
    System::Call 'user32::MapWindowPoints(p 0, p $ScaleParent, p $ScaleRect, i 2)'
    System::Call '*$ScaleRect(i .rScaleLeft, i .r7, i .rScaleRight, i .rScaleBottom)'
    IntOp $R8 $ScaleLeft * $WizardScale
    IntOp $ScaleLeft $R8 / 10000
    IntOp $R8 $ScaleRight * $WizardScale
    IntOp $ScaleRight $R8 / 10000
    System::Call '*$ScaleRect(i $ScaleLeft, i $7, i $ScaleRight, i $ScaleBottom)'
    System::Call 'user32::MapWindowPoints(p $ScaleParent, p 0, p $ScaleRect, i 2)'
    System::Call '*$ScaleRect(i .rScaleLeft, i .r7, i .rScaleRight, i .rScaleBottom)'
    IntOp $ScaleRight $ScaleRight - $ScaleLeft
    IntOp $ScaleBottom $ScaleBottom - $7
    System::Call 'user32::SetWindowPos(p $R9, p 0, i $ScaleLeft, i $7, i $ScaleRight, i $ScaleBottom, i 0x14)'
    System::Free $ScaleRect
    StrCpy $ScalePrevious $R9
    Goto scale_children_loop
scale_children_done:
FunctionEnd

Function ExpandWizardWidth
    System::Alloc 16
    Pop $0
    System::Call 'user32::GetWindowRect(p $HWNDPARENT, p $0)'
    System::Call '*$0(i .r1, i .r2, i .r3, i .r4)'
    System::Free $0
    IntOp $WizardWidth $3 - $1
    IntOp $WizardHeight $4 - $2
    IntOp $R8 $WizardWidth * 115
    IntOp $5 $R8 / 100
    System::Call 'user32::GetSystemMetrics(i 0) i .r6'
    IntOp $6 $6 - 48
    ${If} $5 > $6
        StrCpy $5 $6
    ${EndIf}
    IntOp $R8 $5 * 10000
    IntOp $WizardScale $R8 / $WizardWidth
    StrCpy $WizardWidth $5
    Push $HWNDPARENT
    Call ScaleChildrenWidth
    System::Call 'user32::GetSystemMetrics(i 0) i .r6'
    System::Call 'user32::GetSystemMetrics(i 1) i .r7'
    IntOp $6 $6 - $WizardWidth
    IntOp $6 $6 / 2
    IntOp $7 $7 - $WizardHeight
    IntOp $7 $7 / 2
    System::Call 'user32::SetWindowPos(p $HWNDPARENT, p 0, i $6, i $7, i $WizardWidth, i $WizardHeight, i 0x14)'
FunctionEnd

Function ExpandDialogToTop
    Pop $0
    System::Alloc 16
    Pop $1
    System::Call 'user32::GetWindowRect(p $0, p $1)'
    System::Call 'user32::MapWindowPoints(p 0, p $HWNDPARENT, p $1, i 2)'
    System::Call '*$1(i .r2, i .r3, i .r4, i .r5)'
    IntOp $4 $4 - $2
    StrCpy $3 0
    System::Call 'user32::SetWindowPos(p $0, p 0, i $2, i $3, i $4, i $5, i 0x14)'
    System::Free $1
FunctionEnd

Function RemoveWizardHeader
    GetDlgItem $0 $HWNDPARENT 1034
    ShowWindow $0 ${SW_HIDE}
    GetDlgItem $0 $HWNDPARENT 1035
    ShowWindow $0 ${SW_HIDE}
    GetDlgItem $0 $HWNDPARENT 1036
    ShowWindow $0 ${SW_HIDE}
    GetDlgItem $0 $HWNDPARENT 1037
    ShowWindow $0 ${SW_HIDE}
    GetDlgItem $0 $HWNDPARENT 1038
    ShowWindow $0 ${SW_HIDE}
    GetDlgItem $0 $HWNDPARENT 1039
    ShowWindow $0 ${SW_HIDE}
    GetDlgItem $0 $HWNDPARENT 1045
    ShowWindow $0 ${SW_HIDE}
    GetDlgItem $0 $HWNDPARENT 1046
    ShowWindow $0 ${SW_HIDE}
FunctionEnd

Function FennecGuiInit
    Call ExpandWizardWidth
    Call RemoveWizardHeader
    CreateFont $NavFont "Segoe UI" 11 "600"
    CreateFont $ControlFont "Segoe UI" 11 "400"
    CreateFont $HeaderFont "Segoe UI" 12 "700"
    CreateFont $SubHeaderFont "Segoe UI" 10 "400"
    GetDlgItem $0 $HWNDPARENT 1
    SendMessage $0 ${WM_SETFONT} $NavFont 1
    GetDlgItem $0 $HWNDPARENT 2
    SendMessage $0 ${WM_SETFONT} $NavFont 1
    GetDlgItem $0 $HWNDPARENT 3
    SendMessage $0 ${WM_SETFONT} $NavFont 1
    GetDlgItem $0 $HWNDPARENT 1037
    SendMessage $0 ${WM_SETFONT} $HeaderFont 1
    GetDlgItem $0 $HWNDPARENT 1038
    SendMessage $0 ${WM_SETFONT} $SubHeaderFont 1
    System::Call 'dwmapi::DwmSetWindowAttribute(p $HWNDPARENT, i 20, *i 1, i 4)'
    SetCtlColors $HWNDPARENT "EDEDED" "141414"
    GetDlgItem $0 $HWNDPARENT 1
    SetCtlColors $0 "141414" "EDEDED"
    GetDlgItem $0 $HWNDPARENT 2
    SetCtlColors $0 "EDEDED" "1F1F1F"
    GetDlgItem $0 $HWNDPARENT 3
    SetCtlColors $0 "EDEDED" "1F1F1F"
FunctionEnd

Function HiddenInstFilesShow
    FindWindow $0 "#32770" "" $HWNDPARENT
    Push $0
    Call ExpandDialogToTop
    SetCtlColors $0 "EDEDED" "141414"
    GetDlgItem $1 $0 1004
    ShowWindow $1 ${SW_HIDE}
    GetDlgItem $1 $0 1006
    ShowWindow $1 ${SW_HIDE}
    GetDlgItem $1 $0 1016
    ShowWindow $1 ${SW_HIDE}
    GetDlgItem $1 $0 1027
    ShowWindow $1 ${SW_HIDE}
    System::Alloc 16
    Pop $1
    System::Call 'user32::GetWindowRect(p $0, p $1)'
    System::Call '*$1(i .r2, i .r3, i .r4, i .r5)'
    IntOp $4 $4 - $2
    IntOp $5 $5 - $3
    StrCpy $2 0
    StrCpy $3 0
    System::Free $1
    System::Call 'kernel32::GetModuleHandle(p 0) p .R9'
    System::Call 'user32::CreateWindowEx(i 0, t "Static", t "", i 0x5000000E, i $2, i $3, i $4, i $5, p $0, p 0, p $R9, p 0) p .R8'
    StrCpy $InstallOverlayCtrl $R8
    System::Call 'user32::SetWindowPos(p $InstallOverlayCtrl, p -1, i 0, i 0, i 0, i 0, i 0x13)'
    ${NSD_SetStretchedBitmap} $InstallOverlayCtrl "$PLUGINSDIR\install.bmp" $InstallOverlayBmp
    GetDlgItem $1 $HWNDPARENT 1
    SendMessage $1 ${WM_SETTEXT} 0 "STR:Finish"
    System::Call 'user32::InvalidateRect(p $0, p 0, i 1)'
    System::Call 'user32::UpdateWindow(p $0)'
FunctionEnd

Function HiddenInstFilesLeave
    System::Call 'gdi32::DeleteObject(p $InstallOverlayBmp)'
FunctionEnd

Function WelcomePage
    nsDialogs::Create 1018
    Pop $Dialog
    Push $Dialog
    Call ExpandDialogToTop
    SetCtlColors $Dialog "EDEDED" "141414"
    ${NSD_CreateBitmap} 0 0 100% 100% ""
    Pop $ImageCtrl
    ${NSD_SetStretchedBitmap} $ImageCtrl "$PLUGINSDIR\welcome.bmp" $BmpHandle
    GetDlgItem $1 $HWNDPARENT 1
    SendMessage $1 ${WM_SETTEXT} 0 "STR:Get started"
    SetCtlColors $1 "141414" "EDEDED"
    nsDialogs::Show
    ${NSD_FreeBitmap} $BmpHandle
FunctionEnd

Function DirectoryPage
    nsDialogs::Create 1018
    Pop $Dialog
    Push $Dialog
    Call ExpandDialogToTop
    SetCtlColors $Dialog "EDEDED" "141414"
    ${NSD_CreateBitmap} 0 0 100% 100% ""
    Pop $ImageCtrl
    ${NSD_SetStretchedBitmap} $ImageCtrl "$PLUGINSDIR\directory.bmp" $BmpHandle
    ${NSD_CreateText} 30% 49% 48% 38px "$INSTDIR"
    Pop $PathInput
    SendMessage $PathInput ${WM_SETFONT} $SubHeaderFont 1
    SetCtlColors $PathInput "EDEDED" "1F1F1F"
    ${NSD_OnChange} $PathInput SavePath
    ${NSD_CreateButton} 80% 49% 12% 38px "Browse..."
    Pop $BrowseButton
    SendMessage $BrowseButton ${WM_SETFONT} $SubHeaderFont 1
    SetCtlColors $BrowseButton "141414" "EDEDED"
    ${NSD_OnClick} $BrowseButton BrowseFolder
    System::Call 'user32::SetWindowPos(p $ImageCtrl, p 1, i 0, i 0, i 0, i 0, i 0x13)'
    GetDlgItem $1 $HWNDPARENT 1
    SendMessage $1 ${WM_SETTEXT} 0 "STR:Next >"
    nsDialogs::Show
    ${NSD_FreeBitmap} $BmpHandle
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
    Push $Dialog
    Call ExpandDialogToTop
    SetCtlColors $Dialog "EDEDED" "141414"
    ${NSD_CreateBitmap} 0 0 100% 100% ""
    Pop $ImageCtrl
    ${NSD_SetStretchedBitmap} $ImageCtrl "$PLUGINSDIR\components.bmp" $BmpHandle
        ${NSD_CreateCheckBox} 30% 52% 60% 36px "Fennec Desktop (required)"
    Pop $0
    SendMessage $0 ${WM_SETFONT} $ControlFont 1
    ${NSD_Check} $0
    EnableWindow $0 0
    SetCtlColors $0 "EDEDED" "25282D"
        ${NSD_CreateCheckBox} 30% 61% 60% 36px "Desktop shortcut"
    Pop $ShortcutCheck
    SendMessage $ShortcutCheck ${WM_SETFONT} $ControlFont 1
    ${NSD_Check} $ShortcutCheck
    SetCtlColors $ShortcutCheck "EDEDED" "25282D"
    ${NSD_OnClick} $ShortcutCheck SaveShortcut
    System::Call 'user32::SetWindowPos(p $ImageCtrl, p 1, i 0, i 0, i 0, i 0, i 0x13)'
    GetDlgItem $1 $HWNDPARENT 1
    SendMessage $1 ${WM_SETTEXT} 0 "STR:Next >"
    SetCtlColors $1 "141414" "EDEDED"
    nsDialogs::Show
    ${NSD_FreeBitmap} $BmpHandle
FunctionEnd

Function SaveShortcut
    ${NSD_GetState} $ShortcutCheck $CreateShortcut
FunctionEnd

Function InstallPage
    nsDialogs::Create 1018
    Pop $Dialog
    Push $Dialog
    Call ExpandDialogToTop
    SetCtlColors $Dialog "EDEDED" "141414"
    ${NSD_CreateBitmap} 0 0 100% 100% ""
    Pop $ImageCtrl
    ${NSD_SetStretchedBitmap} $ImageCtrl "$PLUGINSDIR\install.bmp" $BmpHandle
    GetDlgItem $1 $HWNDPARENT 1
    SendMessage $1 ${WM_SETTEXT} 0 "STR:Install"
    SetCtlColors $1 "141414" "EDEDED"
    nsDialogs::Show
    ${NSD_FreeBitmap} $BmpHandle
FunctionEnd

Function FinishPage
    nsDialogs::Create 1018
    Pop $Dialog
    Push $Dialog
    Call ExpandDialogToTop
    SetCtlColors $Dialog "EDEDED" "141414"
    ${NSD_CreateBitmap} 0 0 100% 100% ""
    Pop $ImageCtrl
    ${NSD_SetStretchedBitmap} $ImageCtrl "$PLUGINSDIR\finish.bmp" $BmpHandle
    GetDlgItem $1 $HWNDPARENT 1
    SendMessage $1 ${WM_SETTEXT} 0 "STR:Close"
    SetCtlColors $1 "141414" "EDEDED"
    GetDlgItem $1 $HWNDPARENT 3
    EnableWindow $1 0
    nsDialogs::Show
    ${NSD_FreeBitmap} $BmpHandle
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
