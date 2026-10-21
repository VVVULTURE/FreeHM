; FreeHM Windows installer (Inno Setup 6).
; Build:  ISCC.exe /DAppVersion=0.1.0 /DBinDir=..\..\target\release installer\windows\freehm.iss
;
; One elevation, at install time. The installer asks for administrator rights only to
; install into Program Files and (optionally) register the hardware helper service; the
; app itself always runs as the signed-in user. "Install for me only" works without
; elevation and skips the helper (it can be added later from Settings).

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef BinDir
  #define BinDir "..\..\target\release"
#endif

[Setup]
AppId={{F0309FCD-477A-4D75-A8ED-61F6BFC20E4C}
AppName=FreeHM
AppVersion={#AppVersion}
AppVerName=FreeHM {#AppVersion}
AppPublisher=FreeHM contributors
AppPublisherURL=https://github.com/VVVULTURE/FreeHM
AppSupportURL=https://github.com/VVVULTURE/FreeHM/issues
DefaultDirName={autopf}\FreeHM
DisableProgramGroupPage=yes
PrivilegesRequired=admin
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
OutputDir=..\..\dist
OutputBaseFilename=FreeHM-{#AppVersion}-windows-x64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
SetupIconFile=..\..\assets\icons\freehm.ico
UninstallDisplayIcon={app}\freehm.exe
UninstallDisplayName=FreeHM
LicenseFile=..\..\LICENSE
; HKCU entries are the signed-in user's autostart value (removed on uninstall).
UsedUserAreasWarning=no
; We stop FreeHM ourselves (it has no windows for Restart Manager to close).
CloseApplications=no
VersionInfoVersion={#AppVersion}
VersionInfoDescription=FreeHM installer

[Messages]
WelcomeLabel2=This will install FreeHM [ver], a lightweight hardware statistics overlay for games.%n%nFreeHM never injects into games. Administrator rights are used only now, to install into Program Files and optionally register the hardware helper service.

[Tasks]
Name: helper; Description: "Install the hardware helper service (FPS and frame time; CPU temperature when PawnIO is installed)"; Check: IsAdminInstallMode

[Files]
Source: "{#BinDir}\freehm.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\freehm-helper.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\..\THIRD_PARTY_NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\assets\fonts\OFL-Exo2.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "..\..\crates\freehm-helper\modules\COPYING.LGPL-2.1"; DestDir: "{app}\licenses"; DestName: "PawnIO-modules-LGPL-2.1.txt"; Flags: ignoreversion
#ifexist "..\..\dist\THIRD_PARTY_LICENSES.html"
Source: "..\..\dist\THIRD_PARTY_LICENSES.html"; DestDir: "{app}\licenses"; Flags: ignoreversion
#endif

[Icons]
Name: "{autoprograms}\FreeHM"; Filename: "{app}\freehm.exe"; Comment: "FreeHM hardware overlay settings"

[Registry]
; Created by FreeHM itself on first run (Settings > Startup); removed here on uninstall.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "FreeHM"; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run"; ValueType: none; ValueName: "FreeHM"; Flags: uninsdeletevalue

[Run]
Filename: "{app}\freehm-helper.exe"; Parameters: "install"; StatusMsg: "Registering the FreeHM hardware helper service..."; Flags: runhidden waituntilterminated; Tasks: helper
Filename: "{app}\freehm.exe"; Description: "Start FreeHM"; Flags: nowait postinstall runasoriginaluser skipifsilent

[UninstallRun]
Filename: "{app}\freehm.exe"; Parameters: "--quit"; Flags: runhidden waituntilterminated; RunOnceId: "QuitApp"
Filename: "{app}\freehm-helper.exe"; Parameters: "uninstall"; Flags: runhidden waituntilterminated; RunOnceId: "RemoveHelper"; Check: IsAdminInstallMode

[Code]
function ServiceInstalled(): Boolean;
var
  Code: Integer;
begin
  Result := Exec(ExpandConstant('{sys}\sc.exe'), 'query FreeHMHelper', '', SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0);
end;

// Stop a running FreeHM (and the helper, so its file can be replaced) before upgrading.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
begin
  Result := '';
  if FileExists(ExpandConstant('{app}\freehm.exe')) then
    ExecAsOriginalUser(ExpandConstant('{app}\freehm.exe'), '--quit', '', SW_HIDE, ewWaitUntilTerminated, Code);
  if IsAdminInstallMode and ServiceInstalled() then
    Exec(ExpandConstant('{sys}\sc.exe'), 'stop FreeHMHelper', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Sleep(1500);
end;

// After an upgrade without the helper task, restart a helper that was installed before.
procedure CurStepChanged(CurStep: TSetupStep);
var
  Code: Integer;
begin
  if (CurStep = ssPostInstall) and IsAdminInstallMode and (not WizardIsTaskSelected('helper')) and ServiceInstalled() then
    Exec(ExpandConstant('{sys}\sc.exe'), 'start FreeHMHelper', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Code: Integer;
begin
  // A per-user install may still have a helper added later from Settings: removing a
  // service needs administrator rights, so ask Windows for them (one UAC prompt).
  if (CurUninstallStep = usUninstall) and (not IsAdminInstallMode) and ServiceInstalled() then
    ShellExec('runas', ExpandConstant('{app}\freehm-helper.exe'), 'uninstall', '', SW_HIDE, ewWaitUntilTerminated, Code);
  if (CurUninstallStep = usPostUninstall) and (not UninstallSilent) then
    if MsgBox('Also delete your FreeHM settings and logs?', mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
    begin
      DelTree(ExpandConstant('{userappdata}\FreeHM'), True, True, True);
      DelTree(ExpandConstant('{localappdata}\FreeHM'), True, True, True);
    end;
end;
