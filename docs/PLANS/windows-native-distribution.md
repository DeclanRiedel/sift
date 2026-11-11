# Native Windows distribution milestones

Work tracking (2026-11-11): **Partial / active**. Remaining product work: [current TODO](../TODO.md#platform-distribution-and-native-acceptance).

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

## Per-user installer design

Compile an Inno Setup installer from the completed immutable bundle. Validate
its manifest and every payload hash before compilation. Use a per-user program
directory, current-user shortcuts and uninstall registration with no elevation,
services, PATH changes or runtime-data deletion rules. Development builds use a
separate installer identity/program directory. Preserve files the installer did not
own, including unrelated files inside its program directory.

The installer requires Windows 11 x64 and checks for the Visual C++ runtime
before modifying files. Prerequisite downloading/installing is a separate
operator step; no ad-hoc DLL copies or unverified downloads. Native acceptance
installs into an isolated destination, checks installed hashes and both CLI
helpers, then uninstalls and verifies preservation of an unrelated sentinel.
Signing is a separate acceptance gate until an operator provides an identity
and certificate/service. Do not count an unsigned development installer as a
signed release or a clean-machine/prerequisite matrix.

References: [Inno privileges](https://jrsoftware.org/ishelp/topic_setup_privilegesrequired.htm),
[signing](https://jrsoftware.org/ishelp/topic_setup_signtool.htm).

## Development installer workflow and acceptance

Install Inno Setup 6 on the Windows build host, then run:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\package-windows-installer.ps1 -BundleDirectory "<bundle-folder>" -OutputDirectory "$env:LOCALAPPDATA\SiftBuild\installers"
```

`-CompilerPath` selects a custom ISCC location. The wrapper accepts the fixed
bundle payload, rejects escaping/duplicate paths, extra files, reparse points,
invalid metadata and changed hashes, and compiles a verified staging copy.
Completed output has SHA-256 and build-receipt sidecars; existing artifacts or
receipts are preserved. This workflow installs **Sift Development**. Runtime
locations remain desktop-selected; `--instance-root` selects a separate
development instance.

Inno Setup 6.7.3 compiled the installer natively on 2026-10-27. Native acceptance
installed into a path containing spaces and Unicode, verified all 12 payloads,
ran both CLI help commands from an empty directory, checked current-user-only
uninstall registration and shortcut creation, and uninstalled successfully.
An unrelated file inside the program directory and a runtime sentinel outside
it retained their hashes; user/machine PATH values were unchanged. Tampered
payload, escaping path, existing installer and existing receipt refusals passed.
The final wrapper rebuilt the same installer bytes as the installed artifact.

Installer SHA-256:
`7dacb9c680f06d98c97fe9665cec3098d7fe0330e4434afeecff147eda5ad007`.
Installer and uninstaller both reported `NotSigned`. This establishes the
development install/uninstall workflow on the supplied host, with its existing
runtime prerequisites. Signed stable releases, missing-prerequisite and clean
machine matrices, and graphical desktop acceptance remain open.

## Fresh personal desktop startup design

Native acceptance found that a fresh bundled launcher exits with metadata V0
while the binary requires V54. The desktop requests daemon mode for singleton
ownership, but launcher automatic migration currently admits only personal
in-process mode. Development setup had initialized metadata before this path.

The bundled personal desktop will pass an explicit launcher-only lifecycle flag.
Consume that flag before forwarding server arguments. Permit automatic metadata
setup for this path only with personal deployment, daemon mode, loopback
transport and a loopback bind. Retain daemon/runtime ownership and the existing
exclusive maintenance lock, backups and migration safety classification. Team,
container and ordinary daemon lifecycles retain explicit migration; the existing
personal in-process policy is preserved.
Configured instance roots keep their existing applied-instance workflow.
Skip migration when metadata is disabled.

Acceptance must launch the actual bundled executables against a new isolated
metadata/runtime directory, reach readiness/handshake with a current schema,
and exercise restart and the unchanged ordinary-daemon refusal. The existing
workspace gates and native Windows tests remain required before committing.

Native acceptance passed on 2026-10-27: fresh desktop-owned daemon startup
initialized metadata to V54, returned readiness and negotiated protocol 5.
Restart retained instance identity and changed daemon generation. An ordinary
daemon still refused implicit migration on a new database; metadata-disabled
desktop startup became ready without creating one.

Activation now uses the SDK's typed readiness endpoint and a fresh handshake
under the existing absolute deadline. Its regression test serves the real
production router, replacing a mock that repeated the stale `/ready` path.
The test runs on Windows as well as Unix. Native Windows formatting, strict
workspace Clippy, workspace build, executable metadata/icon checks and all
enabled workspace tests passed: 1,610 passed, zero failed, two ignored. Linux
acceptance remains separate; no graphical desktop acceptance is claimed here.

## Refreshed development artifacts

The first-launch fix is included in native source mirror
`2f180717b9861820c071f3083a3e64a6845d3caa`. The refreshed bundle and installer
repeated extraction/hash, CLI and install/uninstall acceptance. The actual
packaged executables passed fresh V54 metadata setup, protocol 5 readiness,
restart identity/generation, ordinary-daemon refusal and metadata-disabled
startup acceptance. Both artifacts remain unsigned development builds.

- ZIP SHA-256: `e4364926f8ce9b499f73314a00baff27a215a095f0a19b64425ca3c7f366ba1b`
- Installer SHA-256: `a0b993cea6e7139c261386221737bf0cbd14563425bab2e764f0fd0e5c99d923`

## Launcher descendant ownership

Native acceptance on 2026-10-28 reproduced an owned server surviving forced
launcher termination. The Windows launcher now establishes an unnamed,
non-inheritable kill-on-close job before spawning helpers or the server.
Actual-binary acceptance and the permanent lifecycle regression verify server
termination, closed readiness and preserved normal error exit. Formatting,
strict workspace Clippy, workspace build, PE resources and all tests passed:
1,611 passed, zero failed, two ignored. See
[design and evidence](windows-launcher-lifetime.md). The artifact hashes above
predate this fix; fresh development packaging remains required. Native agent
filesystem/network isolation remains separate work.
