; Inferno ARM ASIO installer (Windows on Arm, Inno Setup 6.3+).
; Build with scripts\build-arm64-asio-installer.ps1, which passes:
;   /DAppVersion=x.y.z /DEcDir=<ARM64EC release dir> [/DNativeDir=<ARM64 release dir>] /DRepoRoot=<repo>

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef RepoRoot
  #define RepoRoot ".."
#endif
#ifndef EcDir
  #define EcDir RepoRoot + "\target\arm64ec-pc-windows-msvc\release"
#endif
#ifndef SourceUrl
  #define SourceUrl "https://github.com/c-heck/inferno-windows"
#endif
#ifndef SourceInfo
  #error SourceInfo (generated SOURCE.txt) must be passed by scripts\build-arm64-asio-installer.ps1
#endif

; Fixed: must match the CLSID compiled into inferno_asio.dll and scripts\register-arm64-asio.ps1.
#define AsioClsid "{{C4CD4CF3-37E1-4B6E-AB2F-90016AB0A640}"
#define AsioName "Inferno ARM ASIO"

[Setup]
AppId={{8F2C6B4E-1D7A-4C3B-9E55-6A0B7D2F4C11}
AppName={#AsioName}
AppVersion={#AppVersion}
AppVerName={#AsioName} {#AppVersion}
AppPublisher=Inferno ARM ASIO contributors (unofficial, not affiliated with Audinate)
AppPublisherURL={#SourceUrl}
AppSupportURL={#SourceUrl}
AppUpdatesURL={#SourceUrl}
AppComments=Source code (GNU AGPL v3): {#SourceUrl}
VersionInfoVersion={#AppVersion}
VersionInfoCopyright=GNU AGPL v3 - Inferno AoIP / Inferno ARM ASIO contributors
VersionInfoDescription=Inferno ARM ASIO setup (unofficial, not affiliated with Audinate)
DefaultDirName={autopf}\{#AsioName}
DefaultGroupName={#AsioName}
DisableProgramGroupPage=yes
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
MinVersion=10.0.22000
PrivilegesRequired=admin
CloseApplications=yes
RestartApplications=no
LicenseFile={#RepoRoot}\LICENSE
InfoBeforeFile={#RepoRoot}\installer\before-install.txt
OutputDir={#RepoRoot}\dist
OutputBaseFilename=InfernoArmAsio-{#AppVersion}-arm64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\inferno_control.exe
UninstallDisplayName={#AsioName} {#AppVersion}

[Types]
Name: "ec"; Description: "ARM64EC driver - Nuendo / Cubase on Windows on Arm (recommended)"
#ifdef NativeDir
Name: "native"; Description: "Native ARM64 driver - pure ARM64 ASIO hosts"
#endif

[Components]
#ifdef NativeDir
Name: "driver"; Description: "ASIO driver and control panel"; Types: ec native; Flags: fixed
#else
Name: "driver"; Description: "ASIO driver and control panel"; Types: ec; Flags: fixed
#endif
Name: "driver\ec"; Description: "ARM64EC build"; Types: ec; Flags: exclusive
#ifdef NativeDir
Name: "driver\native"; Description: "Native ARM64 build"; Types: native; Flags: exclusive
#endif

[Tasks]
Name: "firewall"; Description: "Add Windows Firewall rules for Dante (driver ports and installed Nuendo/Cubase)"
Name: "nictune"; Description: "Tune the Dante network adapter (larger queues, no power saving; adapter restarts briefly)"; Flags: unchecked

[Files]
Source: "{#EcDir}\inferno_asio.dll"; DestDir: "{app}"; Components: driver\ec; Flags: ignoreversion restartreplace uninsrestartdelete
Source: "{#EcDir}\inferno_control.exe"; DestDir: "{app}"; Components: driver\ec; Flags: ignoreversion restartreplace uninsrestartdelete
#ifdef NativeDir
Source: "{#NativeDir}\inferno_asio.dll"; DestDir: "{app}"; Components: driver\native; Flags: ignoreversion restartreplace uninsrestartdelete
Source: "{#NativeDir}\inferno_control.exe"; DestDir: "{app}"; Components: driver\native; Flags: ignoreversion restartreplace uninsrestartdelete
#endif
Source: "{#RepoRoot}\scripts\inferno-power.ps1"; DestDir: "{app}\scripts"; Flags: ignoreversion
Source: "{#RepoRoot}\scripts\tune-dante-nic.ps1"; DestDir: "{app}\scripts"; Flags: ignoreversion
Source: "{#RepoRoot}\installer\firewall.ps1"; DestDir: "{app}\scripts"; Flags: ignoreversion
Source: "{#RepoRoot}\inferno_asio\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\installer\NOTICE.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceInfo}"; DestDir: "{app}"; DestName: "SOURCE.txt"; Flags: ignoreversion

[Registry]
; Same entries as scripts\register-arm64-asio.ps1, in the 64-bit HKLM view.
Root: HKLM64; Subkey: "SOFTWARE\ASIO\{#AsioName}"; ValueType: string; ValueName: "CLSID"; ValueData: "{#AsioClsid}"; Flags: uninsdeletekey
Root: HKLM64; Subkey: "SOFTWARE\ASIO\{#AsioName}"; ValueType: string; ValueName: "Description"; ValueData: "{#AsioName} (experimental, unofficial)"
Root: HKLM64; Subkey: "SOFTWARE\Classes\CLSID\{#AsioClsid}"; ValueType: string; ValueName: ""; ValueData: "{#AsioName}"; Flags: uninsdeletekey
Root: HKLM64; Subkey: "SOFTWARE\Classes\CLSID\{#AsioClsid}\InprocServer32"; ValueType: string; ValueName: ""; ValueData: "{app}\inferno_asio.dll"
Root: HKLM64; Subkey: "SOFTWARE\Classes\CLSID\{#AsioClsid}\InprocServer32"; ValueType: string; ValueName: "ThreadingModel"; ValueData: "Both"

[Icons]
Name: "{group}\Inferno ARM ASIO Control Panel"; Filename: "{app}\inferno_control.exe"
Name: "{group}\Inferno ARM ASIO README"; Filename: "{app}\README.md"
Name: "{group}\Notices and licence"; Filename: "{app}\NOTICE.txt"
Name: "{group}\Source code (GNU AGPL v3)"; Filename: "{#SourceUrl}"
Name: "{group}\Uninstall Inferno ARM ASIO"; Filename: "{uninstallexe}"

[Run]
Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\scripts\firewall.ps1"" -Add"; Tasks: firewall; Flags: runhidden waituntilterminated; StatusMsg: "Adding firewall rules..."
Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -Command ""try {{ & '{app}\scripts\tune-dante-nic.ps1' }} catch {{ Write-Host $_ -ForegroundColor Red; Read-Host 'Adapter tuning failed. Press Enter to continue' }"""; Tasks: nictune; Flags: waituntilterminated; StatusMsg: "Tuning the Dante network adapter..."
Filename: "{app}\inferno_control.exe"; Description: "Open the Inferno ARM ASIO control panel (select the Dante network adapter)"; Flags: postinstall nowait skipifsilent runasoriginaluser
Filename: "{app}\NOTICE.txt"; Description: "Show notices, licence and source code location"; Flags: postinstall shellexec nowait skipifsilent unchecked runasoriginaluser

[UninstallRun]
; Restore a power plan left switched by a crashed host, then remove the plan and firewall rules.
Filename: "{app}\inferno_control.exe"; Parameters: "--power-restore"; Flags: runhidden waituntilterminated; RunOnceId: "PowerRestore"
Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\scripts\inferno-power.ps1"" -Uninstall"; Flags: runhidden waituntilterminated; RunOnceId: "PowerPlan"
Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\scripts\firewall.ps1"" -Remove"; Flags: runhidden waituntilterminated; RunOnceId: "Firewall"

[Code]
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  // Crash-recovery logon entry written by the driver; it points into {app}.
  if CurUninstallStep = usPostUninstall then
    RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', 'InfernoArmAsioPowerRestore');
end;
