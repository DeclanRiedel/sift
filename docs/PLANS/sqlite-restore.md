# SQLite restore design

Restore the connected ordinary main database from an existing SQLite backup in
the same managed root. This is an explicitly destructive replacement of its
contents, not a filesystem rename. Require a managed writable connection, root
and tenant authority, no current transaction, a fresh one-use preview token,
explicit write confirmation and a verified pre-restore backup acknowledgment.
The Vim review names root/target and backup, warns about replacing all contents,
and requires the exact phrase `RESTORE root/target FROM backup`.

Use the existing audited ManageSqliteDatabase operation and locked native
extension signatures with an additive Restore action. Validate both root paths
against symlinks, hardlinks and protected paths; reject self/same-inode input,
non-SQLite/corrupt backups, main/WAL inputs above the 1 GiB preflight bound, and stale
target/archive identities. Preview does not replace database contents. Revalidate in the worker
just before apply, open the backup read-only with trusted schema disabled, pin
a read snapshot, then recheck the reviewed source identity. Use SQLite's
backup API into the existing main file through a private destination handle:
the destination transaction commits atomically only at completion, and
unfinished copies roll back on
backup-handle drop. No driver signature changes or filesystem replacement.
Managed interrupts only touch the normal connection, never the private backup
destination handle.

Validate the source with bounded quick_check under cancellation/closing/deadline
progress guards. Copy at most 128 pages per step; bound busy retries, total
bytes and elapsed time, retaining the worker permit until work ends. Source
reads and the restore copy share one snapshot. Native locking/compatibility
errors stop the operation; no extra journal-mode or page-size conversion
PRAGMAs are issued.
Preview holds no lock; apply rereads its precondition before native work.
SQLite bypasses the shared schema cache; preserve fresh introspection after
restore, including on other handles, and verify that observed catalog revisions
change when schema changes. A timeout is not proof that no commit occurred.
Never persist backup contents, absolute paths, or confirmation tokens in
metadata/audit.

Real-file HTTP acceptance must prove preview without mutation, replacement of
old schema/data, WAL-aware restore, integrity, explicit confirmation and backup
acknowledgment, stale target/archive and read-only/transaction/path/protected
refusal, one-use replay refusal and cache refresh. Driver acceptance must prove
mid-copy cancellation rolls back and preserves the open destination connection.
Vim tests prove the typed source/action review, acknowledgment and exact phrase.
Windows/macOS acceptance remains deferred. Deletion, cross-root restore and
external disaster recovery remain separate work.

Reference: https://sqlite.org/c3ref/backup_finish.html
## Acceptance

Linux real-file HTTP/SDK acceptance passes in the existing maintenance fixture:
`cargo test --workspace --test sqlite_maintenance`. It covers WAL-backed restore,
rowids and mixed storage classes, preserved backup contents, restored foreign
keys/indexes/views/triggers, cross-handle schema refresh, and stale/replay,
confirmation, policy, transaction, path, replaced-file and corruption refusals.

The native driver test cancels after the first 128 pages into a WAL destination,
proves intact original data and integrity, rejects an incompatible page size,
and then restores successfully through the same connection. Vim acceptance
proves preview-only dispatch, backup acknowledgment, exact target/backup phrase,
confirmed apply and schema refresh. Full workspace tests, formatting and strict
workspace Clippy pass. Windows/macOS evidence remains deferred.
