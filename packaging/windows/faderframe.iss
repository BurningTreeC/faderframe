; Inno Setup script for FaderFrame (built by packaging/windows/bundle.sh,
; which passes Version, Source and OutputDir).

#ifndef Version
  #define Version "0.0.0"
#endif
#ifndef Source
  #define Source "..\..\dist\FaderFrame"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif

[Setup]
AppId={{CBCAA8A4-C014-49C4-A27F-0F3FAB5AEDEC}
AppName=FaderFrame
AppVersion={#Version}
AppVerName=FaderFrame {#Version}
AppPublisher=BurningTreeC
AppPublisherURL=https://github.com/BurningTreeC/faderframe
AppSupportURL=https://github.com/BurningTreeC/faderframe/issues
DefaultDirName={autopf}\FaderFrame
DefaultGroupName=FaderFrame
DisableProgramGroupPage=yes
LicenseFile={#Source}\LICENSE
SetupIconFile={#Source}\faderframe.ico
UninstallDisplayIcon={app}\faderframe.ico
OutputDir={#OutputDir}
OutputBaseFilename=FaderFrame-{#Version}-windows-x64-setup
Compression=lzma2/max
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequiredOverridesAllowed=dialog
ChangesAssociations=yes
WizardStyle=modern

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Source}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\FaderFrame"; Filename: "{app}\bin\faderframe.exe"; IconFilename: "{app}\faderframe.ico"
Name: "{autodesktop}\FaderFrame"; Filename: "{app}\bin\faderframe.exe"; IconFilename: "{app}\faderframe.ico"; Tasks: desktopicon

[Registry]
; Open .ffproj projects with FaderFrame.
Root: HKA; Subkey: "Software\Classes\.ffproj"; ValueType: string; ValueName: ""; ValueData: "FaderFrame.Project"; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\FaderFrame.Project"; ValueType: string; ValueName: ""; ValueData: "FaderFrame project"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\FaderFrame.Project\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\faderframe.ico"
Root: HKA; Subkey: "Software\Classes\FaderFrame.Project\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\bin\faderframe.exe"" ""%1"""

[Run]
Filename: "{app}\bin\faderframe.exe"; Description: "{cm:LaunchProgram,FaderFrame}"; Flags: nowait postinstall skipifsilent
