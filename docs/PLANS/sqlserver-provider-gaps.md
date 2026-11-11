# SQL Server provider gap checklist

Work tracking (2026-11-11): **Partial / active**. Remaining product work: [current TODO](../TODO.md#sql-server).

Status: active Linux backlog. SQL Server passed the bounded ADR-055 daily-driver
scope. This list separates deliberate exclusions from shipped behavior; the
[acceptance matrix](database-provider-acceptance.md), [DDL gaps](ddl-gaps.md),
and [product inventory](ide-parity-and-provider-extensibility.md) define the
current public support boundary.

## Values, plans, and native definitions

### Disabled and untrusted constraint round-trip design

Support the native `is_disabled` and `is_not_trusted` states of ordinary CHECK
and foreign-key constraints on rowstore tables. Definition export creates the
constraint, then emits `ALTER TABLE ... NOCHECK CONSTRAINT` to disable it, or
NOCHECK followed by CHECK to restore an enabled but untrusted state. Constraint
names are quoted and ordered. `NOT FOR REPLICATION` is rendered in the native
CHECK and foreign-key positions. The catalog graph fingerprints these state
bits and fences structural migration for affected tables, because the generic
diff does not model them. A disposable SQL Server fixture must replay the
generated DDL into a second schema and compare regenerated definitions and
catalog state; a metadata-only assertion is insufficient.

Implemented on the local SQL Server fixture: enabled but untrusted and disabled
CHECK and foreign-key constraints replay into a second schema with identical
regenerated native DDL. The fixture also replays `NOT FOR REPLICATION` on both
constraint kinds, verifies the migration fence, and confirms that a trust
change alters its graph fingerprint. Other advanced constraint forms remain
rejected. The full SQL Server provider
acceptance matrix and workspace gates remain separate.

- [x] Decode `money` and `smallmoney` without a floating-point round trip;
      exact positive/negative boundaries, fractions, and NULLs pass through
      live TDS responses. The local Tiberius 0.12.3 patch retains signed
      ten-thousandths as `Numeric(scale=4)` before the driver formats
      `Value::Decimal`; revisit the patch on upstream upgrades.
- [ ] Decide a lossless representation and bind contract for supported
      `sql_variant`/UDT families; retain explicit unsupported errors for others.
      The first hardening slice now rejects sql_variant/CLR UDT metadata through
      a typed UnsupportedResultShape error before the vendored decoder's panic
      paths. Query and CSV bulk failures discard the partially consumed TDS
      connection; the logical handle becomes ConnectionInvalidated. Bulk abort
      uses disconnect rollback rather than sending SQL over an unread response.
      Lossless value decoding, native base-type metadata and binding remain open.
      All 12 live driver tests passed serially for this hardening slice, including
      variant/NULL/geography refusals and bulk-trigger rollback, together with
      formatting, strict workspace Clippy and workspace tests.
- [x] Add actual execution plans with scoped permissions, supervised execution,
      cancellation, result/plan bounds, and a separate measured-plan UI. The
      API and desktop path use `STATISTICS XML`; live parameterized capture,
      oversized-result refusal, and disposable restricted-login `SHOWPLAN`
      grant/revoke acceptance pass.
- [~] Export supported temporal, memory, replication, and policy table shapes,
      or keep each explicit rejection until its round trip is proven. Basic
      system-versioned tables now round-trip; other shapes remain excluded.
- [~] Preserve advanced storage, nonordinary indexes, disabled/untrusted
      constraints, CLR/table types, and bound defaults/rules where supported.
      Sparse nullable columns and uniform ROW/PAGE compression on ordinary
      rowstore tables and indexes round-trip. Ordinary disabled/untrusted CHECK
      and foreign-key constraints now round-trip, including `NOT FOR
      REPLICATION`; other listed shapes remain.
- [x] Export synonym DDL and dependency references. Native `CREATE SYNONYM`
      round-trips in the live SQL Server fixture, and graph nodes retain the
      catalog base-object path as an unresolved dependency when no target edge
      is proven. Definition export requires `VIEW DEFINITION`.
- [~] Extend schema diff/migration to represent new native shapes without
      silently reducing them to ordinary tables or indexes. A complete,
      same-database graph can now render one catalog-proven CHECK or foreign-key
      disabled/trusted state change on an otherwise stable ordinary table.
      Trust restoration validates existing rows and requires a data-rewrite
      acknowledgment. Temporal/history transitions, replication flags,
      advanced column/compression shapes, and multi-object native migrations
      remain fenced.

## Workbench and administration

- [x] Open CSV quarantine reports from the current import result, with
      authorized retrieval and rejected source-row details.
- [x] Configure and retry durable CSV import from the desktop using a target
      checkpoint table and stable run UUID.
- [x] Reopen retained CSV quarantine reports through a workspace history view.
- [~] Finish plan, process-control, and bulk-import desktop workflows. SQL Server
      process control now uses Vim Monitor selection, reviewed termination,
      capability reasons, and stale-response guards. CSV bulk import now has
      target-column mapping, sampled rows, a dry-run gate, pending/result and
      local cancellation states, and a transfer-recipe route with durable
      checkpoint retry. Reviewed identity Generate/Preserve and computed Omit
      modes now share CSV/XLSX/typed-Parquet review/apply, disposable transaction
      ownership, active progress, SDK/Vim cancellation and explicit unknown
      COMMIT receipts (ADR-142). These opt-in modes use existing ordinary tables,
      supported scalar conversions and atomic Abort; legacy import paths retain
      their previous cancellation/progress contract. Plan desktop work remains. Retain
      SQL Server's documented abort-and-discard cancellation and savepoint limits.
- [x] Add read-only Query Store inspection before designing any plan-forcing
      action. The audited API and desktop monitor show database state, permission
      needs, and up to 100 recent plans with bounded SQL text and runtime metrics.
- [x] Add a read-only SQL Server Agent jobs browser with bounded owned-job
      reads for non-sysadmins, whole-job history, permission-aware state,
      audit, and Vim desktop view. Native SQLAgentUserRole, ReaderRole and
      OperatorRole owned-job reads now pass via sysjobs_view and bounded native
      history procedures on an owned connection. Revocation, no-history,
      latest whole-job selection, redaction, 101-job truncation and preserved
      editor ROWCOUNT pass (ADR-129). Remote-target history can remain unavailable.
- [x] Add a read-only SQL Server server-settings browser with bounded
      `sys.configurations` reads, permission-aware state, audit, and Vim desktop
      view. Native SQL Server 2022 configured/effective values, explicit SELECT
      denial, typed PermissionRequired and subsequent recovery pass (ADR-129).
- [~] Add login, user, role, permission, grant, and ownership editors with
      audited preview/apply paths. Bounded metadata-visible catalogs and Vim
      inspection cover logins, database principals, role membership, schema
      owners, and explicit schema permissions. Typed API preview/apply covers
      database role creation, user-defined role membership, and schema SELECT
      grant/revoke with production confirmation. Source-bound Vim forms now
      create roles, add/remove memberships and grant/revoke schema SELECT;
      selected membership/grant removal remains available. Reviewed current-database
      and schema ownership transfers bind native identities and disclose native
      permission loss (ADRs 121/122). SQL-login user create/remap/orphan repair,
      default-schema change and drop now use native identity-bound review and
      source-bound Vim forms (ADR-132). Login credentials, other identity-provider/
      contained-user families and external-owner families remain open.
      A source-bound Vim effective-permission matrix now uses native checks on
      a disposable connection for the fresh actor or selected ordinary SQL
      database user (ADR-172). Database/schema/table/view/column/sequence/SQL-routine
      checks retain native NULLs, inheritance and DENY exceptions; server-token,
      grant-option/chain and external identity coverage remain separate. See
      [scope and evidence](sqlserver-effective-privileges.md).
- [x] Connect existing copy-only backup/new-name restore and integrity-check
      APIs to Linux desktop workflows. The Vim monitor previews recovery SQL,
      requires an exact typed confirmation for apply, and shows integrity
      findings. Mocked desktop dispatch and validation tests cover the guard;
      live backup/restore acceptance remains in the broader acceptance item.

- [~] Reviewed single-index maintenance: offline REBUILD, REORGANIZE and FULLSCAN
      index statistics ship through audited API/SDK and source-bound Vim controls
      with restricted-user native acceptance (ADR-147). Enabled ordinary
      single-partition rowstore scope only; advanced storage, disabled indexes,
      online/resumable and database-wide orchestration remain open. See
      [scope and acceptance](sqlserver-index-maintenance.md).

### SQL Server desktop maintenance design

Expose a SQL Server-only Maintenance view in the Vim monitor. Backup takes an
existing user database and an absolute **server-side** archive path. Restore
takes a new database name, archive path, backup-set number and explicit logical
file to destination mappings; the backend rejects replacement and runs header,
file-list and VERIFYONLY checks. Each recovery action first sends the existing
audited API request with `apply=false` and shows the generated SQL and warnings.
The desktop holds that exact preview request, rejects changed form inputs, and
requires a typed `BACKUP <database>` or `RESTORE <database>` confirmation before
sending the same request with `apply=true`. A connection change invalidates the
preview. No client-side SQL or path rewriting is allowed.

Integrity checks target the connected database only, with a physical-only
option and no repair mode. Show the native outcome, bounded findings and
warnings. Run/check buttons and Vim shortcuts use the existing audited SDK
calls. A per-request ID prevents a response from a previous connection or
form revision from replacing the current view. Validation uses mocked API
responses; no desktop test invokes a real backup or restore.

## Acceptance

The opt-in live driver suite passed 10/10 tests against the local SQL Server
container on 2026-10-04 with one test thread. This covers the existing driver
slice; the broader DDL, restricted-principal, TLS, and scale acceptance items
below remain open.
The live server plan/Profile acceptance also passed. Its bounded 100,000-row
stream measured 97.033 ms median over five separate local runs
(94.713–97.849 ms); this is not a portable latency guarantee.
The SQL Server 2022 16.0.4250.1 live catalog fixture now proves 64 scoped
tables, 64 foreign keys, 514 graph nodes and 1,089 edges without truncation.
Seven separate local runs measured graph capture at 550.271 ms median
(456.971–601.958 ms), with at most 9,532 KiB peak test-process RSS.

- [ ] Live SQL Server round trips for every added type, DDL shape, plan, and
      administration operation; exercise restricted principals and refusal.
- [~] Linux certificate verification and representative larger catalog/result
      fixtures; record tested SQL Server versions and limits. The 100,000-row
      result and 64-table catalog fixtures above are measured; certificate
      validation and broader catalog shapes and scale remain.
- [x] `cargo fmt`, strict workspace Clippy and workspace tests pass at `e3d6b13e`;
      repeat after each
      implementation slice; opt-in SQL Server suites pass for engine changes.
