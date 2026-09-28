# SQL Server provider gap checklist

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
names are quoted and ordered. `NOT FOR REPLICATION` remains rejected until
its syntax and behavior are tested. The catalog graph fingerprints these state
bits and fences structural migration for affected tables, because the generic
diff does not model them. A disposable SQL Server fixture must replay the
generated DDL into a second schema and compare regenerated definitions and
catalog state; a metadata-only assertion is insufficient.

Implemented on the local SQL Server fixture: enabled but untrusted and disabled
CHECK and foreign-key constraints replay into a second schema with identical
regenerated native DDL. The fixture verifies the migration fence and that a
trust change alters its graph fingerprint. `NOT FOR REPLICATION` and other
advanced constraint forms remain rejected. The full SQL Server provider
acceptance matrix and workspace gates remain separate.

- [x] Decode `money` and `smallmoney` without a floating-point round trip;
      exact positive/negative boundaries, fractions, and NULLs pass through
      live TDS responses. The local Tiberius 0.12.3 patch retains signed
      ten-thousandths as `Numeric(scale=4)` before the driver formats
      `Value::Decimal`; revisit the patch on upstream upgrades.
- [ ] Decide a lossless representation and bind contract for supported
      `sql_variant`/UDT families; retain explicit unsupported errors for others.
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
      and foreign-key constraints now round-trip; `NOT FOR REPLICATION` and
      other listed shapes remain.
- [x] Export synonym DDL and dependency references. Native `CREATE SYNONYM`
      round-trips in the live SQL Server fixture, and graph nodes retain the
      catalog base-object path as an unresolved dependency when no target edge
      is proven. Definition export requires `VIEW DEFINITION`.
- [ ] Extend schema diff/migration to represent new native shapes without
      silently reducing them to ordinary tables or indexes.

## Workbench and administration

- [x] Open CSV quarantine reports from the current import result, with
      authorized retrieval and rejected source-row details.
- [x] Configure and retry durable CSV import from the desktop using a target
      checkpoint table and stable run UUID.
- [x] Reopen retained CSV quarantine reports through a workspace history view.
- [ ] Finish plan, process-control, and bulk-import desktop workflows currently
      marked partial in the product inventory; retain SQL Server's documented
      abort-and-discard cancellation and savepoint limits.
- [x] Add read-only Query Store inspection before designing any plan-forcing
      action. The audited API and desktop monitor show database state, permission
      needs, and up to 100 recent plans with bounded SQL text and runtime metrics.
- [x] Add a read-only SQL Server Agent jobs browser with bounded owned-job
      reads for non-sysadmins, whole-job history, permission-aware state,
      audit, and Vim desktop view. Live role-specific acceptance remains open.
- [x] Add a read-only SQL Server server-settings browser with bounded
      `sys.configurations` reads, permission-aware state, audit, and Vim desktop
      view. Live SQL Server 2022 role-specific acceptance remains open.
- [~] Add login, user, role, permission, grant, and ownership editors with
      audited preview/apply paths. Bounded metadata-visible catalogs and Vim
      inspection cover logins, database principals, role membership, schema
      owners, and explicit schema permissions. Typed API preview/apply covers
      database role creation, user-defined role membership, and schema SELECT
      grant/revoke with production confirmation. The Vim view previews and
      applies removal of a selected membership or schema SELECT grant.
      Login credentials, user
      mapping, ownership editing, and effective-privilege matrix remain open.
- [x] Connect existing copy-only backup/new-name restore and integrity-check
      APIs to Linux desktop workflows. The Vim monitor previews recovery SQL,
      requires an exact typed confirmation for apply, and shows integrity
      findings. Mocked desktop dispatch and validation tests cover the guard;
      live backup/restore acceptance remains in the broader acceptance item.

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

- [ ] Live SQL Server round trips for every added type, DDL shape, plan, and
      administration operation; exercise restricted principals and refusal.
- [ ] Linux certificate verification and representative larger catalog/result
      fixtures; record tested SQL Server versions and limits.
- [ ] `cargo fmt`, strict workspace Clippy, and workspace tests pass after each
      implementation slice; opt-in SQL Server suites pass for engine changes.
