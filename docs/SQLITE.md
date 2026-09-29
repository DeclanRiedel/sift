# SQLite connections

This provider requires public protocol **2** and metadata migration **V046**.
Update the server and clients together. Regenerate existing instance locks with
`sift instance lock <root>` and apply the reviewed manifest. Extension packages
must declare protocol 2 compatibility and regenerate their signed locks; the
extension RPC and driver RPC versions remain 1. Old binaries cannot use SQLite
profiles. V046 preserves existing profile IDs and credential references while
widening the provider discriminator; it does not migrate user database files.

SQLite files belong to the Sift **server**. The desktop uses saved profiles with
a configured root ID and relative path. Opening a profile never creates a file.
No credentials are needed. Read-only is the default and a read-only root always
wins over a profile's requested write access.

For a reproducible instance, add this to its `sift.toml`, then regenerate its
lock and apply it with the normal instance commands:

```toml
[server.drivers.sqlite]
max_connections = 8

[server.drivers.sqlite.roots.analysis]
path = "databases"
allowed_tenants = [1]
read_only = false

[[connections]]
name = "analysis/sqlite"
tenant = "default"
provider = "sqlite"
credential_mode = "shared"

[connections.sqlite]
root_id = "analysis"
path = "warehouse.db"
mode = "read_write"
busy_timeout_ms = 1000
```

Use the actual tenant name and numeric ID from your instance. ID 1 is the first
tenant in a fresh single-tenant instance, not a universal ID. Relative root paths
resolve against the instance root. For the standalone development configuration,
use `[drivers.sqlite]` and `[drivers.sqlite.roots.analysis]` instead.

Roots must be operator-controlled local directories. Absolute profile paths,
traversal, URIs, symlinks, hard-link aliases, special files and Sift state files
are rejected. The file boundary is implemented on Unix and accepted on Linux;
Windows file access currently fails closed. Network filesystems, hostile local
filesystem replacement, SQLCipher, external extensions and attached databases
are outside the supported scope. Writable SQLite databases may create journal,
WAL and shared-memory sidecars. Sift preserves their journal and sync settings.

## Supported workflows

- Native statement/selection/batch execution, streamed dynamic values, query
  variables, copy/export, cancellation and reconnect. A failed batch preserves
  earlier autocommit statements; use a managed transaction for atomic work.
- Serializable read/write and read-only transactions, named savepoints,
  rollback-to and release. Use Sift's transaction controls; raw transaction SQL
  is rejected. A busy COMMIT retains the transaction for retry. Cancellation
  discards the connection and transaction. These choices follow SQLite's
  [transaction semantics](https://www.sqlite.org/lang_transaction.html).
- Main/TEMP explorer, columns, PK/FK/unique indexes, generated/hidden columns,
  STRICT/WITHOUT ROWID behavior, native table/view/index/trigger DDL and refresh
  after changes from another connection. Ordinary table and column CHECK clauses
  expose their names, scope, and expression text from native CREATE SQL.
  Native DDL remains authoritative for every expression and trigger body; CHECK
  metadata is omitted for virtual tables and CREATE SQL larger than 1 MiB.
- SQLite completion, aliases/CTEs, statement selection, formatting and
  diagnostics. Native SQLite executes SQL independently of the editor parser;
  parser coverage is incomplete for some SQLite DDL, including trigger bodies.
- Parameterized inline edits for ordinary tables with catalog-proven non-null
  primary/unique keys. Nullable composite keys, expression/partial unique indexes,
  views, virtual tables and writes to generated columns are rejected.
- Estimated `EXPLAIN QUERY PLAN` trees with native detail text and no invented
  costs or actual timings. ANALYZE/actual plan capture is unsupported.
- Atomic CSV imports, including table creation, with abort, skip-conflict, or
  quarantine behavior. Quarantine uses per-row savepoints and returns rejected
  source rows with bounded reasons; the desktop report workflow remains open.
  Bounded batches roll back together on fatal failure.

The IDE catalog graph is explicitly partial. It includes same-schema foreign
keys, trigger targets, parsed trigger-body dependencies, parsed direct view
reads, FTS5 external-content dependencies, and column references in parsed
CHECK clauses, partial-index predicates, generated columns, and expression
indexes. Missing or ambiguous references stay unresolved. Unsupported
expression syntax, virtual-table modules, table functions in views, and
trigger/view SQL the parser cannot read remain gaps;
affected nodes carry `sqlite_dependency_gap` markers. Virtual-table columns
are omitted under the restricted connection authorizer and marked with
`sqlite_metadata_gap`.
Durable catalog snapshots and diff are available with partial coverage. Preview,
test rollback and apply admit only additive creation of ordinary `main` tables
whose stored DDL contains simple INTEGER, REAL, TEXT, BLOB or NUMERIC columns
with optional NOT NULL. The plan is one-use, revision-bound, audited and
transactional. Unsupported native shapes and all drops, alters and renames are
refused. The Vim desktop baseline action accepts SQLite's partial snapshot and
previews restoring missing supported tables; any other diff change refuses the
plan. Full dependency graphs, general schema migration, database designer
mutations, process controls, notifications, desktop native bulk/transfer targets,
ATTACH/DETACH, unsafe PRAGMAs and file/extension functions
are not advertised.

The audited SQLite maintenance API previews and confirms two scoped file
actions: create a new empty database or take an online backup of the connected
`main` database. Both require a managed writable SQLite connection, an allowed
tenant and a new relative destination in the same configured root. A preview
does not create the file; apply requires its one-use token and explicit
confirmation. Existing destinations, symlink parents, protected paths and
files above 1 GiB are refused. Backups use bounded SQLite online backup steps,
including WAL state. Creation needs no prior backup because the destination is
empty; keep a verified backup before later mutating maintenance. This API does
not register a profile for the new database or offer VACUUM, restore, delete,
or arbitrary PRAGMA writes (ADR-078). The Vim Monitor maintenance view previews
the managed root, source, destination, and backup expectation before a typed
confirmation and apply. It also runs the existing read-only integrity check
(ADR-081).

Values retain SQLite storage classes: null, signed 64-bit integer, float, text
or bytes, including mixed classes in one result column. Decimal parameters bind
as text; a destination's numeric affinity may still coerce them. Generated CSV
decimal/date/time columns use TEXT, booleans INTEGER. Invalid UTF-8, non-finite
numbers, intervals and opaque native parameters fail explicitly. Parameters must
use all anonymous `?` slots or contiguous `?1.. ?N`, in one statement.

The audited `bulk-insert` API also accepts SQLite `format=native` with typed
rows. A preview validates an existing ordinary `main` table, explicit column
affinities, and a bounded payload; apply requires a matching one-use token and
`confirm_write`. It accepts at most 10,000 rows, 128 columns and 8 MiB of typed
data, then inserts batches within one managed transaction with a 120-second
deadline checked between batches. Decimal values are
accepted only as canonical text with at most 38 digits and scale 18, into TEXT
affinity columns; NUMERIC affinity would coerce the value and is refused.
Triggers, virtual/generated/hidden columns, implicit storage-class conversion,
and non-finite numbers are refused. This target does not provide a socket-level
bulk protocol, resumable upload, or desktop transfer-source integration (ADR-074).

Each connection owns one admitted worker, one active operation and bounded page
buffers. Overlapping catalog, semantic and query requests wait asynchronously
for that worker, with a five-second admission deadline; startup catalog loading
does not reject the first query as busy. Admission stays held until the worker
finishes, even if its caller disconnects. The default worker cap is 8 (hard ceiling 128), a page holds at most
128 rows or approximately 1 MiB, and SQLite limits a value/record to 8 MiB.
Sift also rejects result rows whose combined cell payload exceeds 8 MiB.
Busy waits are bounded to 0–5000 ms and observe cancellation. OS I/O that cannot
be interrupted continues to occupy its worker permit until it exits.

## Seeded demo

```sh
nix run .#sift-desktop-demo-wiki
# Or the desktop alone:
nix run .#desktop-demo
# Only create the SQLite fixture:
nix run .#sift-demo-sqlite -- /path/to/demo.db
```

Both desktop demos include `demo/postgres` and `demo/sqlite`. The desktop demo
preserves Sift metadata, including saved queries and workspaces, across launches. SQLite lives at
`demo-data/demo.db` below the demo instance root. The fixture includes customers,
orders, products, a summary view, a change trigger, generated columns, a partial
index and a 100,000-row `large` table. Repeated seeding preserves existing files
and edits. Ordinary `sift instance new` instances do not inherit demo databases.

## Inspect Sift's own metadata

Sift stores users, connection-profile metadata, rooms, workspaces and other app
state in SQLite. Runtime credentials use the separate secret store.

For a local applied instance, use the profile menu or command palette:
**View Sift Metadata (Read-only Snapshot)**. Sift creates a fresh inspection
instance, remembers it in the instance picker, and opens it. Expand its
`sift/metadata-inspection` connection to browse or query the snapshot. Return to
the original instance through the instance picker. Each invocation creates a new
snapshot; it does not refresh older inspection instances.

To inspect
the desktop demo's metadata while it is running:

```sh
nix run .#sift-desktop-metadata
# Inspect another applied instance:
nix run .#sift-desktop-metadata -- /path/to/source-instance
```

The command creates and opens a separate instance containing
`sift/metadata-inspection`, a **read-only snapshot**, owned by the source
instance's bootstrap identity. It retains the generated instance and prints its
location. Rerun the command for a fresh snapshot; it is not a live connection.
Without the desktop launcher:

```sh
sift metadata inspect /path/to/source-instance /path/to/new-inspection-instance
sift-desktop --instance-root /path/to/new-inspection-instance
```

The local Unix account must own the source metadata file. A consistent read
transaction includes committed WAL contents. Only reviewed columns from tenant,
principal, membership, connection_profile, room, workspace and workspace_node
are copied. Authentication/session/token data, secret handles, connection
configuration, SQL/history, document contents, vaults and repository contents
are excluded. The snapshot retains names and workspace paths and is private
to the local owner. Source DDL/triggers are never copied. The export is capped
at 10,000 rows per table, 32 MiB of values, 1 MiB per source value/record and a
30-second SQLite execution budget. `inspection_tables` records truncation and
`inspection_info` records the timestamp and operation; the new instance also
records the operation in its audit log. Existing destinations are never replaced.

Metadata inspection deliberately cannot query or modify the live Sift metadata
database. Generic SQLite profiles remain unable to open Sift's state directory.

The desktop reuses one read-only inspection instance per source manifest ID.
Invoking View Metadata inside that inspection keeps the current instance open;
it does not create another inspection of the inspection. Existing snapshots stay
fixed at their creation time. The CLI can create a fresh snapshot when needed.

The desktop demo also seeds `siftdemo` in the local SQL Server Docker container
and imports its `.env`-managed credential through stdin. The connection is
`demo/sql-server`; try `SELECT * FROM lab.order_summary`. Docker must be running.
`nix run .#dev-mssql seed` seeds this database independently and preserves rows.

Canonical DDL tabs are read-only inspection views, including triggers such as
`record_order_status`. Switch to Query to write executable SQL. Opening a stored
CREATE statement does not recreate the object.
