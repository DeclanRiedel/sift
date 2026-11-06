# Native Linux Debian distribution

Status: native amd64 dev artifact and isolated acceptance verified (2026-11-06); production/signing/graphical gates remain open.

Produce an amd64 Debian package on a native x86_64 GNU/Linux Debian-family host.
Build the six desktop/server/helper binaries from a clean Git checkout with the
locked Cargo graph; reject tracked runtime data, changed source and existing
outputs. Default to the release profile; dev and release-dev are explicitly
Sift Development packages that conflict with the stable package. An explicit
low-memory dev build preserves UI optimization and records the established
codegen-units=256/debug=0 package overrides. Caller resource limits still apply
to all Cargo/compiler descendants; no daemon build bypasses them.

Install all six real binaries together under /usr/lib/sift, with /usr/bin
symlinks, dev.sift.Sift.desktop and the existing PNG in /usr/share/pixmaps.
The runtime resolves its sibling launcher/server from the real executable.
Include licenses/notices, a corresponding git source.tar and a SHA-256 payload
manifest in /usr/share/doc/<package>. Require a configured/explicit maintainer,
record compiler, source commit/tree, host OS, profile, build options and generated
Debian dependencies. File hashes are provenance claims, not source-build attestation.

Require native ELF x86-64/system loader binaries and inspect direct shared-library
dependencies with dpkg-shlibdeps. Fail on unresolved libraries, Nix/private runtime
paths and unresolvable package ownership. Include the known dynamically loaded
Vulkan/Wayland/XKB/XCB/Fontconfig runtime packages; recommend a Vulkan ICD without bundling
host graphics drivers. Record the host-specific dependency floors and release;
this is not a universal Linux binary or clean-machine compatibility declaration.
Validate the desktop entry using desktop-file-validate. Use dpkg-deb --root-owner-group
and no maintainer scripts, triggers, service registration, host PATH changes or
user-runtime writes. Build in an isolated staging directory and publish the .deb,
SHA-256 sidecar and bounded receipt only after source/output rechecks.

Exercise package construction with real ELF/native Debian tools in a synthetic
fixture, plus source/output/path/dependency refusal cases. Then build actual Sift
binaries under the 10% Rust/Cargo cap, package from the committed clean producer,
inspect/extract the package, verify all hashes, sibling placement and desktop
entry, and run CLI helpers with a minimal environment from an empty directory.
Exercise isolated dpkg install/remove if the host supports an unprivileged
alternate root; preserve unrelated and runtime sentinels. Record exact artifact,
source and host identity. No production signer is invented; a package hash is
not publisher trust. Signing, clean-machine graphical/runtime and signed desktop
activation remain independent gates. Windows/macOS acceptance stays deferred.

## Tooling acceptance

Six native-tool fixture tests pass on Ubuntu 25.10 amd64. Real dpkg-deb construction
and extraction verify fixed sibling binaries/symlinks, desktop entry, all content
hashes/modes, restrictive umask handling, source archive exclusion and repeated
package bytes. Source/output/private-RPATH refusals and concurrent receipt
publication preserve existing files. An unprivileged alternate dpkg root installs
and removes the fixture while retaining unrelated and runtime sentinels; both
its administration database and log are private to the temporary root. Dependency
enforcement is bypassed only for that empty test database; this is not clean-machine
prerequisite acceptance. Fixture binary identities are synthetic copies of a system
ELF and do not establish a Sift native artifact receipt.

The producer uses Cargo's emitted executable artifact paths, including fresh/cache
results, rather than assuming an unqualified target directory. Packaging the real
binaries requires the producer to be committed first because its source guard
requires a clean checkout. Actual Sift artifact-specific evidence is recorded below.

## Operator invocation

On a clean native Debian/Ubuntu amd64 checkout with Python 3.9+, Cargo, binutils,
dpkg-dev and desktop-file-utils available, run:

```sh
python3 -B scripts/package-linux-deb.py --output-directory '<outside-checkout-directory>'
```

The default is `release`. For an explicitly development artifact under constrained
local resources, select `--profile dev --low-memory`. `release-dev` is another
explicit development profile; its normal profile settings remain intact. All
Cargo/compiler children inherit the caller's cgroup/affinity controls. The
maintainer defaults to configured Git name/email; `--maintainer` selects another
single-line identity and `--revision` a positive Debian package revision.

Completed output comprises a .deb, .sha256 sidecar and .json completion receipt.
A missing receipt means publication did not complete; existing outputs are never
replaced. The installed `bundle.json` uses `sift-linux-deb-v1`, distinct from the
Windows bundle schema accepted by the offline Ed25519 verifier. It records file
hashes/modes and symlink targets; outer checksums are not publisher signatures.
Actual signing and release-profile evidence remain separate from dev packaging.

The actual desktop ELF audit confirmed Fontconfig is runtime-loaded rather than
listed in DT_NEEDED. The package now explicitly depends on libfontconfig1 (whose
native package carries FreeType/configuration dependencies). A pinned regression
assertion first failed on the omitted dependency, then passed after the producer
fix. The initial actual artifact is superseded by a regenerated package with
this corrected prerequisite; helper/headless checks alone do not prove font or
graphical initialization.

## Real Sift artifact acceptance — 2026-11-06

Ubuntu 25.10 amd64 and Rust 1.96.1 built all six real binaries with `dev` and
explicit low-memory overrides. Native compilation completed in 22m23s. Every
Rust/Cargo descendant retained the shared 160% CPU quota on the 16-logical-CPU
host, one CPU affinity/build job, no swap and the unchanged 3,287,567,155-byte
memory maximum. The native build peaked at 3,265,949,696 bytes. Normal UI
optimization remained enabled; no Rust/application source changed in this slice.

The corrected package is `sift-development_0.1.0~dev.f1bf039d03c7-1_amd64_ubuntu25.10.deb` (144,843,670 bytes).
SHA-256: `a45de61bf356d7bee87f84570a2383bdbef82657ef61309204d4f13b4867ae2f`.
Source commit: `b04abe16c93511ce8170075e874a2810636d7472`.
Source tree: `f1bf039d03c77c4988f80bfc89f8fd16cb073ba7`.

The actual package's fourteen regular payload hashes/modes and six relative
symlinks match; all six binaries remain siblings. The desktop entry validates,
only binary control metadata is present (no maintainer scripts), and the Git
source archive excludes runtime data. Packaged `sift` and `sift-admin` help run
from an empty directory with only /usr/bin:/bin on PATH and isolated XDG paths.
Native launcher/server startup from a path containing spaces and Unicode reaches
`/v1/ready`, negotiates protocol 5, automatically migrates a new metadata database
to schema 57, restarts with the same instance identity and passes SQLite integrity.
This uses an explicit isolated personal-loopback configuration and in-memory
secrets; it is not default graphical startup or credential-backend persistence
acceptance. All test-owned processes stop afterward.

Unprivileged alternate-root dpkg install/remove verifies the installed payload
and preserves unrelated program-directory and external runtime sentinels. The
package administration database and log are temporary. Dependency enforcement is
bypassed solely for that empty alternate database; host runtime libraries are
already present. This does not establish missing-prerequisite/clean-machine
acceptance. Native existing-output and dirty-source refusals preserve the actual
artifact SHA-256. The initial pre-Fontconfig candidate is superseded by the hash
above. The corrected dependency declaration includes libfontconfig1 alongside
native ELF-derived floors and the other runtime-loaded libraries.

All thirteen packaging/verifier tests pass (seven signature/filesystem groups and
six Debian/native-tool groups). Release/release-dev artifact builds, publisher
signing, clean-machine/graphical startup, full desktop update/rollback and other
architectures/distributions remain separate gates. Windows/macOS acceptance
remains deferred. No CI workflow was added or enabled.

## Isolated graphical acceptance — 2026-11-06

The exact corrected dev artifact above passes a limited packaged GUI check on
this host. Its SHA-256 was verified again before extraction. The packaged sibling
launcher/server supplies a temporary personal loopback endpoint with in-memory
secrets; the desktop receives that explicit endpoint via `--server-url` and the
name Package acceptance. STATE, CONFIG, DATA and CACHE XDG directories are all
private to the test. No saved user connection, clipboard content or credentials
are required.

A private Xvfb 21.1.18 server (Ubuntu package 2:21.1.18-1ubuntu1.1, downloaded and
extracted without installing into the host package database) supplies X11.
`VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json` selects the installed
lavapipe ICD from mesa-vulkan-drivers 25.2.8-0ubuntu0.25.10.2. Fontconfig is
libfontconfig1 2.15.0-2.3ubuntu1. The private display has no window manager;
explicit focus and resize of the PID-matched Sift window triggers visible frames.
The application class/title are dev.sift.Sift / Sift · Linux.

Observed checks:

- The packaged workspace, fonts and icons render with Vim NORMAL mode visible.
- `:` opens the command palette; typing Toggle Light/Dark Theme filters to that
  command, and Enter changes the workspace visibly from dark to light.
- The isolated presentation file records `dark_theme=false` and window bounds.
- After stopping and restarting the desktop, server and private display with the
  same isolated state, the workspace visibly restores its light theme.

The local artifact directory contains `native-graphical-acceptance.json` and
`graphical-focused.png`, `graphical-palette.png`, `graphical-light.png` and
`graphical-restart.png`; the receipt records their SHA-256 hashes and the tested
artifact hash. These images contain only the private test display. They are
local observations, not signed release attestations.

The normal host X11 display rejects authorization, including the system xwininfo
client. Native Wayland maps the packaged window with the expected app identity,
but the host session is locked; no unlock was attempted and rendered interaction
was not accepted there. Nested Xwayland also lacks observable frames in that
session. Xvfb with the default ICD reports missing DRI3 presentation support;
only explicit software Vulkan plus focus/resize supplies the accepted frames.
These facts do not establish an application rendering defect or native compositor
acceptance. Accelerated rendering, default graphical startup, Wayland interaction,
IME, keyboard layouts, clipboard, scaling and window-manager behavior remain open.

All GUI/server/display descendants retain the existing shared 160% CPU quota on
16 logical CPUs, single-core affinity, 3,287,567,155-byte memory maximum and no
swap. The successful initial software check peaks at 1,004,744,704 bytes; the
restart peaks at 142,835,712 bytes. An earlier attempt hits the shared memory cap
because previous extracted payloads on tmpfs remain charged to the slice; removing
only those stopped test payloads restores headroom, with no cap increase. Every
test-owned process is stopped afterward and temporary extracted binaries are
removed. No Rust/application source, package payload or CI workflow changes.
