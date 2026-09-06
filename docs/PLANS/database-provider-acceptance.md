# Database provider acceptance evidence

Status: **PostgreSQL/SQL Server scoped acceptance passed 2026-09-06; SQLite
accepted 2026-09-07.** This file records executed checks for the
[graduation checklist](postgres-sqlserver-graduation.md); no unrun gate is implied
by an implementation checkbox. SQLite evidence follows below.

## Declared PostgreSQL / SQL Server scope

| Surface | PostgreSQL | SQL Server |
| --- | --- | --- |
| Connection | Native driver, configured TCP/socket and TLS modes | Native TDS driver, configured encryption/trust settings, warm-idle pool |
| Query values | Native single-statement decoding, exact numeric strings, exact microsecond interval binds | Supported TDS primitives, exact decimal results, explicit typed NULL binds |
| Batch values | Simple-query batches expose text/NULL; use statement execution for native typed metadata | Native typed result sets; unparameterized SQL batches preserve session scope |
| Native escape hatch | Engine-native text binding and display; calendar-month intervals stay native | Opaque UDT/sql_variant decoding and Native binds excluded from lossless round trips |
| Object DDL | Tables, partition roots, views/materialized views, routines, sequences, triggers, enum/composite/domain types | Ordinary tables, views/routines, sequences, triggers, alias types |
| Table DDL fidelity | Native type modifiers, collation, defaults, identity options, stored/virtual generated expressions, constraints, native indexes, trigger states, owned serial configuration | Native type modifiers, collation, defaults, identity seed/increment, computed/persisted expressions, PK/unique/FK/check, ordinary clustered/nonclustered indexes with ordering/includes/filters, trigger states |
| Transactions | Begin/commit/rollback and savepoints | Begin/commit/rollback and savepoint/rollback-to; no release |
| Cancellation | Cooperative backend cancellation | Abort and connection discard |
| Engine operations | Existing plans, process control, COPY and LISTEN/NOTIFY | Existing plans, process control and CSV bulk import |

DDL exports schema definitions, not live counters, data, object grants/owners,
statistics or a complete database dump. Cross-object references remain references;
exporting one object does not recursively export its dependencies. Partition root
DDL does not include child partitions. Standalone indexes remain attached to table
export because ObjectKind does not yet expose index addressing.

PostgreSQL excludes partition children/inheritance, foreign-table options, RLS,
rules, custom table storage/options and unsupported index state from table export.
SQL Server excludes temporal/memory/replication/policy tables, advanced storage,
nonordinary/disabled indexes, disabled/untrusted constraints, CLR/table types and
bound defaults/rules, non-default heap filegroups and non-default/ALTER-only
trigger modules. These boundaries must return explicit errors. Type/trigger
names that cannot be resolved uniquely or definitions hidden by permissions do
not produce fabricated SQL. Extension/synonym DDL stays deferred.

The explorer/catalog projection is not a lossless DDL source. Native object
export and structural schema diff/migration are separate scopes; richer metadata
not represented by migration contracts must remain gated. Basic query/DDL
support does not certify arbitrary cross-engine conversion or full DBA parity.

## Executed checks

Use `nix develop --command bash -c 'set -a; source .env; set +a; …'` on this host:
non-direnv tool shells do not automatically load .env. No credentials belong in
this evidence file. Both databases were started through existing development
helpers; tests create isolated objects and do not reset demo/user databases.

- PostgreSQL 17.10 and SQL Server 2022 16.0.4250.1, Linux x86_64.
  PostgreSQL live driver suite: **19 passed**. SQL Server live driver suite:
  **8 passed** after the session-batch correction.
- Server native DDL round trips: **2 passed**, including restricted principals
  and rich-column migration fences. Existing PostgreSQL DDL fixtures:
  **5 passed**, including non-default sequences and routine signatures.
- Server plans/process/savepoint/stream/timeout acceptance: **2 passed**.
  SQL Server estimated plans use declared parameter types, not runtime parameter
  sniffing. SQL Server actual plans remain explicitly unsupported. PostgreSQL
  ANALYZE uses rollback even for SELECT INTO; rollback failure closes the handle.
  Rollback cannot undo external function effects or sequence consumption.
- Local regression budget: 100,000 streamed rows per engine in <10 seconds,
  <=1024 rows per page; 150ms request timeout acknowledged within <3 seconds.
  Observed PostgreSQL 33.9ms, SQL Server 104.5ms. Direct acceptance binary peak
  child RSS 15,960 KiB for the combined run (compiler excluded). This small local
  fixture is regression evidence, not a production throughput guarantee.
- Native DDL/schema fixture (both engines, including graph enrichment): 0.381s
  elapsed and 11,632 KiB peak child RSS. The fixture stays below the same 10s
  local regression budget; this does not establish enterprise-catalog scaling.
- Required `cargo fmt`, workspace Clippy with warnings denied and
  `cargo test --workspace`: **passed**. Updated API fixtures exercise native
  DDL passthrough and the exclusive SQL Server plan operation.

Local connections exercised socket trust and SQL Server encrypted transport with
TrustServerCertificate. Production certificate verification, other platforms and
large enterprise catalogs remain unverified and outside this scoped acceptance.
SQL Server money/smallmoney currently pass through Tiberius floating decoding;
use an explicit CAST to decimal for exact financial values. Opaque UDT/sql_variant
results and native parameters are excluded from lossless transfer.

Commands: `cargo test -p sift-driver-postgres --features live-pg --test live_pg`,
`cargo test -p sift-driver-sqlserver --features live-mssql --test live_mssql`, and
`cargo test -p sift-server --features live-pg,live-mssql --test ddl_native_round_trip
--test ddl_round_trip --test plan_capture_live -- --nocapture` in the environment
above. RSS measured with Python resource.getrusage(RUSAGE_CHILDREN) around the
compiled plan_capture_live test binary, not Cargo.

## SQLite scope and evidence — 2026-09-07

SQLite 3.46.0 from bundled rusqlite 0.32.1 was exercised on Linux x86_64.
The supported contract is [SQLite connections](../SQLITE.md), including explicit
tenant-authorized server roots, existing files, native values/batches, bounded
workers, cancellation/discard, Serializable transactions and savepoints, native
DDL, partial navigation catalogs, stable-key edits, estimated plans and atomic
CSV import. It is an `ide_capable` provider, not a claim of the full signed,
cross-platform `sift_certified` release matrix.

Executed evidence:

- **7 real-file driver tests**: exact text decimal binds and integer limits,
  blob/NULL/mixed storage classes, native trigger batches and RETURNING, empty
  results, parameter rejection, partial batch failure, read-only bypass attempts,
  root/tenant/traversal/symlink/hard-link denial, open-existing behavior,
  transaction contention and retryable COMMIT BUSY, stalled/running cancellation
  with capacity recovery, late cancellation, implicit transaction rollback,
  large-value rejection, STRICT/WITHOUT ROWID/generated/expression/partial-index
  native DDL round trips and external schema refresh.
- **1 real SQLite HTTP/SDK integration test**: saved-profile opening, nested
  savepoints, rollback/release, TEMP catalog visibility, qualified completion,
  parameterized estimated plans, atomic CSV abort/skip, zero-row updates,
  optimistic inline edits and conflict rollback, nullable-key rejection,
  external refresh and query cancellation removing server transaction state.
- Semantic trigger-body/CASE/quoted-semicolon selection and a desktop form test
  prove SQLite uses its own dialect and root/path/read-only fields without
  carrying credentials from a previously selected network provider.
- V046 upgrade coverage preserves a SQL Server profile and its credential FK,
  accepts SQLite discriminators and passes `foreign_key_check`. Historical
  metadata boundaries and lifecycle/backup fixtures remain covered.
- Metadata snapshot test includes committed WAL data, excludes an authentication
  table, refuses destination replacement and preserves the original data.
  End-to-end CLI `instance new`, `instance apply`, `metadata inspect` created an
  applied inspection instance: nine expected tables, four source rows, 0700
  instance directory and `integrity_check = ok`.
- `nix run .#sift-demo-sqlite` created the 100,000-row fixture with valid integrity;
  reseeding preserved an added customer. Nix built and shell-checked
  `desktop-demo`, `sift-desktop-demo-wiki` and `sift-desktop-metadata` wrappers.
  A graphical desktop launch was not performed in this acceptance run; GPUI's
  existing headless interaction tests cover the edited connection form.
- Direct final compiled-driver measurement: 100,000 streamed rows in **74.1 ms**,
  0.087 seconds total fixture runtime and **21,632 KiB** peak child RSS, with
  <=128 rows per page. This fixture also exercises oversized-row rejection;
  the RSS includes those allocations. Compiler processes are excluded. This is a local
  regression measurement, not a throughput or enterprise-catalog guarantee.

Commands: `cargo test -p sift-driver-sqlite --test provider`,
`cargo test -p sift-server --test sqlite_provider`, normal workspace semantic,
metadata and GPUI tests, and the CLI/Nix commands above. Final `cargo fmt`,
`cargo clippy --workspace --all-targets -- -D warnings` and
`cargo test --workspace` **passed** after execution-count and aggregate-row
limits were checked. EXPLAIN/ANALYZE do not inherit stale DML counts; a row with
two 5 MB blobs is rejected before building an oversized protocol row.

Protocol 2 regressions also passed: **19 PostgreSQL**, **8 SQL Server** live
driver tests, **2 native DDL**, **5 PostgreSQL DDL** and **2 server plan/execution**
tests. The two server stream measurements remained 32.1 ms/103.6 ms respectively.

Explicit exclusions: Windows file authority (fails closed), untested macOS
deployment, network/hostile-local filesystems, full dependency/migration graphs,
virtual-table mutation, extension loading/ATTACH, actual plans, native bulk or
transfer targets, quarantine import, creation/maintenance UI and full SQLite
DDL parser coverage. CHECK metadata is best-effort; native definitions remain
authoritative. Exhaustive filesystem fault injection and enterprise-scale
catalog stress are not claimed by this graduation scope.
