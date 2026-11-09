# Desktop artifact and installer validation

This matrix separates implemented tooling from artifact-specific native evidence.
The current session exposes Linux only; current Windows/macOS acceptance is
explicitly deferred. Historical Windows receipts apply to the source trees and
artifacts recorded in the [native distribution plan](PLANS/windows-native-distribution.md),
not to the current checkout. This is not a signed desktop release declaration.

| Gate | Linux x86-64 | Windows x64 | macOS |
| --- | --- | --- | --- |
| Native desktop distribution producer | Native amd64 Debian/Ubuntu .deb producer; dev artifact accepted on Ubuntu 25.10 (ADR-170) | Portable ZIP and per-user development installer implemented | No completed native app/distribution producer |
| Recorded native artifact evidence | Unsigned dev .deb receipt, 2026-11-06; source/hash below | Unsigned dev ZIP and Inno install/uninstall receipts, 2026-10-27 | None; deferred |
| Signed manifest and exact payload verification | Real Linux payload/link hashes accepted; external Ed25519 signatures over producer receipts now authenticate complete `.deb` bytes; Windows schema-1 payload verification is also supported | Current producer hashes payloads and records Authenticode claims; detached manifest verification is available on Linux, with no production signature receipt | No supported bundle format in this verifier |
| Publisher identity and native signature trust | Not established; the Linux dev .deb is unsigned | Recorded EXEs/installer/uninstaller were NotSigned; no production signer/timestamp trust receipt | No signing/notarization identity or native receipt |
| Prerequisites and clean-machine runtime | Packaged CLI and configured sibling startup/restart plus isolated install/remove pass on the existing host; clean-machine prerequisites remain open | Existing-host helper/startup and isolated install/uninstall passed historically; missing-prerequisite and clean-machine acceptance remain | Deferred |
| Native graphical interaction | Packaged workspace rendering, Vim palette and light-theme restart pass on isolated Xvfb/lavapipe; native accelerated/Wayland, IME, clipboard and scaling remain open | Packaged graphical acceptance remains open | Deferred |
| Signed desktop update and rollback | Desktop component packaging/activation acceptance missing | Desktop component packaging/activation acceptance missing | Deferred |

Linux release/release-dev artifact builds, ARM64 packaging and native acceptance
also remain open. The amd64 dev package does not establish universal Linux support. Server updater
acceptance is tracked separately in [Remote and updates](REMOTE-AND-UPDATES.md):
its fixed server artifact format, embedded trust keys, sequence/expiry checks,
readiness/handshake activation and rollback are not replaced by desktop bundle
inspection. Git commit signatures do not establish artifact publisher trust.

The historical Windows ZIP SHA-256 is
`01c4b045c16dc5bc5b49c52c652af61f3f5e3e3b145e0d86d9d8c4730720e8fd`;
the installer SHA-256 is
`7dacb9c680f06d98c97fe9665cec3098d7fe0330e4434afeecff147eda5ad007`.
These are recorded receipts, not files revalidated on this Linux host. Their
source identity, native compiler/tool versions and exact exclusions remain in
the linked native plan.

## Native Linux development artifact

The [Debian producer and evidence](PLANS/linux-debian-distribution.md) records
`sift-development_0.1.0~dev.f1bf039d03c7-1_amd64_ubuntu25.10.deb` (144,843,670 bytes),
SHA-256 `a45de61bf356d7bee87f84570a2383bdbef82657ef61309204d4f13b4867ae2f`, from source commit
`b04abe16c93511ce8170075e874a2810636d7472` / tree
`f1bf039d03c77c4988f80bfc89f8fd16cb073ba7`.

The actual artifact passes fourteen payload hashes/modes, six sibling links,
desktop-entry validation, CLI relocation, packaged personal-loopback launcher/
server readiness and protocol 5, schema-57 initialization/integrity and restart.
Alternate-root install/remove preserves unrelated/runtime data; dependencies
are bypassed only in that empty temporary package database. These tests use
existing-host prerequisites and an explicit in-memory-secret configuration,
not clean-machine, default graphical or keychain/file-secret persistence proof.
Existing-output and dirty-source refusal leave the artifact unchanged. The native
build used the 10% Rust/Cargo CPU/RAM cap. This is an unsigned dev artifact;
production profiles, publisher trust and signed desktop activation remain open.

The same artifact also renders its workspace, text and icons on a private Xvfb
server using the host's explicit lavapipe Vulkan ICD. Vim `:` opens the command
palette; filtering and confirming Toggle Light/Dark Theme changes the visible
workspace, persists `dark_theme=false` under isolated XDG state, and restores
light appearance after a full desktop/server/display restart. This is a scoped
software-rendering check, not accelerated/native compositor acceptance. Native
Wayland creates a mapped window, but the locked session prevents visual and
interaction acceptance; native X11 authorization is unavailable. IME, clipboard,
scaling and default desktop startup remain open. Details and limitations are in
the [Linux evidence](PLANS/linux-debian-distribution.md#isolated-graphical-acceptance--2026-11-06).

The installed `sift-linux-deb-v1` manifest differs from the Windows bundle format
accepted by the verifier below. Its SHA-256 sidecars are integrity receipts,
not a cryptographic publisher signature.

## Offline manifest/payload gate

Run `scripts/verify-desktop-bundle.py` on Linux with Python 3.9+ and OpenSSL with
Ed25519 support. The Windows bundle input is the schema-1 Windows x64 directory
produced by `scripts/package-windows.ps1`: six sibling EXEs, corresponding source
archive, README, project license and three asset/vendor license files.

The operator supplies an independent external Ed25519 public PEM key and an
external raw 64-byte signature over the exact `bundle.json` bytes. A selected
signing operator can create that signature with:

```sh
openssl pkeyutl -sign -rawin -inkey '<operator-private-key.pem>' \
  -in '<bundle-directory>/bundle.json' -out '<external-manifest.sig.raw>'
```

Keep the private key, public trust-key input and detached signature outside the
bundle. Never derive verification trust from a key shipped inside an artifact.
This raw desktop signature is distinct from the server release manifest's
base64url signature encoding. Signing does not change the bundle manifest or
assert native Authenticode/notarization acceptance.

```sh
python3 -B scripts/verify-desktop-bundle.py \
  --bundle '<bundle-directory>' \
  --signature '<external-manifest.sig.raw>' \
  --public-key '<operator-public-key.pem>' \
  --target x86_64-pc-windows-msvc
```

`--openssl` can select an operator-trusted OpenSSL executable outside the bundle.
The verifier snapshots bounded manifest/signature/key inputs, requires Ed25519,
checks the exact signature before interpreting JSON, then validates the fixed
inventory and streams all signed lengths/hashes. Directory-descriptor/no-follow
inspection rejects extra/missing entries, aliases, symlinks, hardlinks, devices,
FIFOs and path substitution. It never extracts `source.tar` or runs a payload.

Successful stdout is one JSON receipt with manifest and public-SPKI digests,
source/target/profile claims, file count and bytes. `native_acceptance` and
`source_build_attestation` are explicitly `not_performed`: Authenticode fields
and source identities are signed publisher claims. Errors return nonzero with
no success receipt and no payload/key contents or OpenSSL diagnostics.

Inspection observes mutable files at that time; it does not lock future install
inputs. Reverify exact staging inputs before installation. This gate does not
provide expiry/antirollback, certificate/timestamp trust, binary-format/runtime
validation or a desktop updater. Existing unsigned development packaging remains
available; production acceptance requires all applicable gates above.

## Linux package-byte signature gate

The same verifier accepts the native producer's `sift-linux-deb-v1` outer
`.deb.json` receipt and its matching `.deb`. Keep the signature, selected public
key and trusted OpenSSL executable outside the artifact directory. Sign the exact
receipt bytes with the operator's Ed25519 key, then verify:

```sh
python3 -B scripts/verify-desktop-bundle.py \
  --artifact '<output-directory>/<artifact.deb>' \
  --receipt '<output-directory>/<artifact.deb.json>' \
  --signature '<external-receipt.sig.raw>' \
  --public-key '<operator-public-key.pem>' \
  --target x86_64-unknown-linux-gnu
```

The receipt filename must match the artifact name plus `.json`. Verification
checks the exact signature before parsing the bounded producer schema, pins the
artifact directory, refuses links/special files, streams the signed length/hash,
and checks observed file/path/receipt changes. Other builds in that output
directory are allowed. The complete archive bytes are authenticated, including
installer inputs; the verifier never extracts or executes them.

Successful output declares `scope=signed_artifact_bytes` and separately reports
`archive_format_validation`, `native_acceptance` and `source_build_attestation`
as `not_performed`. ELF/dependency/link metadata remains a signed producer claim;
Debian control/data semantics are not inspected. This is not APT/OpenPGP trust,
a real publisher-identity receipt, or installation/runtime/update acceptance.
The unsigned native development artifact evidence above remains unchanged.

On 2026-11-09, Python 3.13.7/OpenSSL 3.5.3 passed twelve verifier groups and
seven Debian-producer groups, including an actual native `.deb` fixture with an
externally signed receipt. Fixture binaries/identities and keys are disposable;
this is not a signed Sift release. Run:

```sh
python3 -B -m unittest discover -s scripts/tests -p 'test_desktop_bundle_verification.py' -v
python3 -B -m unittest discover -s scripts/tests -p 'test_linux_deb_packaging.py' -v
```

## Current verifier evidence

On 2026-11-06, Python 3.13.7 and OpenSSL 3.5.3 on Linux passed all seven acceptance
groups:

```sh
python3 -B -m unittest discover -s scripts/tests \
  -p 'test_desktop_bundle_verification.py' -v
```

They cover real Ed25519 signing/verification, wrong keys and algorithms, exact-byte
manifest changes, malformed signatures, duplicate JSON, schema/source/size/hash
errors, path substitution, extra/missing payloads, file/directory symlinks,
hardlinks, FIFO refusal, observed post-read changes, oversized input and attempted execution of an OpenSSL
replacement inside the bundle. CLI receipts and sanitized failure output are
checked. Keys are disposable temporary test keys and payloads are synthetic,
non-executable bytes; these results do not close any native artifact, publisher
identity, installer or runtime gate. No release workflow was added or enabled.

See [design and acceptance](PLANS/desktop-artifact-verification.md), ADR-169,
and the broader [desktop plan](PLANS/gpui-desktop.md).
