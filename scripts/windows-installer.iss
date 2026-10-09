#define AppName "ReCast"
#define AppExecutable "recast-widget.exe"

[Setup]
AppId=io.github.codex-usage-widget
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=ReCast contributors
DefaultDirName={localappdata}\Programs\{#AppName}
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.22000
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\{#AppExecutable}
SetupIconFile={#ProjectDir}\assets\icon.ico
OutputDir={#InstallerOutputDir}
OutputBaseFilename=recast-widget-{#AppVersion}-windows-x64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "japanese"; MessagesFile: "compiler:Languages\Japanese.isl"

[Messages]
FinishedLabel=[name] のインストールが完了しました。%n%nスタートメニューの「{#AppName}」から起動してください。デスクトップにショートカットを作成した場合は、そちらからも起動できます。

[Tasks]
Name: "desktopicon"; Description: "はい"; GroupDescription: "デスクトップにショートカットを作成しますか？"; Flags: exclusive unchecked
Name: "nodesktopicon"; Description: "いいえ"; GroupDescription: "デスクトップにショートカットを作成しますか？"; Flags: exclusive

[Files]
Source: "{#PayloadDir}\{#AppExecutable}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\THIRD-PARTY-LICENSES.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\assets\FONT-LICENSE.txt"; DestDir: "{app}\assets"; Flags: ignoreversion
Source: "{#PayloadDir}\assets\NOTICE.md"; DestDir: "{app}\assets"; Flags: ignoreversion

[InstallDelete]
; Files left by the Codex Usage Widget versions when updating over them.
Type: files; Name: "{app}\codex-usage-widget.exe"
Type: files; Name: "{userprograms}\Codex Usage Widget.lnk"
Type: files; Name: "{userdesktop}\Codex Usage Widget.lnk"

[Icons]
Name: "{userprograms}\{#AppName}"; Filename: "{app}\{#AppExecutable}"; WorkingDir: "{app}"
Name: "{userdesktop}\{#AppName}"; Filename: "{app}\{#AppExecutable}"; WorkingDir: "{app}"; Tasks: desktopicon

; Launch through the installed shortcuts after Setup exits. On Windows 11,
; Setup's RedirectionGuard was observed on child processes and rejected the
; official Codex CLI's user-created junction with Windows error 448.

[Code]
const
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  ApprovedKey = 'Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run';

function IsRunEntryFor(const Name, Executable: String): Boolean;
var
  Command: String;
begin
  Result := RegQueryStringValue(HKCU, RunKey, Name, Command);
  if Result then
  begin
    Command := Trim(Command);
    Result := (CompareText(Command, Executable) = 0) or
              (CompareText(Command, '"' + Executable + '"') = 0);
  end;
end;

procedure RemoveRunEntry(const Name, Executable: String);
begin
  if IsRunEntryFor(Name, Executable) then
  begin
    RegDeleteValue(HKCU, RunKey, Name);
    RegDeleteValue(HKCU, ApprovedKey, Name);
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  State: AnsiString;
begin
  // Keep starting at login when updating over the Codex Usage Widget versions,
  // even before ReCast is opened for the first time.
  if (CurStep = ssPostInstall) and
     IsRunEntryFor('Codex Usage Widget', ExpandConstant('{app}\codex-usage-widget.exe')) then
  begin
    RegWriteStringValue(HKCU, RunKey, '{#AppName}', '"' + ExpandConstant('{app}\{#AppExecutable}') + '"');
    // A login item turned off in Task Manager stays off.
    if RegQueryBinaryValue(HKCU, ApprovedKey, 'Codex Usage Widget', State) then
      RegWriteBinaryValue(HKCU, ApprovedKey, '{#AppName}', State);
    RemoveRunEntry('Codex Usage Widget', ExpandConstant('{app}\codex-usage-widget.exe'));
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    RemoveRunEntry('{#AppName}', ExpandConstant('{app}\{#AppExecutable}'));
end;
