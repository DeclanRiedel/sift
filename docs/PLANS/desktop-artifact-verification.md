# Desktop bundle verification and acceptance gates

Status: offline gate implemented and accepted on Linux (2026-11-06); production/native gates remain open.

The current producer is the native Windows x64 development bundle, schema 1.
Add a Linux offline verification tool for that exact format. This is an artifact
integrity gate, not Windows execution, Authenticode, installer or release acceptance.
Keep existing unsigned development packaging usable and keep the server updater's
manifest format, embedded trust and activation contract unchanged.

Require an operator-selected external Ed25519 public PEM key and external raw
64-byte detached signature over the exact bundle.json bytes. Never obtain the
trust key from the bundle. Bound inputs, snapshot them, require canonical Ed25519
SPKI and verify with OpenSSL before interpreting the signed manifest. Reject
ambiguous JSON, unknown schema/target/profile, invalid source identities, malformed
sizes/hashes, duplicate/case-colliding paths and any payload other than the fixed
six executables, source archive, readme and four license/notice files from the
current producer. File-name/hash claims are signed publisher claims; they do not
attest compilation from the named source tree.

Use Linux directory descriptors and no-follow opens for payload directories/files.
Reject symlinks, hardlinks, devices, FIFOs, extra/missing entries and in-place
changes observed while hashing. Stream bounded hashes; never extract source.tar
or execute a payload. Recheck manifest bytes after inspection. A successful receipt
binds manifest/public-key digests, target/source claims, file count and bytes.
Verification is observational: mutable files need revalidation before use. It
neither locks installation inputs nor supplies expiry/antirollback or native
publisher trust. Unsupported verification hosts fail explicitly.

Exercise real OpenSSL Ed25519 verification with disposable private test keys,
then tampered signatures/manifests/payloads, wrong keys/algorithms, duplicate JSON,
path substitution, extra/missing files, symlinks/hardlinks and special files. These
are synthetic payload fixtures, never native executable/install evidence. No new
smoke scripts or CI workflows. Publish a matrix distinguishing historical native
development receipts, current artifact integrity evidence, publisher trust,
prerequisites, install/uninstall/runtime and update lifecycle gates. Native
Windows/macOS acceptance remains deferred; actual signing identities remain an
operator prerequisite. Continue the broader packaging goal after this gate.

## Acceptance evidence

Python 3.13.7 and OpenSSL 3.5.3 pass all seven real-crypto/filesystem groups in
`python3 -B -m unittest discover -s scripts/tests -p 'test_desktop_bundle_verification.py' -v`.
Coverage includes valid CLI receipts, exact-byte and payload tampering, wrong
keys/algorithms, malformed/duplicate metadata, fixed-path inventory, file and
directory symlinks, hardlinks, special files, bounded inputs, changes after a payload read and an attempted
in-bundle OpenSSL executable. No payload executes, and failure output excludes
payload/key contents. The published artifact matrix records the explicit gaps.
Only Python and documentation changed; Rust/application source and the manual-only
CI configuration are unchanged. The preceding Rust milestone passed formatting,
strict default/native Clippy, 1,780 workspace tests (six existing ignores) and
three-engine native suite acceptance under the 10% CPU/RAM cap. No additional
Rust/Cargo workload was launched for this offline tool.

## Linux package-byte signature gate

Extend the existing verifier to the exact `sift-linux-deb-v1` outer receipt
published by `package-linux-deb.py`. The operator selects the `.deb`, matching
`.deb.json` sidecar, external raw Ed25519 signature over that receipt and an
independent external public PEM key. Verify the exact receipt bytes before JSON
interpretation, validate its fixed producer schema/target/source/package claims,
and stream the signed whole-artifact length/SHA-256 through pinned no-follow
file descriptors. Reject aliases, links, special files, changed inputs, duplicate
JSON, unsupported targets and unsafe filenames; unrelated artifacts in the same
output directory need not be rejected.

This authenticates the complete archive bytes, including installer inputs,
without extracting or executing them. It does not inspect Debian control/data
semantics, attest the source build, establish a public signing identity, integrate
APT/OpenPGP trust, or perform installation/runtime/update acceptance. Receipts
must state these limits. Keep the Windows bundle interface/receipts unchanged
and keep trust material/OpenSSL outside the artifact directory. Cover genuine
Ed25519 verification, wrong keys/targets, signature/receipt/artifact tampering,
path/file substitution, observed mutation and sanitized CLI failure using the
existing Python test suite; no new smoke script or CI workflow is needed.

### Linux gate acceptance — 2026-11-09

Implemented package-byte verification without changing the Windows interface.
Python 3.13.7/OpenSSL 3.5.3 pass twelve verifier groups plus seven native Debian
producer groups. They include exact receipt/artifact binding, wrong signatures/
keys/targets, malformed metadata, links/special files, observed mutation, bounded
inputs, untrusted-tool refusal and sanitized CLI output. A producer-built native
Debian archive with synthetic binaries and disposable signing keys passes the
same public verification function. This closes the offline Linux package-byte
signature tooling gap; production identity, archive semantics, native runtime,
release profiles and signed desktop activation remain open. No Rust, dependency
or CI change is needed for this gate.
