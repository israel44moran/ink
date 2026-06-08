; Instalador de Ink (motor de tinta) - generado con Inno Setup.
; Instala el ejecutable de release (con el logo incrustado), crea accesos directos en el menu
; inicio y (opcional) en el escritorio, y deja un desinstalador. No requiere administrador.

#define MyAppName "Ink"
#define MyAppVersion "0.2"
#define MyAppExeName "Ink.exe"
; Misma identidad (AppUserModelID) que fija el programa por dentro (SetCurrentProcessExplicitAppUserModelID).
; Debe coincidir EXACTAMENTE para que la barra de tareas asocie el icono al acceso directo.
#define MyAppId "Israel.Ink.Notas"
#define SrcExe "C:\Users\Usuario 1\Desktop\App escritura\target\release\ink-app.exe"
#define IconFile "C:\Users\Usuario 1\Desktop\App escritura\crates\ink-app\assets\icon.ico"

[Setup]
AppId={{A1B2C3D4-E5F6-47A8-9B0C-1D2E3F4A5B6C}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
VersionInfoVersion=0.2.0.0
DefaultDirName={autopf}\Ink
DefaultGroupName=Ink
DisableProgramGroupPage=yes
UninstallDisplayName=Ink
UninstallDisplayIcon={app}\{#MyAppExeName}
OutputDir=C:\Users\Usuario 1\Desktop\App escritura\dist
OutputBaseFilename=Ink-Setup-{#MyAppVersion}
SetupIconFile={#IconFile}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

[Languages]
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: checkedonce

[Files]
Source: "{#SrcExe}"; DestDir: "{app}"; DestName: "{#MyAppExeName}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Ink"; Filename: "{app}\{#MyAppExeName}"; AppUserModelID: "{#MyAppId}"
Name: "{autodesktop}\Ink"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon; AppUserModelID: "{#MyAppId}"

[Run]
; Refrescar la cache de iconos del shell para que el logo aparezca de inmediato en la barra de tareas.
Filename: "{sys}\ie4uinit.exe"; Parameters: "-show"; Flags: runhidden
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,Ink}"; Flags: nowait postinstall skipifsilent
