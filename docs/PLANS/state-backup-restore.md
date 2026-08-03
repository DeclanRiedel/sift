# Sift state backup and restore

Status: accepted for implementation (ADR-039).

## Recovery boundary

The v1 archive protects state owned by Sift, not data owned by connected
Postgres or SQL Server instances. It contains a consistent metadata SQLite
snapshot, a versioned manifest, and—when the file secret backend is active—a
portable encrypted-secret payload. It excludes logs, caches, runtime locks,
daemon descriptors, downloaded releases, and reacquirable extension package
bytes.

The destination installation owns its runtime identity. The source instance id
is diagnostic manifest data only. Restore preserves tenants, principals,
rooms, documents, policies, history, extension state, connection profiles, and
connection/password credentials, but revokes every bearer/session credential
and one-use authentication artifact. Authentication MAC keys are removed and
regenerated on next start. An in-place restore therefore keeps the destination
instance id; a new installation keeps its newly-created id.

## Archive and secret handling

The archive format is a ZIP container with a schema-versioned JSON manifest
and fixed entry names. Every entry uses authenticated AES-256 ZIP encryption.
The encryption password is read from an explicit private key file and is never
accepted as a command-line value, environment value, manifest field, or log
field. Output is created privately under a random partial name, fsynced, and
atomically renamed; existing output is never overwritten.

The manifest records Sift version, metadata schema/current compatibility floor,
creation time, source instance id when available, secret-backend disposition,
uncompressed sizes, and SHA-256 for every payload. Restore permits only the
fixed v1 entries, enforces size ceilings before extraction, verifies hashes,
checks SQLite integrity and migration compatibility, and rejects unknown
format versions.

For the file backend, `secrets.enc` and its source encryption key are entries
inside the encrypted archive. Restore decrypts that store only in a private
staging directory and re-encrypts its entries with the destination's configured
secret key; the destination key is never replaced. Memory secrets are
non-durable and are not represented. OS-keychain bytes are non-exportable: the
manifest records that external secrets are required, and apply requires an
explicit acknowledgement while retaining the destination keychain.

## Consistency and ownership

Every serving process holds a shared maintenance lock for its lifetime,
regardless of runtime mode. Backup creation, restore, and metadata migration
take the exclusive side and fail immediately when a server or another
maintenance process is active. This makes the SQLite snapshot and secret
payload one stopped-server consistency unit. The existing metadata migration
lock remains the narrower schema-writer serialization guard.

Personal launch still owns automatic compatible migration before serving.
Daemon, team, remote, and container operators run lifecycle commands while the
service is stopped. Remote startup explicitly migrates before acquiring the
serving lock.

## Restore transaction

`backup restore` is validation-only unless `--apply` is supplied. Apply:

1. obtains the exclusive maintenance lock;
2. validates/decrypts into a private same-filesystem staging directory;
3. creates a full encrypted rescue archive of current destination state;
4. sanitizes restored authentication state and records the restore audit row;
5. installs staged secret state and metadata with a durable restore journal;
6. removes the journal only after files and parent directories are synced;
7. reports the rescue archive path.

The database is renamed last, so readers never see restored metadata paired
with the old secret store. A journal preserves the old file paths until commit;
the next restore invocation rolls an interrupted replacement back before doing
new work. Restore never auto-migrates the archive: it must already be readable
by the current binary, after which the ordinary explicit migration lifecycle
applies.

## Surface and audit

The operator surface is:

- `sift-server backup create --output <path> --key-file <path>`;
- `sift-server backup inspect --archive <path> --key-file <path>`;
- `sift-server backup restore --archive <path> --key-file <path>` (dry run);
- the same restore command with `--apply` and, for keychain archives,
  `--allow-external-secrets`.

Create and restore are represented by operation kinds and durable audit rows;
manifests contain no actor secrets or raw credentials. CLI output is structured
JSON suitable for automation. V1 writes only to an explicit local destination:
scheduling, retention deletion, cloud/object-store upload, tenant-selective
restore, and backing up connected databases are deliberately separate work.

## Required tests

Implemented hardening coverage (2026-08-03): file and memory round trips;
keychain external-secret acknowledgement; wrong keys; authenticated-payload
tampering; duplicate, unknown, traversal-shaped, unencrypted, oversized, and
future-version archive rejection; serving-process exclusion; live-WAL capture;
destination instance/key preservation; API-token revocation; dry-run
non-mutation; rescue archive creation; recovery/finalization across every
journal phase; failed-staging cleanup; Unix file/key permissions; and the full
bearer/refresh/API-token and one-use authentication sanitization matrix,
including removal of portable out-of-band OAuth verifier secrets while durable
identity credentials remain readable and destination keychain entries remain
external.

The remaining graduation work is the older/newer schema compatibility-floor
CI matrix and CLI/remote lifecycle redaction and manifest-schema fixtures.

- archive round trip for file and memory backends;
- wrong key, tampering, duplicate/unknown entry, traversal, and size-limit
  rejection;
- running-server and concurrent-maintenance exclusion in every runtime mode;
- SQLite WAL consistency and integrity validation;
- destination key preservation with restored credential readability;
- session/API-token/one-use artifact revocation and auth-key rotation;
- dry run makes no destination changes;
- failed and interrupted install restores the rescue state;
- older/newer schema compatibility-floor matrix;
- permissions, redaction, deterministic manifest schema, and remote lifecycle.
