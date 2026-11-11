# SQLite connections

Current development status and remaining work: [TODO](TODO.md); detailed [plan index](PLANS/README.md).

SQLite support entered public protocol **2** with metadata migration **V046**.
Current binaries use public protocol **5** and metadata migration **V054**.
Update the server and clients together. Regenerate existing instance locks with
`sift instance lock <root>` and apply the reviewed manifest. Extension packages
must declare current protocol compatibility and regenerate their signed locks; the
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
test rollback and apply admit additive creation of ordinary `main` tables
whose stored DDL contains simple INTEGER, REAL, TEXT, BLOB or NUMERIC columns
with optional NOT NULL. The plan is one-use, revision-bound, audited and
transactional. Explicit accepted table-rename mappings also admit these simple
shapes when both catalogs are unfiltered and untruncated, cover main/TEMP, and
contain only proven simple tables and columns. Rename plans preserve native
data and include the exact transactional inverse. Accepted column mappings
also support native column renames within an unchanged simple table. The
backend proves the whole declaration, accounts for every changed name, and
carries the companion table-proof change without rebuilding data. Column-only
selection remains selective; overlapping selected mappings are scheduled, with
cycle-breaking intermediate names visible in the reviewed transactional SQL
(ADR-123). The Vim table designer can rename existing columns through this
reviewed path. Typed RenameObject previews derive table/column targets from
verified live proof; preview never executes DDL (ADR-124).
Views, triggers, indexes, constraints and incomplete dependency projections
refuse rename planning. The same complete simple-table proof now admits appended
nullable INTEGER/REAL/TEXT/BLOB/NUMERIC columns without defaults or constraints
(ADR-149). Typed AddColumn preview derives native facets/canonical proof without
executing DDL. The migration validates both declarations and unchanged original
columns; a whole-table selection must include all additions, while column-only
selection may choose a contiguous prefix. Native validation rolls back its owned
transaction; apply exposes NULLs without rewriting existing cells. Added-column
rollback is lossy: plans omit automatic DROP COLUMN SQL and disclose the reason.
The Vim designer exposes the new-column row with TEXT initially and fixed NULL;
existing-column type/nullability and reorder edits remain unavailable.
The same proof admits native removal of ordinary columns while retaining at least
one column (ADR-174). Typed DropColumn previews are available from the selected
SQLite designer column using Preview removal, or `d` on the column row. Review
shows exact native SQL and warns that committed column cells are permanently
lost. Explicitly acknowledge data loss with Space on the acknowledgement control,
then Apply; the acknowledgement binds the exact plan identity. Restore a backup
to recover committed removed data; no automatic inverse SQL is generated.
Validation rolls back native removal. Surviving column order, rowids, values and
storage classes are preserved. Whole-table/ordinal-companion selection requires
all removals; column-only selection may remove a subset. Mixed add/rename/type/
nullability/reorder changes, the last column and dependency-bearing shapes refuse
removal plans. Other drops and general alters remain refused. The Vim desktop
baseline action accepts SQLite's partial snapshot and previews restoring missing
supported tables; that baseline workflow still refuses other diff changes.
Full dependency graphs, general schema migration, database
designer mutations beyond proven renames, nullable additions and column removals,
process controls, notifications,
larger streaming native transfers, ATTACH/DETACH, unsafe PRAGMAs and
file/extension functions are not advertised.

The audited SQLite maintenance API previews and confirms empty-file creation
and online backups of the connected
`main` database. These actions require a managed writable SQLite connection, an allowed
tenant and a new relative destination in the same configured root. A preview
does not create the file; apply requires its one-use token and explicit
confirmation. Existing destinations, symlink parents, protected paths and
files above 1 GiB are refused. Backups use bounded SQLite online backup steps,
including WAL state. Creation needs no prior backup because the destination is
empty; keep a verified backup before later mutating maintenance. This API does
not register a profile for the new database or offer delete or arbitrary
PRAGMA writes (ADR-078). In-place `VACUUM` is separately admitted
for a managed writable root: preview reports a conservative extra-space need,
and apply requires a verified-backup acknowledgment and exact confirmation.
Execution is cancellable with a 120-second native cap; the server request
timeout defaults to 30 seconds and may end it first. SQLite may change implicit
ROWIDs during VACUUM;
see the [SQLite VACUUM documentation](https://www.sqlite.org/lang_vacuum.html)
(ADR-085). The Vim Monitor maintenance view previews
the managed root, source, destination, and backup expectation before a typed
confirmation and apply. It also runs the existing read-only integrity check
(ADR-081). In Vim mode, `c` selects create, `b` backup, `v` VACUUM, `i` integrity,
`s` selects restore, `p` previews, `a` applies after confirmation, and `r` runs
the integrity check.

Explicit-target statistics and index maintenance are available through the same
reviewed API and Vim panel (ADR-146). `n` selects ANALYZE of one ordinary table in
main; `x` selects REINDEX of one main index, including native constraint indexes.
Enter the exact table/index name, preview SQL and native source, acknowledge a
verified backup, then type the displayed source-and-object confirmation. Full
ANALYZE can change query plans. Both actions write a native transaction, with
bounded review, stale-target proof under a write lock, cancellation, rollback and
restored worker authority. Virtual/shadow/internal tables, TEMP/attached targets,
database-wide or collation-name commands are excluded. Names cap at 255 bytes,
proof at 201 definitions/1 MiB, and source plus WAL at 1 GiB. Native preview caps
at ten seconds and apply at 120 seconds; server timeout may end observation first.
Timeout does not establish commit outcome. Physical ENOSPC and native Windows/
macOS acceptance remain open/deferred.

SQLite RESTORE replaces the connected `main` contents from an existing backup
in the same managed root (ADR-125). Preview validates the backup without
replacing data. Apply requires a fresh one-use token, explicit confirmation and
a verified pre-restore backup acknowledgment. The Vim Monitor offers `s` for
Restore, an existing-backup field and the exact confirmation phrase
`RESTORE root/target FROM backup`. This overwrites all main schema and data;
the selected backup is preserved. Native backup transactions roll back
unfinished copies. Root/tenant, read-only, unrestricted-schema, path/file,
1 GiB and deadline/cancellation checks apply. The source read snapshot includes
WAL data. A timeout alone cannot establish whether restore committed.
Cross-root restore, deletion and native Windows/macOS acceptance remain open.

Values retain SQLite storage classes: null, signed 64-bit integer, float, text
or bytes, including mixed classes in one result column. Decimal parameters bind
as text; a destination's numeric affinity may still coerce them. Generated CSV
decimal/date/time columns use TEXT, booleans INTEGER. Invalid UTF-8, non-finite
numbers, intervals and opaque native parameters fail explicitly. Parameters use
native anonymous `?`, positive `?NNN`, or case-sensitive `:name`, `@name`, `$name`
slots in one statement (ADR-138). Prefixes are part of a name; repeated names and
numbered aliases share one value. New names and anonymous binds take the next
index after the largest assigned index. Mixed forms are supported when the final
slots are contiguous; gaps, zero/oversized indexes and parameterized batches are
refused before any statement step. Unicode and Tcl namespace/suffix names follow
the bundled native tokenizer. Internal `#` parameters remain unsupported.

The positional value vector follows native slot order, checked against prepared
metadata before binding. SQL executes unchanged; normalized placeholders are
used only for policy/classification. For example, `:x, :x, ?1, @x, $x` needs three
values. `$9, $1` needs two values in occurrence order; these are names, not
PostgreSQL indexes. Lexical SQL errors retain `SyntaxError`; invalid slot layouts
and arity retain `InvalidParameterValue`. Native requests support up to 32766 slots; desktop forms and
reusable definitions support up to 256. Definition save/edit/rerun checks the
actual slot count. Definitions store no values and review always asks for fresh
ones. Legacy definitions with an incorrect count require editing.

Vim parameter forms use `i` to edit, Tab/Shift-Tab to switch fields, Enter to run,
and Escape to cancel. Labels show native indexes and bounded aliases (up to
eight aliases, 64 characters each, with truncation/count indicators); the original
SQL remains authoritative. Definition review retains alias labels after values
are entered, and Tab after the last field returns to the confirm controls.
Templates reserve native slots before generated `?NNN` bindings and ignore
quoted/comment/identifier content. A template inside an atomic native parameter
name is refused. For example, `:x + {{value}}` compiles the template value as
`?2` while retaining `:x`. Existing template and workload-confirmation rules apply.

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
