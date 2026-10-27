# Native Windows distribution milestones

Design started 2026-10-27. The user supplied a Windows test host and deferred
macOS. Linux builds remain on the selected Nix host. Addresses, passwords and
signing keys stay outside source snapshots and committed configuration.

1. Pin the repository Rust release to the version selected by the current Nix
   lock, 1.96.1. Rustup and Nix must resolve the same compiler/components before
   platform comparisons. Retain explicit compiler versions with native results.
2. Build the desktop and bundled server/launcher/administration executables on
   Windows using MSVC. Keep source and target caches separate, and generate
   only host-local development environment/key files.
3. Produce a versioned desktop bundle and per-user installer with source/build
   identity, file hashes, product metadata and licenses. Keep user runtime data
   outside installed program files and preserve it across uninstall.
4. Bind signing to an operator-selected certificate and timestamp endpoint.
   Verify the exact signer, trust result and timestamp for binaries, installer
   and uninstaller. Unsigned development artifacts retain explicit provenance.
5. Run native install/uninstall and bundled-helper acceptance in an isolated
   destination, checking hashes and preservation of unrelated user data. Record
   real artifact evidence before graduating the distribution checklist.

Native Windows agent isolation requires its own authority and IPC design plus
Windows acceptance; build-tool installation alone does not establish isolation.

## Executable version metadata design

Generate the Windows resource input under Cargo OUT_DIR from a checked-in
template. Derive the numeric version from Cargo's major/minor/patch components
plus a zero build component, requiring each field to fit a Windows version word.
Retain the complete Cargo version in FileVersion/ProductVersion strings and keep
the existing application icon. Resource generation must preserve the source tree
and accept Unicode build paths through an explicit UTF-8 code page.

Native acceptance reads FileVersionInfo from the compiled executable and checks
its product name, full version, numeric components and original filename. These
checks establish executable metadata; installer, signing and isolation milestones
retain their own acceptance requirements.

Reference: https://learn.microsoft.com/en-us/windows/win32/menurc/versioninfo-resource

## Native build evidence — 2026-10-27

Windows 11 build 26200, MSVC Build Tools 17.14.42 and Rust 1.96.1 passed
workspace formatting, strict all-target Clippy, the workspace build and all
enabled workspace tests: 1,607 passed, zero failed, two ignored. Unix-only SQLite
file fixtures remain Unix-only; Windows explicitly tests the unsupported
maintenance boundary before file access. The desktop offline probe test allows
the production ten-second deadline plus scheduling margin.
The development source tree was
`da7d9993f796928a2726c8ad40a7756e4262f415`, including the concurrent PostgreSQL
partition attachment milestone.

The built desktop reports product Sift, version 0.1.0 and original filename
sift-desktop.exe, with numeric version 0.1.0.0 and icon resource 1 present.
Its SHA-256 was
`575a850671845053d6a3433d1efda9b633134675e7b1ed930a2c4783d726cb18`.
This is a development build receipt, not a signed release artifact. Both the
current-user and local-machine personal certificate stores contained no code
signing certificate. Installer, signing and native agent isolation remain open;
macOS is deferred by the user.

## Portable bundle workflow

From a clean native Windows checkout, run:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\package-windows.ps1 -OutputDirectory "$env:LOCALAPPDATA\SiftBuild\artifacts"
```

The default is a release build. `-BuildProfile dev` or `release-dev` selects an
explicit development profile. Packaging builds the desktop and all five server
helpers for x64 MSVC, verifies desktop product/version metadata, refuses source
changes during compilation and refuses existing output. The ZIP contains the
six sibling executables, project/asset/vendored notices, a clean corresponding
source archive and a manifest recording compiler, profile, source identity,
file sizes, SHA-256 hashes and Authenticode status. The ZIP receives its own
SHA-256 sidecar; incomplete compression never creates the final ZIP filename.
Development environment files and user runtime data are excluded by the source
checkout's ignore rules and the explicit executable/file list.

The portable bundle requires a compatible x64 Visual C++ v14 Redistributable
and Windows ICU/Direct3D system libraries. It does not install prerequisites or
register an uninstaller. Native dependency inspection found VCRUNTIME140.dll
in the desktop import table. See Microsoft's
[runtime deployment guidance](https://learn.microsoft.com/en-us/cpp/windows/determining-which-dlls-to-redistribute?view=msvc-170).
Installer, signing and clean-machine prerequisite acceptance remain separate
milestones.

Native portable acceptance passed on 2026-10-27 for the `dev` profile. The
86,916,373-byte ZIP extracted successfully; all 12 payload sizes/hashes matched,
all six executables were present, and packaged `sift`/`sift-admin` help commands
ran from an empty working directory with only Windows system directories on
PATH. The corresponding source archive contained no tracked runtime data.
Dirty-source and existing-output refusals passed without changing the artifact.
All six executable signature statuses were `NotSigned`.

The Windows-native source mirror tree was
`f76c0b89081e21cf14d1669c18ccc69abd4f13a6`; its Git archive accompanies the
binaries. The ZIP SHA-256 was
`01c4b045c16dc5bc5b49c52c652af61f3f5e3e3b145e0d86d9d8c4730720e8fd`.
This proves the development bundle workflow, not a release-profile build,
signed installer, clean-machine installation or graphical desktop acceptance.
