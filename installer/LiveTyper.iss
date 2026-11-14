; LiveTyper installer (Inno Setup 6). Build it with scripts\build-installer.ps1.
;
; It installs for the current user only (no administrator rights): the app, a Start menu
; shortcut, the Microsoft Visual C++ runtime if the PC does not have it, and the two speech
; models if they are not already in %LOCALAPPDATA%\LiveTyper\models. The models download from
; Hugging Face during the setup, and their SHA-256 values are checked. This is the only network
; step of LiveTyper (ADR 0001).

#ifndef AppVersion
  #error Give /DAppVersion=x.y.z (scripts\build-installer.ps1 does this)
#endif
#ifndef VcRedist
  #error Give /DVcRedist=<path of vc_redist.x64.exe> (scripts\build-installer.ps1 does this)
#endif

[Setup]
AppId={{79FC2CAB-079E-4D83-9309-54D0FDE09F45}
AppName=LiveTyper
AppVersion={#AppVersion}
AppPublisher=LiveTyper
DefaultDirName={autopf}\LiveTyper
DefaultGroupName=LiveTyper
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=LiveTyper-Setup-{#AppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\livetyper.exe
UninstallDisplayName=LiveTyper
AppMutex=Local\LiveTyper.SingleInstance
RestartApplications=no

[Files]
Source: "..\assets\fonts\DejaVuSans-LICENSE.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "..\target\release\livetyper.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#VcRedist}"; DestDir: "{tmp}"; DestName: "vc_redist.x64.exe"; Flags: deleteafterinstall; Check: VcRuntimeMissing

[Icons]
Name: "{autoprograms}\LiveTyper"; Filename: "{app}\livetyper.exe"

[Tasks]
Name: "startup"; Description: "Start LiveTyper when I sign in to Windows"
Name: "startup\folder"; Description: "With a shortcut in the Startup folder"; Flags: exclusive
Name: "startup\registry"; Description: "With a value in the registry (Run key)"; Flags: exclusive unchecked

[Run]
; vc_redist asks for administrator rights itself (shellexec shows the Windows prompt).
Filename: "{tmp}\vc_redist.x64.exe"; Parameters: "/install /passive /norestart"; StatusMsg: "Installing the Microsoft Visual C++ runtime..."; Flags: shellexec waituntilterminated; Check: VcRuntimeMissing
Filename: "{app}\livetyper.exe"; Description: "Start LiveTyper now"; Flags: postinstall nowait skipifsilent; Check: CanStartLiveTyper

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "LiveTyper"; Flags: uninsdeletevalue

[UninstallDelete]
Type: files; Name: "{userstartup}\LiveTyper.lnk"

[Code]
const
  ModelsUrl = 'https://huggingface.co/';

var
  DownloadPage: TDownloadWizardPage;
  StartupError: Boolean;

function ModelsDir: String;
begin
  Result := ExpandConstant('{localappdata}\LiveTyper\models');
end;

{ The Visual C++ 2015-2022 runtime, version 14.29 or newer (the app is built with 14.29). }
function VcRuntimeMissing: Boolean;
var
  Installed, Minor: Cardinal;
  Key: String;
begin
  Key := 'SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64';
  Result := not (RegQueryDWordValue(HKLM64, Key, 'Installed', Installed) and (Installed = 1)
    and RegQueryDWordValue(HKLM64, Key, 'Minor', Minor) and (Minor >= 29));
end;

procedure AddModel(const Repo, FileName, Sha256: String);
begin
  if FileExists(ModelsDir + '\' + FileName) then
    Log('Model already present: ' + FileName)
  else
    DownloadPage.Add(ModelsUrl + Repo + '/resolve/main/' + FileName, FileName, Sha256);
end;

procedure InitializeWizard;
begin
  DownloadPage := CreateDownloadPage(SetupMessage(msgWizardPreparing),
    'Downloading the speech models (about 1.4 GB). This happens only once.', nil);
end;

{ After the Ready page: download the models that are not present, before anything is installed. }
function NextButtonClick(CurPageID: Integer): Boolean;
begin
  Result := True;
  if CurPageID <> wpReady then
    exit;
  DownloadPage.Clear;
  AddModel('handy-computer/nemotron-speech-streaming-en-0.6b-gguf',
    'nemotron-speech-streaming-en-0.6b-Q8_0.gguf',
    '90d8c89714cd31efc88be62a40c6b2bea57e0cc2063af1ffe2c28f1a228ca110');
  AddModel('handy-computer/parakeet-tdt-0.6b-v3-gguf',
    'parakeet-tdt-0.6b-v3-Q8_0.gguf',
    '5859f77944efcd8eafa23a6350731960b2b55b2203df51f319665c807d802cc7');
  DownloadPage.Show;
  try
    try
      DownloadPage.Download;
    except
      if DownloadPage.AbortedByUser then
        Log('Model download stopped by the user.')
      else
        SuppressibleMsgBox(AddPeriod(GetExceptionMessage), mbCriticalError, MB_OK, IDOK);
      Result := False;
    end;
  finally
    DownloadPage.Hide;
  end;
end;

{ Move the downloaded models from the temporary folder into the models folder. }
procedure MoveModel(const FileName: String);
var
  Source, Target: String;
begin
  Source := ExpandConstant('{tmp}\' + FileName);
  Target := ModelsDir + '\' + FileName;
  if not FileExists(Source) then
    exit;
  if not RenameFile(Source, Target) then
    if not FileCopy(Source, Target, False) then
      RaiseException('Cannot write ' + Target);
end;

procedure ConfigureStartup;
var
  Choice: String;
  ExitCode: Integer;
begin
  Choice := 'off';
  if WizardIsTaskSelected('startup\folder') then
    Choice := 'startup_folder'
  else if WizardIsTaskSelected('startup\registry') then
    Choice := 'registry';

  if not Exec(ExpandConstant('{app}\livetyper.exe'), '--startup=' + Choice, '',
    SW_HIDE, ewWaitUntilTerminated, ExitCode) then
  begin
    StartupError := True;
    SuppressibleMsgBox('Cannot start LiveTyper to set Windows startup: ' + SysErrorMessage(ExitCode),
      mbError, MB_OK, IDOK);
  end
  else if ExitCode <> 0 then
  begin
    StartupError := True;
    SuppressibleMsgBox('Cannot set Windows startup. See the LiveTyper log for details.',
      mbError, MB_OK, IDOK);
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssInstall then
  begin
    ForceDirectories(ModelsDir);
    MoveModel('nemotron-speech-streaming-en-0.6b-Q8_0.gguf');
    MoveModel('parakeet-tdt-0.6b-v3-Q8_0.gguf');
  end
  else if CurStep = ssPostInstall then
    ConfigureStartup;
end;

function CanStartLiveTyper: Boolean;
begin
  Result := not StartupError;
end;

function GetCustomSetupExitCode: Integer;
begin
  Result := 0;
  if StartupError then
    Result := 10;
end;

{ The models, settings and log stay unless the user wants them removed. }
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if (CurUninstallStep = usPostUninstall) and not UninstallSilent then
    if MsgBox('Also remove the speech models (about 1.4 GB), the settings and the log?',
      mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
    begin
      DelTree(ExpandConstant('{localappdata}\LiveTyper'), True, True, True);
      DelTree(ExpandConstant('{userappdata}\LiveTyper'), True, True, True);
    end;
end;
