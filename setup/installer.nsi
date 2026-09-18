Unicode true
ManifestDPIAware true

!ifndef APP_VERSION
    !error "APP_VERSION must be provided with /DAPP_VERSION=<version>"
!endif
!ifndef AMD64_BINARY
    !error "AMD64_BINARY must be provided"
!endif
!ifndef I386_BINARY
    !error "I386_BINARY must be provided"
!endif
!define INSTALLER_FILENAME "fw-windows-setup-v${APP_VERSION}.exe"
!ifndef OUTPUT_FILE
    !define OUTPUT_FILE "${INSTALLER_FILENAME}"
!endif

!define PRODUCT_NAME "FW (Cloudflare Local Forwarder)"
!define PRODUCT_PUBLISHER "then77"
!define PRODUCT_REGISTRY_KEY "Software\FW"
!define PRODUCT_UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\FW"
!define PRODUCT_URL "https://github.com/then77/cf-fw"

!define MULTIUSER_EXECUTIONLEVEL Highest
!define MULTIUSER_MUI
!define MULTIUSER_INSTALLMODE_COMMANDLINE
!define MULTIUSER_INSTALLMODE_DEFAULT_CURRENTUSER
!define MULTIUSER_INSTALLMODE_DEFAULT_REGISTRY_KEY "${PRODUCT_REGISTRY_KEY}"
!define MULTIUSER_INSTALLMODE_DEFAULT_REGISTRY_VALUENAME "InstallMode"
!define MULTIUSER_INSTALLMODE_FUNCTION FW.InstallModeChanged
!define MULTIUSER_INSTALLMODE_UNFUNCTION un.FW.InstallModeChanged
!define MULTIUSER_INSTALLMODEPAGE_TEXT_TOP "Choose whether FW is installed only for you or for everyone who uses this computer."
!define MULTIUSER_INSTALLMODEPAGE_TEXT_CURRENTUSER "Install for me only"
!define MULTIUSER_INSTALLMODEPAGE_TEXT_ALLUSERS "Install for all users"
!define MULTIUSER_INSTALLMODEPAGE_SHOWUSERNAME

!include "MultiUser.nsh"
!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "nsDialogs.nsh"
!include "x64.nsh"

!if /FileExists "assets\installer-sidebar.bmp"
    !define MUI_WELCOMEFINISHPAGE_BITMAP "assets\installer-sidebar.bmp"
    !define MUI_WELCOMEFINISHPAGE_BITMAP_STRETCH AspectFitHeight
    !define MUI_UNWELCOMEFINISHPAGE_BITMAP "assets\installer-sidebar.bmp"
    !define MUI_UNWELCOMEFINISHPAGE_BITMAP_STRETCH AspectFitHeight
!endif

Name "${PRODUCT_NAME} ${APP_VERSION}"
OutFile "${OUTPUT_FILE}"
InstallDir "$LOCALAPPDATA\Programs\FW"
ShowInstDetails show
ShowUninstDetails show

VIProductVersion "${APP_VERSION}.0"
VIAddVersionKey /LANG=1033 "ProductName" "${PRODUCT_NAME}"
VIAddVersionKey /LANG=1033 "ProductVersion" "${APP_VERSION}"
VIAddVersionKey /LANG=1033 "CompanyName" "${PRODUCT_PUBLISHER}"
VIAddVersionKey /LANG=1033 "FileDescription" "FW installer"
VIAddVersionKey /LANG=1033 "FileVersion" "${APP_VERSION}"
VIAddVersionKey /LANG=1033 "LegalCopyright" "Copyright ${PRODUCT_PUBLISHER}"
VIAddVersionKey /LANG=1033 "Comments" "Official repository: ${PRODUCT_URL}"
VIAddVersionKey /LANG=1033 "OriginalFilename" "${INSTALLER_FILENAME}"

Var AddPathCheckbox
Var AddPathDescription
Var PathState
Var AddPath
Var EnableAlias
Var SummaryLabel

!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_TEXT "Run FW Setup"
!define MUI_FINISHPAGE_RUN_FUNCTION LaunchFWSetup

!insertmacro MUI_PAGE_WELCOME
!insertmacro MULTIUSER_PAGE_INSTALLMODE
!insertmacro MUI_PAGE_DIRECTORY
Page custom PathOptionsPageCreate PathOptionsPageLeave
Page custom ConfirmPageCreate
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_UNPAGE_FINISH

!insertmacro MUI_LANGUAGE "English"

Function .onInit
    !insertmacro MULTIUSER_INIT
    InitPluginsDir
    File /oname=$PLUGINSDIR\fw-path-integration.ps1 "fw-path-integration.ps1"
FunctionEnd

Function un.onInit
    !insertmacro MULTIUSER_UNINIT
FunctionEnd

Function FW.InstallModeChanged
    ${If} $MultiUser.InstallMode == "AllUsers"
        ${If} ${RunningX64}
            StrCpy $INSTDIR "$PROGRAMFILES64\FW"
        ${Else}
            StrCpy $INSTDIR "$PROGRAMFILES\FW"
        ${EndIf}
    ${Else}
        StrCpy $INSTDIR "$LOCALAPPDATA\Programs\FW"
    ${EndIf}
FunctionEnd

Function un.FW.InstallModeChanged
FunctionEnd

Function RunPathPrecheck
    nsExec::ExecToStack '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\fw-path-integration.ps1" -Action Check -Scope "$MultiUser.InstallMode" -InstallDir "$INSTDIR"'
    Pop $PathState
    Pop $0

    ${If} $PathState == 0
        StrCpy $AddPath 1
        StrCpy $EnableAlias 1
    ${ElseIf} $PathState == 1
        StrCpy $AddPath 1
        StrCpy $EnableAlias 0
    ${Else}
        StrCpy $PathState 2
        StrCpy $AddPath 0
        StrCpy $EnableAlias 0
    ${EndIf}
FunctionEnd

Function PathOptionsPageCreate
    Call RunPathPrecheck
    nsDialogs::Create 1018
    Pop $0
    ${If} $0 == error
        Abort
    ${EndIf}

    !insertmacro MUI_HEADER_TEXT "Installation Options" "Choose how FW is made available from terminals."

    ${NSD_CreateCheckbox} 0 8u 100% 14u "Add FW to PATH"
    Pop $AddPathCheckbox
    ${NSD_CreateLabel} 14u 28u 94% 34u ""
    Pop $AddPathDescription

    ${If} $PathState == 0
        ${NSD_Check} $AddPathCheckbox
        ${NSD_SetText} $AddPathDescription "Lets you run FW from a terminal using fw <port>."
    ${ElseIf} $PathState == 1
        ${NSD_Check} $AddPathCheckbox
        ${NSD_SetText} $AddPathDescription "Lets you run FW from a terminal using fw.exe <port>."
    ${Else}
        ${NSD_Uncheck} $AddPathCheckbox
        EnableWindow $AddPathCheckbox 0
        ${NSD_SetText} $AddPathDescription "Can't install FW to PATH due to collision with existing fw command."
    ${EndIf}

    nsDialogs::Show
FunctionEnd

Function PathOptionsPageLeave
    ${If} $PathState == 2
        StrCpy $AddPath 0
        StrCpy $EnableAlias 0
        Return
    ${EndIf}

    ${NSD_GetState} $AddPathCheckbox $0
    ${If} $0 == ${BST_CHECKED}
        StrCpy $AddPath 1
        ${If} $PathState == 0
            StrCpy $EnableAlias 1
        ${Else}
            StrCpy $EnableAlias 0
        ${EndIf}
    ${Else}
        StrCpy $AddPath 0
        StrCpy $EnableAlias 0
    ${EndIf}
FunctionEnd

Function ConfirmPageCreate
    nsDialogs::Create 1018
    Pop $0
    ${If} $0 == error
        Abort
    ${EndIf}

    !insertmacro MUI_HEADER_TEXT "Ready to Install" "Review your choices before installing FW."

    ${If} $MultiUser.InstallMode == "AllUsers"
        StrCpy $0 "All users"
    ${Else}
        StrCpy $0 "Current user"
    ${EndIf}

    ${If} $AddPath == 1
        StrCpy $1 "Yes"
    ${Else}
        StrCpy $1 "No"
    ${EndIf}

    ${If} $EnableAlias == 1
        StrCpy $2 "Yes (fw <port>)"
    ${ElseIf} $AddPath == 1
        StrCpy $2 "No (fw.exe <port>)"
    ${Else}
        StrCpy $2 "No"
    ${EndIf}

    ${NSD_CreateLabel} 0 8u 100% 12u "Installation scope: $0"
    Pop $SummaryLabel
    ${NSD_CreateLabel} 0 24u 100% 12u "Install location: $INSTDIR"
    Pop $SummaryLabel
    ${NSD_CreateLabel} 0 40u 100% 12u "Add FW to PATH: $1"
    Pop $SummaryLabel
    nsDialogs::Show
FunctionEnd

Section "Install"
    SetOutPath "$INSTDIR"
    ${If} ${RunningX64}
        File /oname=fw.exe "${AMD64_BINARY}"
    ${Else}
        File /oname=fw.exe "${I386_BINARY}"
    ${EndIf}

    CreateDirectory "$INSTDIR\uninstall-data"
    SetOutPath "$INSTDIR\uninstall-data"
    File /oname=fw-path-integration.ps1 "fw-path-integration.ps1"
    SetOutPath "$INSTDIR"

    ${If} $AddPath == 1
        ${If} $EnableAlias == 1
            nsExec::ExecToLog '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$INSTDIR\uninstall-data\fw-path-integration.ps1" -Action Install -Scope "$MultiUser.InstallMode" -InstallDir "$INSTDIR" -AddPath -EnableAlias'
        ${Else}
            nsExec::ExecToLog '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$INSTDIR\uninstall-data\fw-path-integration.ps1" -Action Install -Scope "$MultiUser.InstallMode" -InstallDir "$INSTDIR" -AddPath'
        ${EndIf}
        Pop $0
        ${If} $0 != 0
            MessageBox MB_ICONEXCLAMATION|MB_OK "FW was installed, but PATH integration failed. You can still run $INSTDIR\fw.exe directly."
            StrCpy $AddPath 0
            StrCpy $EnableAlias 0
        ${EndIf}
    ${EndIf}

    WriteUninstaller "$INSTDIR\Uninstall.exe"

    WriteRegStr SHCTX "${PRODUCT_REGISTRY_KEY}" "InstallMode" "$MultiUser.InstallMode"
    WriteRegStr SHCTX "${PRODUCT_REGISTRY_KEY}" "InstallDir" "$INSTDIR"
    WriteRegDWORD SHCTX "${PRODUCT_REGISTRY_KEY}" "PathAdded" $AddPath
    WriteRegDWORD SHCTX "${PRODUCT_REGISTRY_KEY}" "AliasIntegrated" $EnableAlias

    WriteRegStr SHCTX "${PRODUCT_UNINSTALL_KEY}" "DisplayName" "${PRODUCT_NAME}"
    WriteRegStr SHCTX "${PRODUCT_UNINSTALL_KEY}" "DisplayVersion" "${APP_VERSION}"
    WriteRegStr SHCTX "${PRODUCT_UNINSTALL_KEY}" "Publisher" "${PRODUCT_PUBLISHER}"
    WriteRegStr SHCTX "${PRODUCT_UNINSTALL_KEY}" "URLInfoAbout" "${PRODUCT_URL}"
    WriteRegStr SHCTX "${PRODUCT_UNINSTALL_KEY}" "HelpLink" "${PRODUCT_URL}/issues"
    WriteRegStr SHCTX "${PRODUCT_UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
    WriteRegStr SHCTX "${PRODUCT_UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
    WriteRegDWORD SHCTX "${PRODUCT_UNINSTALL_KEY}" "NoModify" 1
    WriteRegDWORD SHCTX "${PRODUCT_UNINSTALL_KEY}" "NoRepair" 1
SectionEnd

Section "Uninstall"
    ReadRegDWORD $0 SHCTX "${PRODUCT_REGISTRY_KEY}" "PathAdded"
    ReadRegDWORD $1 SHCTX "${PRODUCT_REGISTRY_KEY}" "AliasIntegrated"

    ${If} ${FileExists} "$INSTDIR\uninstall-data\fw-path-integration.ps1"
        ${If} $0 == 1
            ${If} $1 == 1
                nsExec::ExecToLog '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$INSTDIR\uninstall-data\fw-path-integration.ps1" -Action Uninstall -Scope "$MultiUser.InstallMode" -InstallDir "$INSTDIR" -RemovePath -RemoveAlias'
            ${Else}
                nsExec::ExecToLog '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$INSTDIR\uninstall-data\fw-path-integration.ps1" -Action Uninstall -Scope "$MultiUser.InstallMode" -InstallDir "$INSTDIR" -RemovePath'
            ${EndIf}
            Pop $2
        ${ElseIf} $1 == 1
            nsExec::ExecToLog '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$INSTDIR\uninstall-data\fw-path-integration.ps1" -Action Uninstall -Scope "$MultiUser.InstallMode" -InstallDir "$INSTDIR" -RemoveAlias'
            Pop $2
        ${EndIf}
    ${EndIf}

    Delete "$INSTDIR\fw.exe"
    Delete "$INSTDIR\uninstall-data\fw-path-integration.ps1"
    RMDir "$INSTDIR\uninstall-data"
    Delete "$INSTDIR\Uninstall.exe"
    RMDir "$INSTDIR"

    DeleteRegKey SHCTX "${PRODUCT_UNINSTALL_KEY}"
    DeleteRegKey SHCTX "${PRODUCT_REGISTRY_KEY}"
SectionEnd

Function LaunchFWSetup
    Exec '"$INSTDIR\fw.exe" --setup'
FunctionEnd
