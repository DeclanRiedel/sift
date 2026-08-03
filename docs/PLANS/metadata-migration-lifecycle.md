# Metadata migration lifecycle

Status: accepted for implementation (ADR-038).

## Ownership

Opening the metadata database is read/write access to the current schema, not
permission to change that schema. `MetadataStore::open` therefore never runs
migrations. Normal server startup and offline helper commands require the
embedded schema version and checksum history to be current and fail with an
operator-facing command when it is not.

`sift-server migrate status` inspects the database without changing it.
`sift-server migrate apply` is the sole schema writer. It creates an online
SQLite backup before changing a non-empty database, applies the embedded SQL
migrations, and then performs versioned application-data upgrades. A failed
SQL migration remains governed by Refinery's per-migration transaction; the
pre-migration backup is the recovery boundary for the complete operation.
An adjacent OS-locked migration file rejects a second migration process before
either process changes schema; the server itself must still be stopped by the
operator outside the personal launcher lifecycle.

## Deployment policy

The personal, in-process launcher runs `migrate apply --automatic` before it
starts the selected binary. Daemon, team, remote, and container lifecycles do
not migrate implicitly: their operator or orchestrator runs `migrate status`
and `migrate apply` while the server is stopped.

Automatic migration accepts only migrations classified as backward-compatible
with the previous binary. This preserves launcher's candidate rollback: if a
new binary fails its health/protocol handshake after migrating, the old binary
can still start. A future contract migration must be explicitly applied and
must ship with a release procedure that removes binary rollback across that
boundary. V006 is the sole historical pre-release exception. V019 and its
raw-text-to-Loro row upgrade are a contract boundary; automatic initialization
may cross it only for a database with no prior schema or product data.

`PRAGMA user_version` records the minimum migration reader required by a
contract change. A previous binary may accept an unknown forward migration
tail only when that floor is no newer than the latest migration it embeds; it
still validates every name and checksum in the history prefix it knows. This
is what makes rollback after an automatic additive/data migration possible
without weakening the contract-migration gate.

## Classification

Every embedded migration has an explicit code classification:

- `expand`: additive schema that the previous binary can ignore;
- `data`: a data rewrite whose resulting representation remains readable by
  the previous binary;
- `contract`: destructive or representation-breaking and never automatic;
- `legacy-contract`: a documented pre-release boundary, never permitted for a
  new migration.

An embedded migration without a classification is an error and cannot be
applied. Checksum/name divergence or an unknown applied version is likewise a
hard failure rather than an invitation to repair history automatically.

## Backup and restore boundary

Backups are SQLite online-backup snapshots stored beside the database under
`backups/`, named with the source schema version, UTC timestamp, and a random
suffix. They include committed WAL state. Secret bytes remain in the separate
secret backend and are not copied because metadata migration does not mutate
that backend. Restore is deliberately an offline operator action: stop the
server, preserve the failed database for diagnosis, put the selected snapshot
at the configured metadata path, and run `migrate status` before serving.

This design does not yet claim scheduled product backup/restore, retention,
remote object storage, or disaster-recovery orchestration; those remain part
of the broader operational foundation.
