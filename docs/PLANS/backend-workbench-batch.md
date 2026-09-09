# Backend workbench batch

Status: in progress. User approved all ten features, with milestone commits.

Milestone 1 complete: CSV quarantine reports and downloads, SQLite savepoint
recovery, redacted row errors. Formatting, strict workspace Clippy and full
workspace tests passed (2026-09-09). Durable resume remains in development.

## Order and acceptance

1. Transfer quarantine: authorized report download containing source row numbers,
   column names, rejected values and bounded reasons; preserve expiry and audit.
2. Durable transfer resume: immutable source/target identity, durable checkpoints,
   safe replay and explicit handling of the target-commit/checkpoint crash gap.
3. Long-running-query alerts: configurable thresholds, deduplication and audited
   access; no SQL or credentials in external telemetry.
4. Idle-in-transaction alerts: transaction-age detection, separate from query age.
5. Query-performance history: bounded retention and scoped reads of timings and
   outcomes; do not persist query parameter values.
6. PostgreSQL maintenance: typed VACUUM, ANALYZE and REINDEX requests, quoted
   targets, authorization, audit and supervised execution.
7. Integrity checks: provider-specific supported checks and structured reports.
8. PostgreSQL restore validation: read-only destination checks before explicit
   apply; preserve the offline Sift-state recovery boundary.
9. SQL Server recovery: explicit server-side archive paths, bounded backup and
   restore, preview by default, no implicit replacement of existing databases.
10. Tenant-selective Sift restore: offline validated merge, destination-owned
    identity, secret remapping, rescue backup and recoverable apply.

Each milestone includes focused behavior tests, documentation and a commit.
Workspace formatting, strict Clippy and tests are required before graduation.
Backend/API/operator workflows are in scope; new desktop screens are not.

## Design constraints

- Preserve the locked Driver trait; use existing execution and engine extensions.
- Every action goes through authorization and audit. Query execution remains
  supervised with timeouts and cancellation.
- Resume must never label an arbitrary caller-supplied row offset as a verified
  checkpoint. Metadata and target-database commits are separate durability domains;
  replay requires a target-side idempotency mechanism or reconciliation.
- Recovery validates only explicitly selected targets. Tests use disposable data;
  implementation work does not authorize restoring or maintaining user databases.
- Quarantine contains database contents: workspace access controls, expiry and
  size bounds apply. Credentials and raw driver errors must not enter reports.

## Durable resume design

The recipe opts into an explicitly named target-side checkpoint table and UUID
run identity. Each 100-row chunk and its next-row checkpoint commit in the same
target transaction. Source bytes, parsing options, target and recipe/actor
identity are fingerprinted; replay refuses mismatches. V1 requires existing
targets and abort-on-error semantics, not quarantine or create-table imports.
The caller reuploads the original source after a restart. Checkpoint tables are
not automatically deleted. Losing or manually altering them invalidates replay
guarantees; external trigger side effects are outside database atomicity.

Locking follows [PostgreSQL row locking](https://www.postgresql.org/docs/17/explicit-locking.html),
[SQL Server UPDLOCK/HOLDLOCK](https://learn.microsoft.com/en-us/sql/t-sql/queries/hints-transact-sql-table?view=sql-server-ver17)
and [SQLite immediate transactions](https://www.sqlite.org/lang_transaction.html).
