# PostgreSQL provider gap checklist

Status: active Linux backlog. PostgreSQL passed the bounded ADR-055 daily-driver
scope; this checklist tracks work beyond that scope. A checked item here requires
working behavior and evidence, not merely a visible control. The
[acceptance matrix](database-provider-acceptance.md), [DDL gaps](ddl-gaps.md),
and [product inventory](ide-parity-and-provider-extensibility.md) remain the
sources for current support claims.

## Native definitions and migrations

- [~] Export partition children and inheritance without losing partition bounds,
      attachment, indexes, and dependency order; partition children with local
      indexes, constraints, and triggers now round-trip and are fenced from
      structural migrations. Single-parent inheritance with local columns and
      indexes also round-trips. Parent tables are fenced and fingerprint direct
      descendants even when those children are outside the requested schema;
      multiple inheritance also round-trips in catalog parent order. Full
      dependency order remains open.
- [x] Export foreign-table server, table options, and column options in native
      DDL. Definition reads require table ownership and foreign-server USAGE;
      a live round trip and restricted-role denial cover the supported shape.
      The referenced foreign server must exist when replaying the DDL.
- [~] Represent row-level security policies and rules in native DDL, with
      explicit ownership and grant boundaries. Table export now includes policies,
      ENABLE/FORCE state, rewrite rules and their enabled state, plus a migration
      fence; ownership and grants remain.
- [ ] Cover custom table storage/options and currently rejected index states.
- [ ] Export extension definitions and dependencies without treating extension
      member objects as independent creations.
- [ ] Address standalone indexes and sequence ownership. Any new public object
      kind or signature follows ADR-017 and a protocol bump.
- [ ] Extend catalog diff/migration to preserve supported rich index, partition,
      policy, and ownership shapes; reject loss before preview/apply.

## Workbench and administration

### Replication and statistics inspection design

This slice is read-only. The server returns bounded snapshots from
`pg_stat_replication`, `pg_stat_wal_receiver`, and `pg_replication_slots`, plus
current-database and accessible user-table statistics. It omits connection
strings, host addresses, SQL text, and other credential-bearing fields.
Replication reads require `pg_read_all_stats` membership or superuser rights;
PostgreSQL permission errors are surfaced explicitly. Database statistics are
limited to `current_database()`, and table statistics require SELECT privilege
on each relation. Managed schema-restricted profiles cannot use these
cross-schema views. Each endpoint has its own audited Operation and
PostgreSQL-only capability. Queries use the existing supervised driver path,
with a fixed server-owned SQL statement, hard row limits, and paged table rows.

The desktop Monitor adds Replication and Statistics views with Vim navigation,
refresh, and table-statistics paging. Values are snapshots, not live rates;
statistics reset time and unavailable counters remain explicit. No
replication control, slot mutation, setting changes, or polling loop is added.

### Extension and partition workbench design

This slice separates database extensions from Sift's own extension packages.
Read operations return bounded, role-visible PostgreSQL extension and partition
catalog rows through the existing supervised query path, with PostgreSQL-only
capabilities and audited Operations. Catalog reads must not infer ownership or
privilege from a displayed name. Permission failures remain explicit.

Management uses a frozen, server-generated preview containing the exact SQL,
the affected object, and the risk. A later apply request repeats the typed
action and preview token; the server rechecks the current catalog and the
caller’s operation policy before executing through the supervised query path.
Apply rejects active editor transactions, uses the request timeout, and leaves
PostgreSQL permission and dependency failures visible to the user.
Identifier inputs are quoted, never accepted as SQL fragments. Initial actions
are extension install/drop with RESTRICT and partition detach; partition attach,
extension update/cascade, and cross-object dependency previews remain separate
work. The desktop offers Vim navigation, preview, and explicit confirmation.
An inspection view alone does not complete the management checklist.

- [x] Open CSV quarantine reports from the current import result, with
      authorized retrieval and rejected source-row details.
- [x] Configure and retry durable CSV import from the desktop using a target
      checkpoint table and stable run UUID.
- [x] Reopen retained CSV quarantine reports through a workspace history view.
- [x] Listen to PostgreSQL notifications in the desktop with scoped stream
      cleanup, bounded history, and keyboard navigation.
- [ ] Finish the PostgreSQL plans, process-control, and bulk-import
      desktop workflows currently marked partial in the product inventory.
- [~] Add extension and partition inspection/management UI through audited,
      previewable operations. Bounded inspection, extension install/drop, and
      partition detach have typed preview/apply and Vim desktop controls;
      partition attach, extension update, dependency graph previews, and broader
      object management remain open.
- [x] Add replication and statistics inspection UI with bounded reads and
      explicit permission errors. Primary senders, standby WAL receiver,
      replication slots, current-database counters, and SELECT-visible table
      counters have audited snapshots and Vim Monitor views. No replication
      control, slot mutation, or inferred rates are included.
- [x] Add a read-only server-settings browser with bounded, role-visible reads,
      sensitive-value redaction, audit, and a Vim desktop view. Settings writes
      remain a separate design requiring scope, policy, confirmation, and audit.
- [ ] Add database users/roles, grants, ownership, and RLS editors with
      capability checks and reviewable changes.
- [ ] Connect existing dump/restore, maintenance, and integrity-check backends
      to complete Linux desktop workflows where operator policy permits.

## Acceptance

### Replication and statistics inspection implementation

The replication API requires PostgreSQL `pg_read_all_stats` membership or
superuser and returns explicit Forbidden otherwise. It caps senders and slots
at 200 rows each with truncation flags and reads at most one WAL receiver.
Connection strings, client addresses, SQL text, and credential-bearing fields
are omitted. The statistics API reads only `current_database()` counters and
pages SELECT-visible user tables (100 in the desktop, API maximum 200, offset
ceiling 10,000). All values are read-only snapshots; lag may be unavailable,
table row counts are estimates, and rates are not inferred. Managed profiles
restricted to selected schemas are denied these cross-schema endpoints.
The desktop Monitor provides Replication and Statistics tabs with Vim `j/k`
selection, `r` refresh, `n/p` statistics paging, and Esc to return to the
active pane. Both actions use supervised query execution and audited Operations.
Verification: backend/SDK and desktop/UI checks passed, and three focused
server integration tests cover privilege denial, safe typed fields, and
statistics pagination. A UI state regression test protects against stale
responses after connection changes. Strict workspace Clippy and full tests
remain for the post-merge integration gate; no live PostgreSQL role fixture was
run in this isolated worktree.

### Extension and partition workbench implementation

The server exposes audited paged PostgreSQL extension and partition catalog
reads (100 rows per desktop page, API maximum 200, offset ceiling 10,000).
Partition reads require SELECT privilege on parent and child. Managed profiles
with schema restrictions cannot use this cross-schema workbench; read-only
profiles cannot apply changes. The preview endpoint quotes identifiers and
returns exact SQL, a risk warning, and a hash of the observed catalog state.
Apply requires explicit confirmation, repeats the catalog read, rejects stale
state, checks query and object-operation permissions, and runs through the
server's supervised query path. PostgreSQL enforces ownership, CREATE and
dependency rules; errors are shown directly. Preview checks are not a lock on
concurrent DDL, so PostgreSQL may still reject an apply after preview.

The desktop Monitor has Extensions and Partitions views. Vim `j/k` selects,
`n/p` pages, `r` refreshes, `i` previews extension install, and `d` previews
extension drop or partition detach. The preview shows exact SQL and warning;
Enter confirms and Esc closes it. Other management actions remain open.
Verification: API/SDK and desktop/workspace UI checks pass; three focused
server integration tests cover pagination, confirmation, apply and stale-state
rejection. Formatting passes. Strict workspace Clippy and full tests remain for
the post-merge integration gate.

- [ ] Live round trips for each added native DDL shape, including restricted
      roles, cross-object dependencies, and explicit unsupported cases.
- [ ] Linux TLS certificate verification and larger catalog/result fixtures;
      record tested versions and performance limits rather than claiming all
      PostgreSQL deployments.
- [ ] `cargo fmt`, strict workspace Clippy, and workspace tests pass after each
      implementation slice; opt-in PostgreSQL suites pass for engine changes.
