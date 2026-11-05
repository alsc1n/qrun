; QRun 安装包: 装到当前用户目录 (%LOCALAPPDATA%\Programs\QRun), 不要管理员权限;
; 设置和数据照旧写在 exe 旁, 与绿色版同一套逻辑.
; 先 cargo build --release, 再编译: ISCC /DAppVersion=0.1.0 installer\qrun.iss

#define AppName "QRun"
#define AppExe "qrun.exe"

[Setup]
AppId={{98983307-BC2C-4CD7-BBFD-3CC28C3F50A5}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=alsc1n
AppPublisherURL=https://github.com/alsc1n/qrun
DefaultDirName={autopf}\{#AppName}
DisableProgramGroupPage=yes
DisableWelcomePage=no
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
LicenseFile=..\LICENSE
SetupIconFile=..\assets\qrun.ico
UninstallDisplayIcon={app}\{#AppExe}
WizardStyle=modern dynamic windows11
ShowLanguageDialog=no
OutputDir=..\target\installer
OutputBaseFilename=qrun-{#AppVersion}-x64-setup
Compression=lzma2
SolidCompression=yes
; 应用内更新会静默运行安装包 (/SILENT /DIR=原目录); 万一 QRun 还没退干净, 由安装包关掉它, 不卡在"文件被占用"
CloseApplications=force

[Languages]
Name: "chinesesimplified"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"
Source: "..\NOTICE"; DestDir: "{app}"
Source: "..\assets\fonts\*"; DestDir: "{app}\fonts"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; 开机自启由 QRun 自己写入 (platform/autostart.rs), 卸载时一并删掉, 不留失效的启动项.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "QRun"; Flags: uninsdeletevalue

[Run]
; 不加 skipifsilent: 应用内静默更新装完后要自动重新启动 QRun; 手动安装时仍是完成页的勾选项
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall
