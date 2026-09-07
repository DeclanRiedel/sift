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
  after changes from another connection. CHECK metadata is included when parsed;
  native DDL remains authoritative for all expressions and trigger bodies.
- SQLite completion, aliases/CTEs, statement selection, formatting and
  diagnostics. Native SQLite executes SQL independently of the editor parser;
  parser coverage is incomplete for some SQLite DDL, including trigger bodies.
- Parameterized inline edits for ordinary tables with catalog-proven non-null
  primary/unique keys. Nullable composite keys, expression/partial unique indexes,
  views, virtual tables and writes to generated columns are rejected.
- Estimated `EXPLAIN QUERY PLAN` trees with native detail text and no invented
  costs or actual timings. ANALYZE/actual plan capture is unsupported.
- Atomic CSV imports, including table creation, with abort or skip-conflict
  behavior. Bounded batches roll back together on failure; quarantine is unsupported.

The IDE navigation catalog is explicitly partial. Full dependency graphs,
schema comparison/migration, database designer mutations, process controls,
notifications, native bulk/transfer targets, database creation, ATTACH/DETACH,
unsafe PRAGMAs and file/extension functions are not advertised.

Values retain SQLite storage classes: null, signed 64-bit integer, float, text
or bytes, including mixed classes in one result column. Decimal parameters bind
as text; a destination's numeric affinity may still coerce them. Generated CSV
decimal/date/time columns use TEXT, booleans INTEGER. Invalid UTF-8, non-finite
numbers, intervals and opaque native parameters fail explicitly. Parameters must
use all anonymous `?` slots or contiguous `?1.. ?N`, in one statement.

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
