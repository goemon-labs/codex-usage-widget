#define AppName "Codex Usage Widget"
#define AppExecutable "codex-usage-widget.exe"

[Setup]
AppId=io.github.codex-usage-widget
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=Codex Usage Widget contributors
DefaultDirName={localappdata}\Programs\{#AppName}
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.22000
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\{#AppExecutable}
SetupIconFile={#ProjectDir}\assets\icon.ico
OutputDir={#InstallerOutputDir}
OutputBaseFilename=codex-usage-widget-{#AppVersion}-windows-x64-setup
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

[Icons]
Name: "{userprograms}\{#AppName}"; Filename: "{app}\{#AppExecutable}"; WorkingDir: "{app}"
Name: "{userdesktop}\{#AppName}"; Filename: "{app}\{#AppExecutable}"; WorkingDir: "{app}"; Tasks: desktopicon

; Launch through the installed shortcuts after Setup exits. On Windows 11,
; Setup's RedirectionGuard was observed on child processes and rejected the
; official Codex CLI's user-created junction with Windows error 448.

[Code]
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Command, Executable: String;
begin
  if CurUninstallStep = usUninstall then
    if RegQueryStringValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run',
      '{#AppName}', Command) then
    begin
      Command := Trim(Command);
      Executable := ExpandConstant('{app}\{#AppExecutable}');
      if (CompareText(Command, Executable) = 0) or
         (CompareText(Command, '"' + Executable + '"') = 0) then
      begin
        RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', '{#AppName}');
        RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run', '{#AppName}');
      end;
    end;
end;
