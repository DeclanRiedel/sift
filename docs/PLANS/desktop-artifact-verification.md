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
