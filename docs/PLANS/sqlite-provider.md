# SQLite provider design

Status: **designed for implementation, 2026-09-06; not implemented or graduated.**
Requested before queuing the four tasks in
[the overnight handoff](database-provider-overnight.md). Implementation follows
the bounded [PostgreSQL/SQL Server graduation](postgres-sqlserver-graduation.md).
This plan records proposed decisions; graduate proven decisions into an ADR.

## Outcome and integration choice

Add a first-party `crates/driver-sqlite` using the workspace's bundled
`rusqlite` dependency (currently 0.32). Implement the existing Driver methods
without changing their signatures. Register provider `sift/sqlite`, dialect
`sift/sqlite`, and native discriminator `Engine::Sqlite`. Do not route SQLite
through PostgreSQL or T-SQL and do not reuse the metadata store or its pool.

Use the native adapter, not an extension subprocess. The existing native Driver
boundary is the smallest fit; external RPC expansion is unnecessary for this
scope. All user actions retain existing Operation authorization, audit, resource
limits, isolated execution, cursor registry, and timeout paths. Protocol types
remain pure serde; UI dependencies stay out of shared crates.

The provider version is Sift's package version. Ping reports the actual bundled
SQLite runtime version, logical database name, empty current_user (no database
principal), and no pool warmth. Test the bundled runtime rather than promise
compatibility with every installed system SQLite library or extension.

## First implementation scope

| Surface | Initial contract |
| --- | --- |
| Connections | Open existing local-server database files, read-only or read-write; saved profiles, test, disconnect |
| SQL | Statement, selection, unparameterized document batches; positional parameterized statements; typed streamed results and cancellation |
| Transactions | Begin, commit, rollback, read-only enforcement; one active stream per handle |
| Explorer | main/temp tables, views, columns, PK/FK/unique/check metadata where proven, indexes and triggers; shallow/deep refresh |
| DDL | Native stored definitions for ordinary tables, indexes, views, triggers; no invented reconstruction |
| IDE | SQLite syntax, statement selection, formatting, catalog completion, aliases/CTEs, diagnostics with uncertain types explicit |
| Results | Existing grid, copy/export, paging; parameterized inline edits only with catalog-proven stable keys |
| Unsupported initially | Savepoint controls, graph/migration/designer mutation, bulk/transfer targets, structured explain, process control, notifications |

Raw EXPLAIN queries can run as SQL, but do not advertise structured plan capture
or fabricate costs. Do not advertise graph/schema-migration support from a
partial dependency model. CSV export can consume query results; import/bulk
capabilities remain off until their SQLite DML/type contract is implemented.

Also defer database creation, file upload/download, ATTACH/DETACH, URI filenames,
shared-cache or named in-memory databases, SQLCipher, user-loaded extensions,
custom collations/functions, virtual-table editing/creation, backup/maintenance
UI, and cross-file browsing. Ordinary file fixtures cover initial tests.

## Configuration and file authority

Public ProviderConnectionSpec configuration is provider-specific, with no host,
port, user, password, or database TLS fields:

```json
{
  "provider_id": "sift/sqlite",
  "configuration": {
    "root_id": "analysis",
    "path": "warehouse.db",
    "mode": "read_only",
    "busy_timeout_ms": 1000
  },
  "credential_handles": {}
}
```

Require root_id/path; default mode to read_only and busy_timeout_ms to 1000,
bounded to 0..5000. Reject unknown fields and credentials. `path` is a relative
path below an operator-configured root on the Sift server, never the desktop's
filesystem on a remote connection. Open-existing only: never set CREATE flags
and never silently create an empty database after a typo.

Add `drivers.sqlite` instance configuration with named roots, allowed tenants,
root read-only policy, and max_connections (initial default 8). No roots means
file access is disabled. Personal instances also use explicit roots; the local
file picker may select only files under those roots. Remote UI offers root and
relative-path entry, with a clear server-file label; no local picker pretending
to select a remote file. Root discovery must expose only authorized root IDs,
not unrestricted directory listings or absolute host paths.

Resolve root permissions at the server adapter before Driver::open. Transport
the approved path in the SQLite EngineConnectionSpec variant; legacy network
fields in the internal ConnectionSpec are empty/None, not fake hostnames.
Recheck authorization for profile reopen and ad-hoc connection testing. Effective
read-only is the strictest of profile policy, root policy, and requested mode.
Persist root ID and relative path as configuration, never resolved secrets.

Reject absolute paths, traversal, NULs, file: URIs, :memory:, symlinks/reparse
points, and non-regular files. Deny Sift's metadata/secrets/state locations and
aliases to them using file identity where available. Root directories must be
operator-controlled: canonicalization alone does not prevent a hostile local
process replacing a file between validation and SQLite's path-based open.
Do not claim a filesystem sandbox against such a process. Validate Windows
drive/UNC/reparse behavior before declaring Windows file access supported.
An unproven platform path boundary fails closed rather than weakening checks.

SQLite may create journal/WAL sidecars beside a writable database. Include this
in root policy/documentation. Do not change journal_mode or synchronous on open;
respect the existing database. Network-mounted databases are outside initial
support. No new environment variables are needed unless config loading requires
them; any introduced variables must be listed in .env.example and loaded through
the existing .env lifecycle.

## Worker ownership, admission, and cancellation

One admitted handle owns one SQLite connection on a dedicated worker thread,
a bounded command mailbox, and one active execution generation. All prepare,
step, schema, transaction, and close calls run there. Async Driver methods send
commands and await replies; none execute SQLite on a Tokio request worker.
Do not put permanently resident workers in Tokio's shared blocking pool.

Acquire a driver permit before spawning/opening, in addition to server tenant
limits. Reject exhaustion immediately with PoolExhausted; never start unlimited
threads or queue unlimited work. Reject concurrent execution on the same handle
with a stable busy error. Retain the permit until the worker actually exits.
Timeouts must not release capacity while abandoned workers remain alive.

Page buffers have existing server byte/row ceilings. Use cancellation-aware
bounded sends (including while the consumer is stalled); bare blocking_send
cannot be the only escape from backpressure. Keep statements on their owning
thread and do not buffer complete result sets. Cap individual values using
SQLite runtime limits before allocating protocol cells, plus existing result
limits. Cursor receiver drop, eviction, disconnect, and shutdown all cancel work.

Cancellation validates connection/cursor/generation, marks the handle closing,
sets a cancellation flag, and uses rusqlite's interrupt handle. Serialize that
decision with execution completion/admission so a late cancel cannot interrupt
the next query. Add progress and bounded busy handling that observe cancellation
and deadline, including cancel-before-first-step. Interrupt alone is a no-op
when no statement is running. [SQLite interrupt semantics](https://sqlite.org/c3ref/interrupt.html)
and [progress callbacks](https://sqlite.org/c3ref/progress_handler.html) define
the engine boundary.

Initial cancel/timeout policy discards the connection, matching the existing
server invalidation pattern, rather than promising transaction preservation.
Emit QueryCanceled or QueryTimedOut once, invalidate server transaction/cursor
state, and require reopen. Finalize statements and roll back before close on
the worker. Catch worker panics; close the channel with a typed terminal error.
Server close/shutdown waits only within existing deadlines. A stuck OS file I/O
may outlive that deadline; the occupied worker stays charged against the cap.

## Execution and SQL authority

Use rusqlite Batch for SQLite-native statement boundaries, including trigger
bodies and semicolons in strings. Prepare and execute sequentially so later
statements may reference tables created earlier. Stop at the first error;
earlier autocommit statements remain committed. Never imply document execution
is atomic and never replay a failed write automatically.

For parameters, initial support is one statement with contiguous ?1..?N slots
(anonymous ? slots are equivalent). Reject named/sparse binds and multiple
statements before executing any part of a parameterized request; use parser
validation plus SQLite parameter metadata, not string splitting. Query-variable
compilation must produce SQLite bind markers. Validate cardinality exactly.

Send NextResult before rows even for empty results; decode DML RETURNING normally.
Report affected rows only for DML, using each completed statement's change count,
not stale counts after SELECT/DDL. Preserve existing batch outcome conventions
and terminal errors. A stream close before Done is not success.

Install the SQLite authorizer before preparing user SQL, alongside normal
server operation policy. SQL text checks alone are insufficient. Deny ATTACH,
DETACH, load_extension, writable_schema, unsafe directory/output controls,
VACUUM INTO, and changes to driver-owned protection settings. Disable extension
loading, enable defensive mode, and set trusted_schema off. The hooks feature
exists in the locked rusqlite source and can be enabled in the new driver crate.
[SQLite authorizer](https://www.sqlite.org/c3ref/set_authorizer.html)

Use an explicit read-only PRAGMA allowlist (schema introspection, integrity
reads, version/compile metadata); reject unknown and mutating PRAGMAs initially.
SQLite can run some PRAGMAs during prepare, so authorization must precede it.
Set foreign_keys on and private-cache mode at initialization. Guard server-owned
transaction operations with a narrow internal authorization context; user SQL
BEGIN/COMMIT/ROLLBACK/SAVEPOINT/RELEASE is rejected initially to avoid diverging
from Sift's transaction state. Document using the existing transaction controls.
[PRAGMA behavior](https://www.sqlite.org/pragma.html)

## Transactions and errors

Support Serializable explicitly; set SQLite transaction UI defaults to it.
The current generic TxMode default is ReadCommitted: do not silently pretend
SQLite implements that mode. Reject all other isolation requests explicitly.
Read-write begin uses BEGIN IMMEDIATE; read-only begin uses BEGIN DEFERRED plus
connection-local query_only and authorizer enforcement. Restore prior protections
after completion. Nested transactions and savepoint controls remain unsupported.

Check is_autocommit after errors. If SQLite ended a managed transaction, remove
the stale server transaction reference; invalidate the connection if existing
state APIs cannot express that transition safely. COMMIT returning BUSY retains
the transaction/handle for an explicit later commit or rollback; fix server
consume-on-error behavior if necessary. Never automatically rerun the user's
transaction. These behaviors follow [SQLite transaction semantics](https://www.sqlite.org/lang_transaction.html).

Map interruption, deadline, permissions, missing/corrupt/non-database files,
constraints, invalid parameters, BUSY/LOCKED, and disk/I/O failures to stable
DriverErrors with SQLite extended native codes. Reuse existing generic codes
where truthful; use sanitized Other plus native_code where no precise portable
code exists. Do not label lock contention as a pool failure or infer successful
rollback after an unknown write outcome.

## Values and metadata

Decode each cell's runtime storage class: NULL, INTEGER -> Int64, REAL -> Float64,
TEXT -> Text, BLOB -> Blob. A result column may contain different classes across
rows. Declared type/affinity is metadata, not permission to cast every value.
Do not parse strings into dates, JSON, UUIDs, or decimals on read. Expression
columns with no proven type use a SQLite Native dynamic type with Other category;
never infer exact metadata from the first row. Reject invalid text encoding with
a typed error rather than silently replacing bytes.
[SQLite dynamic typing](https://www.sqlite.org/datatype3.html)

Bind Null/TypedNull as NULL, integer widths as checked i64, floats as REAL,
Bool as 0/1, Text as TEXT, and Blob as BLOB. Bind Decimal as its canonical text,
never f64; SQLite target affinity may still coerce it, so cross-engine transfer
must not advertise exact decimal preservation. Serialize JSON compactly, UUID
canonically, dates/times as ISO text, and TimestampTz as UTC RFC3339. Reject
Interval and opaque Native parameters initially. Reject non-finite floats rather
than silently changing them to NULL. Publish these conversions in provider docs.

## Catalog, DDL, caches, and editing

Use one logical catalog per open file, with main and temp schema names. Object
identity includes provider, approved root/path identity, and schema/name/kind;
never use file basename alone. Connection-private temp schema snapshots and
semantic caches must not leak across handles or tenants.

Read sqlite_schema and table-valued PRAGMAs/table_xinfo, index_list/index_xinfo,
and foreign_key_list with bounded filters. Preserve column order, default text,
generated/hidden flags, declared types, PK ordinals, FK grouping/actions, index
expressions and sort/collation metadata when known. Proposed SqliteColumnFacets
carry declared_type, affinity, default_expr, primary_key_ordinal, and hidden kind
(normal/virtual-hidden/generated-virtual/generated-stored). Use exact stored
CREATE SQL to recover properties missing from PRAGMAs; unsupported parse shapes
remain unknown instead of becoming fabricated constraints or dependencies.

Object DDL returns stored CREATE SQL plus explicit dependent index/trigger
definitions in a documented order for table export. Skip automatic indexes with
NULL sql; their constraints live in the table definition. Preserve STRICT,
WITHOUT ROWID, generated expressions, CHECK, collation, and AUTOINCREMENT text.
Do not export sqlite_sequence runtime counters. Objects with no exportable SQL
fail explicitly. Existing virtual tables may be listed/read when their built-in
module is present, but their DDL and edits stay unsupported. [Schema table](https://www.sqlite.org/schematab.html)

Read main/temp schema_version around schema loads; retry only bounded read-only
introspection when the revision changes. Refresh on local DDL and explicit
refresh; check revisions before reusing a cached catalog for semantic requests
on the existing bounded metadata path. Retain current user-transaction visibility.
Do not use file mtime as the catalog revision or install triggers in user files.

Inline edits require a proven non-null unique identity. SQLite ordinary composite
primary keys may be nullable: PK marking alone is insufficient. Do not synthesize
rowid identities or edit views/virtual tables initially. Exclude generated columns
from writes and preserve existing optimistic conflict/preview/audit behavior.

## Protocol and integration migration

Current public protocol constant is 1. Adding Engine::Sqlite, SQLite connection
and column facets is public shape growth under ADR-017: implement a protocol
bump and explicit compatibility decision, regenerate schemas/OpenAPI/fixtures,
and update SDK/server/desktop/extension compatibility docs together. Rebase on
any protocol bump from preceding graduation tasks; do not hard-code a stale
version or silently expand an already-recorded released contract.

Audit exhaustive two-engine dispatch in registry configuration/credentials,
capabilities, session policy, DDL/DML, quoting, SQL variables, semantic Flavor,
formatting, import/transfer, schema comparison, plan/process/savepoint handlers,
and desktop connection/transaction UI. Unsupported SQLite branches must fail
explicitly, not fall through to SQL Server SQL. No new core Driver verbs or
SqliteExt are required for initial scope.

Initially advertise only driver.core@1, driver.cancel@1,
driver.transactions@1, driver.schema.shallow@1, and driver.schema.deep@1 after
their evidence passes. Ensure operation gating also covers built-in-only routes.
Do not inherit a built-in quality label without meeting its host-owned corpus.
SQLite semantic support is a separate dialect corpus; parser uncertainty remains
visible and does not block ordinary valid SQL execution except policy controls.

## Implementation slices and acceptance

- [ ] S1: Protocol/configuration design and compatibility note, provider identity,
      typed SQLite spec/facets, root policy, descriptor and disabled capability
      dispatch. Add crate using the pinned bundled SQLite dependency.
- [ ] S2: Admitted workers, open/ping/close, guarded SQL, streamed runtime values,
      parameters/batches, cancellation/deadlines, transactions and error mapping.
- [ ] S3: Shallow/deep metadata, exact DDL, revision/cache ownership, stable-key
      inline DML; gate graph/migrations/bulk/plan features explicitly.
- [ ] S4: SQLite semantic pack, query variables, file-profile UI, read-only and
      isolation selection, existing Vim query/result/export/transaction flows.
- [ ] S5: Complete focused engine/server/UI contract tests, workspace checks,
      provider support docs and evidence. Graduate an ADR only for proven scope.

Tests use temporary database files and ordinary local workspace tests, with no
external database service or new smoke script. Cover mixed storage classes,
NULL/empty/large results and blobs, exact decimal binding, parameter mismatch,
trigger-body batches, partial batch failure and DML RETURNING. Exercise two
handles contending for a file, commit BUSY, auto-rollback, readonly bypass attempts,
cancel-before-start/during-step/under-backpressure/after-completion, shutdown and
permit retention. Verify a cancelled cursor cannot affect another query.

Include traversal/symlink/URI/state-file/root/tenant denial, no-create behavior,
ATTACH/VACUUM INTO/unsafe PRAGMA denial, malformed database errors, ordinary and
STRICT/WITHOUT ROWID/generated/index/trigger DDL round trips, nullable keys,
external DDL refresh, and temp schema isolation. Exercise full HTTP/SDK paths
and unsupported operation gating, plus the SQLite semantic and Vim UI flows.

Run cargo fmt, workspace Clippy with warnings denied, and cargo test --workspace.
Record bundled SQLite version, platform, representative large-result memory,
cancellation latency, and unresolved platform checks. Keep performance evidence
distinct from correctness and never claim unrun cross-platform validation.

## Authorized follow-on design: advanced scope and local inspection

After S1–S5, add managed savepoints using a server-internal `SqliteExt` (the
locked core Driver signatures stay unchanged). Names are quoted identifiers;
create, rollback-to and release retain existing Operation/audit paths. Capture
estimated plans with `EXPLAIN QUERY PLAN`, preserving SQLite's detail text and
parent IDs; costs and actual runtime remain absent, and ANALYZE is explicitly
unsupported. CSV import uses bounded parameterized INSERT batches inside an
explicit transaction with rollback on failure; retain the published affinity
conversion limits. Neither feature implies graph/migration support.

Demo setup creates a separate seeded SQLite file under an explicitly configured
demo root, alongside PostgreSQL. Repeated setup is idempotent and never replaces
an existing file containing user edits. Both desktop-demo variants expose the
saved SQLite profile. The file belongs to the server instance, with a documented
root/path configuration that also works without the desktop launcher.

The metadata convenience command is local-owner-only and creates an inspection
snapshot in a dedicated root, never a writable connection to Sift's live state.
Build a fresh SQLite database from an explicit allowlist of metadata tables and
columns; omit authentication sessions, tokens, secret handles and credential
configuration. Deny generic access to the live metadata database and its aliases.
The command opens that snapshot through the normal audited connection path and
labels it as a snapshot with its creation time. It must not imply live refresh.
