# PostgreSQL provider gap checklist

Work tracking (2026-11-11): **Partial / active**. Remaining product work: [current TODO](../TODO.md#postgresql).

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
- [~] Cover custom table storage/options and currently rejected index states.
      Built-in heap table `reloptions`, TOAST relation options, column
      storage/compression overrides, and clustered/replica-identity index state
      now round-trip through native table DDL; standalone index DDL restores
      clustered/replica state too. TOAST options use the `toast.` storage
      parameter prefix in the table's `WITH` clause; a partition child with
      those options remains refused.
      Their catalog shape is fingerprinted and fenced from generic migrations.
      Non-default table tablespaces, non-heap access methods, column
      options/FDW state, invalid or not-ready indexes, and partition-child
      storage/options remain explicit refusals.
- [~] Export extension definitions and dependencies without treating extension
      member objects as independent creations. A version-pinned native install
      recipe now exports the verified PostgreSQL 16 `pg_trgm` 1.6 member
      manifest; changed membership, config tables, extension prerequisites,
      and relation members are refused. Catalog
      membership and version are fingerprinted, graph members link to an
      in-scope extension, and independent member DDL is refused. Package
      equivalence across servers, other extension packages/versions, config
      data, extension updates, and automatic dependency ordering remain open.
- [~] Address standalone indexes and sequence ownership. Sequence DDL now
      round-trips ordinary `OWNED BY` dependencies and refuses internal identity
      or extension member sequences; ownership is fingerprinted and fenced from
      generic migrations. Table DDL refuses extension member indexes and
      sequences. Standalone non-constraint PostgreSQL indexes now have a public
      index object kind and native definition export, including expressions,
      operator classes, included columns, predicates, and storage options.
      Index catalog identity and full native shape are fingerprinted, and
      affected tables are fenced from generic migrations. Constraint-backed,
      partition-attached, extension-member, invalid, and not-ready indexes
      refuse standalone export. Dependency ordering and
      automatic rich-index migration rendering beyond standalone index changes
      on stable existing tables remain open.
- [~] Extend catalog diff/migration to preserve supported rich index, partition,
      policy, and ownership shapes; reject loss before preview/apply. Complete
      definition-bearing PostgreSQL graphs now preview standalone index create
      and drop on an unchanged existing table using captured native DDL and
      qualified `DROP INDEX ... RESTRICT`. Index-set-only parent fingerprint
      changes are represented by the index node; unsafe index states, changed
      tables, missing definitions, and DDL source models are refused. A single
      ordinary owned-sequence create on an unchanged existing heap table now
      renders catalog-captured `CREATE SEQUENCE` before `ALTER SEQUENCE ...
      OWNED BY`; the paired table-fingerprint change is absorbed only when both
      changes are selected. Drop uses `RESTRICT`. Other owned-sequence changes,
      table creation and external dependency ordering remain open. A sole
      policy on a stable existing heap table can be renamed when its catalog
      OID, command, roles, expressions, permissive mode, and RLS flags are
      unchanged; rollback renames it back. A sole policy can also be created
      or dropped on an unchanged one-column `bigint id` heap table with RLS
      enabled when the catalog proves `SELECT TO PUBLIC USING (id > N)` for a
      bounded integer literal. Both directions require privilege-risk
      acknowledgement and have inverse rollback. Other policy definitions,
      body edits, RLS state changes, and multiple policies remain unsupported.
      One ordinary heap child can be attached to or detached from an unchanged
      range-partitioned parent when both tables already exist and the catalog
      proves a single dependency-free partition bound. The paired parent
      fingerprint change is absorbed only with the child change; rollback
      reverses the attachment. Partition creation/drop, multi-child ordering,
      policy/sequence combinations, and index/constraint dependencies remain
      unsupported. One catalog-proven policy create, drop, or rename can now
      share a plan with one standalone index create or drop on a different
      unchanged table; both changes must be selected, and the index and policy
      round-trip through live PostgreSQL. Changes to the same table or a third
      object still refuse preview.

## Workbench and administration

- [x] PostgreSQL 16+ public role lifecycle: typed create/rename/attribute patch/
      drop, native CREATEROLE/ADMIN and privileged-flag checks, bounded shared
      dependency impact and source-bound API/SDK/Vim review (ADR-143).
      Public workload session defaults ship separately (ADR-145). Credentials,
      arbitrary/secret-bearing defaults and dependency remediation remain open.

- [x] Inspect historical deadlocks from operator-configured server-local JSON
      logs through the audited API/SDK and Vim Monitor History. Sources pin
      managed tenant/profile, host/port and database, require upstream statistics
      rights and reject schema-restricted profiles. Reads project at most 50
      events from the last 4 MiB without returning SQL, raw DETAIL or paths.
      Malformed/oversized lines are skipped; unknown wait formats mark incomplete
      participants. A disposable native deadlock fixture and managed-scope,
      redaction, resource-bound and privilege tests pass. Durable archival and
      remote log collection remain operator infrastructure.

### Replication and statistics inspection design

This slice is read-only. The server returns bounded snapshots from
`pg_stat_replication`, `pg_stat_wal_receiver`, and `pg_replication_slots`, plus
current-database and accessible user-table statistics. It omits connection
strings, host addresses, SQL text, and other credential-bearing fields.
Replication reads require effective `pg_read_all_stats` usage or superuser rights;
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
are extension install/drop with RESTRICT, version-pinned extension update,
partition detach and typed attachment of existing ordinary tables. Attachment
includes bounded dependency impact. Extension cascade and general cross-object
dependency management remain separate work. The desktop offers Vim navigation, preview, and explicit confirmation.
An inspection view alone does not complete the management checklist.

- [x] Open CSV quarantine reports from the current import result, with
      authorized retrieval and rejected source-row details.
- [x] Configure and retry durable CSV import from the desktop using a target
      checkpoint table and stable run UUID.
- [x] Reopen retained CSV quarantine reports through a workspace history view.
- [x] Listen to PostgreSQL notifications in the desktop with scoped stream
      cleanup, bounded history, and keyboard navigation.
- [~] Complete broader PostgreSQL plans, process-control and bulk-import desktop
      depth. Bounded Profile/Monitor and reviewed CSV/native import paths exist;
      remaining families stay partial in the product inventory.
- [~] Add extension and partition inspection/management UI through audited,
      previewable operations. Bounded inspection, extension install/drop, and
      partition detach have typed preview/apply and Vim desktop controls;
      extension update to an explicit version now has guarded preview/apply and
      a Vim `u` action targeting the displayed default version. Typed partition
      attachment has a Vim `a` form and bounded dependency impact. General
      dependency graph management and broader object management remain open.
- [~] Add database role, schema grant, and database/schema ownership editing.
      Bounded catalogs and typed guarded preview/apply APIs cover `CREATE ROLE
      NOLOGIN`, explicit schema `USAGE`/`CREATE` grants, and owner changes.
      Source-bound Vim forms now preview and confirm those changes. Role
      attributes and memberships now have native identity/authority-bound review
      (ADRs 140/143). Public workload session defaults have typed global/current-
      database inspection and reviewed set/reset (ADR-145). Credentials, arbitrary
      GUCs remain open. Object privilege mutation now covers all six represented
      families with guarded native transitions (ADR-163); reviewed future-object
      default privileges cover tables, sequences, routines, types and schemas,
      including global/schema scope and grant options (ADR-183). A native selected-role
      ACL matrix now covers database/schema/table/column/sequence/routine access
      and grant options in the API/SDK and source-bound Vim view (ADR-148);
      provenance (ADR-162) and guarded object mutations (ADR-163) now ship;
      cascading revocation, grant-chain paths and additional families remain open.
- [x] Add replication and statistics inspection UI with bounded reads and
      explicit permission errors. Primary senders, standby WAL receiver,
      replication slots, current-database counters, and SELECT-visible table
      counters have audited snapshots and Vim Monitor views. No replication
      control, slot mutation, or inferred rates are included.
- [x] Add a read-only server-settings browser with bounded, role-visible reads,
      sensitive-value redaction, audit, and a Vim desktop view. Settings writes
      remain a separate design requiring scope, policy, confirmation, and audit.
- [~] Add database users/roles, grants, ownership, and RLS editors with
      capability checks and reviewable changes. Bounded policy inspection and
      owner-checked policy rename, typed create/alter/drop, roles/expressions and
      table RLS toggles have native enforcement acceptance (ADR-120). The broader
      grant/effective-privilege families above remain open.
- [~] Connect existing dump/restore, maintenance, and integrity-check backends
      to complete Linux desktop workflows where operator policy permits. Vim
      Monitor now previews and confirms explicit-target VACUUM, ANALYZE, and
      REINDEX through the audited API, and runs bounded amcheck heap checks.
      Dump/restore remains an operator CLI workflow: the desktop needs a
      server-owned, policy-scoped archive transfer and target contract before
      remote instances can use it safely.

## Acceptance

The opt-in live driver suite passed 21/21 tests on 2026-10-04 with one test
thread. Listener cleanup assertions now measure the delta from pre-existing
`sift-listen` connections, since other local processes may own listeners on
the same fixture. This validates the existing driver slice; the broader DDL,
TLS, and scale acceptance items below remain open.
The live server plan/Profile acceptance also passed. Its bounded 100,000-row
stream measured 28.468 ms median over five separate local runs
(27.907–29.682 ms); this is not a portable latency guarantee.
The PostgreSQL 17.10 live catalog fixture now proves 64 scoped tables, 64
foreign keys, 514 graph nodes and 1,089 edges without truncation. Seven
separate local runs measured graph capture at 40.336 ms median
(39.513–43.899 ms), with at most 9,084 KiB peak test-process RSS.

### Replication and statistics inspection implementation

The replication API requires effective PostgreSQL `pg_read_all_stats` usage or
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
Enter confirms and Esc closes it. `u` previews updating an installed extension
to its displayed default version. The target is explicit, not resolved again
at apply; the server checks the installed version, catalog identity, owner,
schema and available update path on every preview/apply. No-op and unavailable
paths are rejected. Updates warn about extension scripts and locks.

In Partitions, `a` opens attachment of an existing ordinary child table. Enter
parent/child schema and table names, choose range/list/hash/default bounds,
and review exact SQL plus at most 50 dependency descriptions. Values use
CSV-style double quoting to preserve commas or literal NULL/MINVALUE/MAXVALUE;
unquoted reserved words have their native bound meanings. Tab moves fields,
Esc leaves editing, `m` cycles modes, `i` returns to editing, and Enter previews.
The reviewed apply rechecks ownership, native columns, parent routing,
dependencies and session interpretation settings. PostgreSQL validates overlap
and rows at apply; scans, locks and native errors remain visible. No table
creation, foreign/partitioned child attachment or automatic dependency rewrite
is implied.
Verification: API/SDK and desktop/workspace UI checks pass; three focused
server integration tests cover pagination, confirmation, apply and stale-state
rejection. Formatting passes. Strict workspace Clippy and full tests remain for
the post-merge integration gate.

- [ ] Live round trips for each added native DDL shape, including restricted
      roles, cross-object dependencies, and explicit unsupported cases.
- [~] Linux TLS certificate verification and larger catalog/result fixtures;
      record tested versions and performance limits rather than claiming all
      PostgreSQL deployments. The 100,000-row result and 64-table catalog
      fixtures above are measured; TLS certificate validation and broader
      catalog shapes and scale remain.
- [x] `cargo fmt`, strict workspace Clippy and workspace tests pass at `e3d6b13e`;
      repeat after each
      implementation slice; opt-in PostgreSQL suites pass for engine changes.


### Explicit role membership workbench

PostgreSQL 16+ now has a bounded explicit membership catalog and Vim grant/update/
revoke forms with named grantors and ADMIN/INHERIT/SET choices (ADR-140). Native
identity-bound preview/apply rejects observations changed since review and uses
RESTRICT for revocation. Other grantors can preserve membership; the catalog is
not an effective privilege matrix. PostgreSQL 17.10 native acceptance verifies
restricted administration, options, multiple grantors, dependency/cycle refusal,
member and grantor replacement, changed options and changed actor. Public role
lifecycle and attributes subsequently graduated in ADR-143; reviewed public
session defaults follow in ADR-145. Passwords, other GUCs and reviewed cascade
remain open.


### Native selected-role privilege matrix

Read-only inspection now evaluates native has_*_privilege checks for a selected
role, bounded by the connected actor's object visibility. PUBLIC, inherited
membership, grant options, column ACLs and owner/superuser semantics use native
results, with RLS and query-authorization caveats. Vim Roles v opens a source-bound
matrix with virtualized object navigation and explicit access/grant cells.
See [design and acceptance](postgres-privilege-matrix.md), ADR-148. This does not
ship object grant/revoke forms, inheritance-path provenance or every native
privilege family; those remain open.
