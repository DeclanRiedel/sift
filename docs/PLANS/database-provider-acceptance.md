# Database provider acceptance evidence

Status: **scoped acceptance passed, 2026-09-06.** This file records executed checks for the
[graduation checklist](postgres-sqlserver-graduation.md); no unrun gate is implied
by an implementation checkbox. SQLite implementation/evidence follows separately.

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
