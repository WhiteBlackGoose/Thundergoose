; Inno Setup script for the Windows installer. Built by the release workflow:
;   iscc /DVersion=0.1.0 packaging\windows\thundergoose.iss
#ifndef Version
  #define Version "0.0.0"
#endif

[Setup]
AppId={{B3C1E2A4-7D5F-4E8A-9C61-2F0D7A9E5B13}
AppName=Thundergoose
AppVersion={#Version}
AppPublisher=WhiteBlackGoose
AppPublisherURL=https://github.com/WhiteBlackGoose/Thundergoose
DefaultDirName={autopf}\Thundergoose
DefaultGroupName=Thundergoose
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\thundergoose.exe
SetupIconFile=..\thundergoose.ico
OutputDir=..\..\dist
OutputBaseFilename=Thundergoose-{#Version}-windows-x64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Install for the current user without admin rights, or for everyone.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\..\target\release\thundergoose.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; The AppUserModelID lets Windows show Thundergoose's notifications under its own name.
Name: "{autoprograms}\Thundergoose"; Filename: "{app}\thundergoose.exe"; AppUserModelID: "Thundergoose"
Name: "{autodesktop}\Thundergoose"; Filename: "{app}\thundergoose.exe"; AppUserModelID: "Thundergoose"; Tasks: desktopicon

[Run]
Filename: "{app}\thundergoose.exe"; Description: "{cm:LaunchProgram,Thundergoose}"; Flags: nowait postinstall skipifsilent
