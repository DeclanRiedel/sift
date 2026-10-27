; Unsigned development channel. Stable signing has its own acceptance gate.
#ifndef BundleDirectory
  #error BundleDirectory is required
#endif
#ifndef InstallerOutputDirectory
  #error InstallerOutputDirectory is required
#endif
#ifndef InstallerName
  #error InstallerName is required
#endif
#ifndef AppVersion
  #error AppVersion is required
#endif
#ifndef RepositoryDirectory
  #error RepositoryDirectory is required
#endif

[Setup]
AppId=dev.sift.Sift.Development
AppName=Sift Development
AppVersion={#AppVersion}
DefaultDirName={localappdata}\Programs\Sift Development
DefaultGroupName=Sift Development
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible and not arm64
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.22000
SourceDir={#BundleDirectory}
OutputDir={#InstallerOutputDirectory}
OutputBaseFilename={#InstallerName}
SetupIconFile={#RepositoryDirectory}\crates\desktop\assets\sift-icon.ico
UninstallDisplayIcon={app}\sift-desktop.exe
LicenseFile={#BundleDirectory}\LICENSE
Compression=lzma2
SolidCompression=yes
CloseApplications=yes
RestartApplications=no
SignedUninstaller=no
WizardStyle=modern

[Files]
Source: "sift-desktop.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "sift-server.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "sift-launcher.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "sift-admin.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "sift-remote.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "sift.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "README.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "source.tar"; DestDir: "{app}"; Flags: ignoreversion
Source: "bundle.json"; DestDir: "{app}"; Flags: ignoreversion
Source: "licenses\LICENSE.qlementine-icons"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "licenses\tiberius-APACHE.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "licenses\tiberius-MIT.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion

[Icons]
Name: "{group}\Sift Development"; Filename: "{app}\sift-desktop.exe"; AppUserModelID: "dev.sift.Sift"

[Code]
function InitializeSetup(): Boolean;
begin
  Result := FileExists(ExpandConstant('{sys}\vcruntime140.dll'));
  if not Result then
    SuppressibleMsgBox('Install a compatible Microsoft Visual C++ v14 x64 Redistributable before installing Sift.', mbError, MB_OK, IDOK);
end;
