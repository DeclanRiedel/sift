# Provider and operations completion

Requested 2026-10-27. Deliver bounded, independently validated milestones and
commit each one. This plan supplements the canonical product inventory; a
milestone does not graduate its entire parent feature.

## Milestones

- [x] PostgreSQL extension updates: version-pinned preview/apply and Vim Monitor
  action, with stale-state rejection and explicit script-execution warning.
- [ ] PostgreSQL partition attach and dependency previews: typed bounds, catalog
  identity checks, affected dependencies, reviewed SQL and native acceptance.
- [x] PostgreSQL historical deadlocks: operator-configured bounded log source,
  timestamp/session/resource projection, redaction, retention and audited reads.
- [ ] PostgreSQL rich-schema migrations: extend supported shapes individually
  with native round trips and inverse/explicitly irreversible plans.
- [x] SQL Server unsupported-value hardening: typed refusal before decoder panic,
  disposal of partially consumed connections, and native bulk rollback checks.
- [ ] SQL Server values: lossless sql_variant/UDT representation and binding
  design before driver changes; retain explicit refusals for unproven families.
- [ ] SQL Server imports: reviewed identity/computed modes and bounded progress;
  preserve supervised cancellation and transactional failure behavior.
- [ ] SQL Server advanced DDL/migrations: round-trip each added native shape.
- [ ] SQLite schema changes: rename/drop/alter and designer workflows, preserving
  dependencies, native constraints and root authority; rebuilds need rollback.
- [ ] SQLite operations: restore/delete, larger native streaming transfers, then
  individually designed ATTACH/extensions/functions/virtual-table write scopes.
- [ ] Recovery: unified provider-aware UI, cross-instance identity mapping,
  externally orchestrated recovery and PostgreSQL cluster/PITR tooling.
- [ ] Extensions/MCP: reconcile remaining runtime/contribution and governance
  gaps against shipped behavior, then complete each missing contract and UI.
- [ ] Distribution: platform builds, installers, signing and validation matrix;
  publisher identities and platform evidence must come from real artifacts.
- [ ] AI execution: explicit run authority, side-effect policy, durable execution
  receipts and recovery; native Windows/macOS isolation requires native tests.

## Gates

Design coupled protocol/server/desktop changes before implementation. Preserve
the locked Driver trait, pure serde wire types, supervised driver execution,
opaque secret handles, audited Operations and Vim-only controls. Use existing
preview/apply and production-confirmation paths. Keep unsupported cases explicit.

Each implementation commit records focused behavior coverage, formatting,
strict workspace Clippy and workspace tests. Native provider behavior also
needs existing opt-in live fixtures when available. Platform/signing, operator
log sources and external recovery environments are evidence prerequisites,
not results that Linux mocks can establish. CI remains manual-dispatch only.

## Extension update design

Add a typed UpdateExtension action with an explicit target version. Preview
reads installed state and the catalog-provided update path to that version;
missing installations, no-op targets and absent paths fail closed. Render the
version as a quoted identifier, never a SQL fragment. Fingerprint extension
identity, owner/schema/version and the update path so a changed catalog rejects
apply. PostgreSQL retains final ownership and script privilege enforcement.

Monitor `u` chooses the selected extension's displayed default version and
opens the existing exact-SQL preview. Enter confirms through the existing
audited apply operation; changing connection invalidates the pending preview.
The target remains explicit if the server's default changes after inspection.
Updates may execute extension scripts and acquire locks; no automatic update,
cascade, package download or version ordering inference is introduced.

Reference: https://www.postgresql.org/docs/current/sql-alterextension.html

## Evidence

Extension updates: a disposable PostgreSQL 17.10 cluster executed the exact
catalog-state query and updated hstore 1.4 to 1.8. The final installed version
matched the target; a non-owner role was denied by PostgreSQL. The cluster was
removed after the check. HTTP tests cover pinned SQL, explicit confirmation,
missing update paths, no-op targets and ownership changes between preview and
apply. Formatting, strict workspace Clippy and the complete workspace test suite
passed. ADR-111 records the stable contract. Broader PostgreSQL management stays
partial in the canonical inventory.

## PostgreSQL deadlock history design

Operators register server-local PostgreSQL JSON log files in driver config,
pinning each source to a managed tenant/profile, connection host/port and
database. HTTP callers cannot supply paths. Sources are disabled by default;
missing/mismatched sources fail explicitly. Schema-restricted profiles cannot
read history. The PostgreSQL identity must be superuser or have effective
pg_read_all_stats membership before a source is opened.

Read only the final 4 MiB of a regular file, skip partial/malformed/oversized
lines, select SQLSTATE 40P01 for the exact current database, and retain at most
50 summaries with at most 16 participants. Parse only known numeric resource
forms from deadlock DETAIL; never return message, statement, context, arbitrary
DETAIL or source paths. Accept UTC/numeric-offset timestamps; other log-time
zones are unsupported until an unambiguous conversion contract exists.

Use the existing audited ListDeadlocks operation, SDK and Monitor History view.
File work runs on bounded blocking workers with an absolute timeout, retaining
worker permits after timeout until IO ends. Recheck connection authority after
the read. No raw logs or summaries enter SQLite, no automatic server-log setup,
remote file fetching, log rotation management or durable archive is claimed.

Reference: https://www.postgresql.org/docs/17/runtime-config-logging.html#RUNTIME-CONFIG-LOGGING-JSONLOG

History validation: four focused tests cover source/privilege/database matching,
policy restriction after an allowed read, file/line/event/participant bounds,
timestamp decoding and SQL redaction. The opt-in native fixture starts a
disposable socket-only PostgreSQL cluster, creates a real deadlock using
explicit lock barriers and reads its generated JSON through managed history;
it passed. Formatting, strict workspace Clippy and the full workspace test suite
passed. ADR-112 records the stable source and projection contract.

## SQL Server unsupported-value boundary design

The vendored TDS decoder currently panics before it can describe sql_variant or
CLR UDT metadata. Add a dedicated UnsupportedType error before consuming their
type-specific metadata, map it to UnsupportedResultShape, and invalidate/drop
the physical connection after query or native bulk metadata failure. Never
restore an unread, partially decoded TDS response to the driver map or pool.
The failed logical handle reports ConnectionInvalidated until explicitly closed.
Other connections remain usable. Native acceptance covers variant (including
NULL) and geography metadata; no casts or stringification substitute for lossless
support. Full base-type tagging, collation/precision preservation and symmetric
bind encoding remain a separate protocol/driver design.

Hardening validation: all 12 live SQL Server driver tests passed serially,
including non-NULL/NULL sql_variant and geography refusals, stable invalidated
handles, independent connection success, and CSV-trigger result failure with
zero committed rows. Formatting, strict workspace Clippy and workspace tests
passed. ADR-113 records the boundary. Lossless values remain unchecked.
