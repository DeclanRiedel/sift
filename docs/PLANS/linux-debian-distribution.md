# Native Linux Debian distribution

Status: producer implemented and tooling accepted (2026-11-06); actual Sift artifact acceptance in progress.

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
Vulkan/Wayland/XKB/XCB runtime packages; recommend a Vulkan ICD without bundling
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
requires a clean checkout. A capped native Sift build is running; artifact-specific
hashes, extraction/helper and install/remove results will be recorded afterward.

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
