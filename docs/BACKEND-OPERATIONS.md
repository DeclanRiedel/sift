# Backend data operations

These features are API/operator workflows; no desktop interaction is required.

## Native integrity checks

`POST /v1/sessions/{session}/connections/{connection}/integrity` and SDK
`check_integrity` accept one explicitly selected check:

- `{"check":"sqlite","quick":false}`: `main.integrity_check`, or `quick_check`
  when quick is true. Attached databases and foreign-key violations are outside
  this check; see [SQLite PRAGMA](https://www.sqlite.org/pragma.html).
- `{"check":"postgres_heap","schema":"public","name":"items"}`: PostgreSQL
  14+ `verify_heapam` using the catalog-discovered, already-installed `amcheck`
  extension. No automatic installation. Checks the selected heap, not indexes,
  partitions recursively, or TOAST values; see [amcheck](https://www.postgresql.org/docs/current/amcheck.html).
- `{"check":"sql_server","physical_only":false}`: current-database
  `DBCC CHECKDB` with no repair option. Physical-only skips additional logical
  checks. Native limitations apply, including memory-optimized tables and
  unsupported database editions; see [CHECKDB](https://learn.microsoft.com/en-us/sql/t-sql/database-console-commands/dbcc-checkdb-transact-sql?view=sql-server-ver17).

Reports distinguish `no_issues_reported`, `issues_reported`, and `incomplete`.
The first means only that this scoped check returned without reported issues,
not proof of database health. Diagnostic findings are capped at 1,000; caps or
returned warnings mark the report incomplete. Native errors, timeouts and
unexpected result shapes are failed requests, never successful checks. SQL
Server corruption errors use the normal API error channel, not findings rows.
Findings may expose data details and should be treated as database contents.

Checks use ExecuteQuery policy admission, native database privileges, normal
timeouts/cancellation, result limits and typed audit. Active transactions are
rejected. Native diagnostic work can take locks and consume substantial I/O;
it does not request repair, but it is not a zero-impact operation.

## PostgreSQL maintenance

`POST /v1/sessions/{session}/connections/{connection}/maintenance/postgres`
accepts explicit `schema`, `name`, an `action` object and optional `apply: true`.
For example:

```json
{"schema":"public","name":"items","action":{"action":"vacuum","analyze":true}}
```

Actions are `vacuum` (optional `analyze`), `analyze`, `reindex_table` and
`reindex_index` (optional `concurrently`). The SDK exposes `postgres_maintenance`.
Omitting `apply` returns generated SQL only, after connection-policy and active
transaction checks. Apply uses normal ExecuteQuery permissions, native database
privileges, request timeout/cancellation and a typed audited operation.

No database-wide target, VACUUM FULL, arbitrary options, implicit transaction end
or automatic repair is offered. Names are independently quoted and limited to
63 bytes to avoid PostgreSQL name truncation. Restricted SQL policies fail
closed if their parser cannot authorize a command. Preview is not an existence
or native privilege check. Applied means the command returned successfully;
native notices are not guaranteed to be surfaced by every driver, so this is
not proof that every partition/index was processed. Native locks can delay other sessions,
and cancellation does not guarantee undoing maintenance already performed.
Concurrent reindex failure can leave an invalid temporary index: see
[REINDEX](https://www.postgresql.org/docs/current/sql-reindex.html).
[VACUUM](https://www.postgresql.org/docs/current/sql-vacuum.html) and
[ANALYZE](https://www.postgresql.org/docs/current/sql-analyze.html) retain their
native target/partition semantics and privilege requirements.

## Query-performance history

`GET /v1/metadata/history/performance?days=7&profile_id=123` returns hourly
execution counts, error/canceled counts and mean/p95/maximum durations for the
authenticated principal only. `profile_id` is optional; `days` defaults to seven
and must be 1–30. The SDK exposes `query_performance`.

Summaries read existing durable query history without duplicating SQL or
parameter storage and do not delete history. They include at most the newest
10,000 matching executions and explicitly report `truncated` when capped.
Unknown durations contribute to execution counts but not timing statistics;
p95 is nearest-rank over known durations, including failed/canceled executions
when timed. Empty hours are omitted. No SQL, binds, error messages or other
principals' history are returned. This measures Sift-recorded executions, not
all activity on the database. Reads are audited as metadata operations.

## Process alerts

`GET /v1/sessions/{session}/connections/{connection}/processes/alerts` streams
NDJSON `ProcessAlertSample` values. The SDK exposes `watch_process_alerts` as a
bounded byte stream. Each line contains the sample time, observed process count,
an `incomplete` flag and alert transitions. SQL text, usernames and credentials
are excluded. Each poll is an authorized, supervised and audited `ListProcesses`
operation; SQLite process monitoring remains unsupported.

Query parameters: `long_query_seconds` (default 60), `idle_transaction_seconds`
(300), `poll_seconds` (5, allowed 2..300), and `duration_seconds` (3600, allowed
1..3600). A threshold of zero disables that rule; both cannot be disabled.
Thresholds are capped at one day. Clients reconnect after stream expiry and
receive current conditions again. Monitoring runs only while subscribed; this
is not an installation-wide daemon or external notification delivery service.

Active conditions fire once per process/start-time/rule and emit `active: false`
when a complete subsequent sample resolves them. Samples reaching the 500-row
cap are marked incomplete and cannot clear previous alerts. Missing timestamp
or engine visibility does not prove health. Shutdown, connection closure or
sampling failures end the stream.

Idle time starts when the connection becomes idle within its open transaction,
not when its last query started. PostgreSQL uses `xact_start` and `state_change`
from [pg_stat_activity](https://www.postgresql.org/docs/16/monitoring-stats.html).
SQL Server includes sleeping sessions with open transactions and uses
[session request times](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-views/sys-dm-exec-sessions-transact-sql?view=sql-server-ver17)
plus transaction metadata. Native SQL Server timestamps are normalized using
the server's current UTC offset; clock/time-zone changes can affect age estimates.

## Transfer quarantine

CSV imports accept `conflict_policy: "quarantine"` for PostgreSQL, SQL Server
and SQLite. Rejected rows include zero-based logical data-row offsets (header
excluded), sanitized reasons and source values in inferred-column order. JSON
null represents a null source value, not the text `"NULL"`. Multiline CSV fields
remain one logical row. Authorization, connection, cancellation and other fatal
errors stop the import rather than being treated as bad rows.

Upload-to-table recipes publish a seven-day workspace artifact when rows are
rejected. Download its `quarantine_artifact.id` through the existing artifact
endpoint or SDK `transfer_quarantine_report`. Reports contain `version: 1`,
`columns` and `rows`, and have the same 64 MiB bound and authorization as other
artifacts. These contain database data; do not publish them as logs. SQLite
quarantine uses one transaction; PostgreSQL and SQL Server retain the existing
row-at-a-time commit behavior. Parquet quarantine remains unsupported.

## Durable CSV resume

CSV upload-to-table recipes can opt into target-side checkpoints:

```json
{"durable_resume":{"checkpoint_table":"public.sift_transfer_checkpoint","run_id":"8433f24a-2465-4a26-a6f9-cb24012aee08"}}
```

The options belong to one immutable import job. Supply the same source bytes,
recipe revision, actor and destination on retry; the server rejects fingerprint
mismatches, including changed column types. Use a new UUID for a different job.
The target must exist; use abort conflict policy, no type overrides, no manual
`resume_from_row`, and no `create_table`. Dry run validates without creating a
checkpoint table or writing rows. PostgreSQL, SQL Server and SQLite use the
existing supervised transaction APIs.

The explicitly named checkpoint table is created if missing. Each 100-row chunk
commits imported rows and its next-row checkpoint together in the target
database. Failed chunks roll back; committed chunks survive a lost response or
server restart. Reupload the source to resume. Replaying a completed job inserts
zero rows, even when the target lacks unique keys. Permission checks cover both
the target and checkpoint table. Concurrent lock/serialization conflicts can
require another retry.

Checkpoint tables are application-owned state: do not edit, remove, restore
independently, or reuse an unrelated table with that name. Such changes invalidate
replay guarantees. Nontransactional external trigger effects are not covered.
This is an API workflow, not a desktop resume button; quarantine-mode resume and
Parquet resume are not supported.

## Parquet

The query export endpoint accepts `"format":"parquet"`. Transfer recipes accept
`format_id: "parquet"` for query-to-artifact exports and upload-to-table imports.
Both reuse existing session ownership, authorization, supervision and audit.

Exports retain column order, nulls, signed integer widths, floats, booleans and
binary values. Decimal, temporal, UUID, JSON and native values use their exact
text representation rather than guessed precision or timezone conversions.
Empty exports retain their schema. Multiple result sets and duplicate column
names are rejected. Encoded output and cumulative decoded values are capped at
64 MiB; artifacts retain the usual expiry and access controls.

Imports support flat Arrow Boolean, Int16/32/64, Float32/64, Utf8 and Binary
columns. Unsupported types fail before database writes. Imports preserve typed
parameters, including literal `NULL` text versus SQL null and raw binary data.
They can create a table or insert into an existing table, use one transaction,
and roll back on failure. Dry runs decode and validate without creating tables.
Resume, conflict skipping/quarantine, catalog-qualified targets and type
overrides are explicitly unsupported for this Parquet slice. The shared import
report's `columns` field is a coarse preview; it is not an Arrow schema.

## Scheduled Sift state backups

Run `sift-server backup run-policy --policy /absolute/path/policy.json` from an
operator timer. The command checks the interval itself, so missed timer ticks
produce one backup, not a catch-up storm. ADR-039 still applies: **the server
must be stopped**. Arrange an explicit maintenance window in the service
manager; this command never stops or restarts a process for you.

Policy JSON fields:

| Field | Meaning |
| --- | --- |
| `id` | Stable UUID owning this policy's private subdirectory and ledger |
| `directory` | Absolute local recovery directory |
| `key_file` | Absolute private archive encryption-key file, per ADR-039 |
| `interval_seconds` | 60 through 31536000 seconds |
| `keep_last` | 1 through 1000 completed local archives |
| `remote_url_file` | Optional private file containing a complete HTTPS PUT URL |

Only archives recorded in this policy's ledger are eligible for retention.
Failed creation/upload does not prune previous recovery copies. An interrupted
run authenticates and reuses its pending archive. Do not edit the ledger or
change its policy identity to repurpose an existing archive directory.

Remote uploads stream the encrypted ZIP, without its key. The URL file may
contain a presigned object-store PUT URL; generate a fresh unique URL for each
occurrence. A destination supporting path templates may use `{archive}` in the
URL file, replaced with the UUID archive filename. Sift sends `If-None-Match: *`,
does not follow redirects, and imposes a five-minute upload timeout. Destinations
must honor conditional PUT for no-overwrite semantics. Signing credentials and
remote lifecycle retention remain operator-owned; no cloud account is inferred.

If upload succeeds but the process dies before saving its ledger, a retry may
receive an overwrite refusal. Supply a new unique upload URL to finish the
pending occurrence; Sift keeps local recovery copies rather than assuming the
remote object is valid. Remote restore starts by downloading the archive and
using the existing `backup inspect` / `backup restore` commands.

## Metrics and traces

`GET /v1/metrics` is an administrator-only, audited Prometheus text endpoint.
Use the normal authentication and protocol-version headers for the instance.
It reports completed HTTP requests by status class, requests in flight,
time-to-response-header histograms, and dropped OTLP spans. Labels never contain
SQL, request paths, connection names, credentials or user identifiers.

OTLP request-span export is disabled by default. Set `SIFT_LOG__OTLP_ENDPOINT`
in the root `.env`, or `log.otlp_endpoint` in development config. Locked instance
manifests use `server.log.otlp_endpoint`. Supply the **complete traces URL** of
your collector. HTTPS is required except for loopback HTTP; userinfo, query
strings and fragments are rejected.

The exporter uses [OTLP/HTTP JSON](https://opentelemetry.io/docs/specs/otlp/),
with a 256-span queue, batches up to 32 and a five-second request timeout. It is
best effort: queue overflow, collector failures and process exit can lose spans.
No retry or shutdown delivery guarantee is implied. This slice exports HTTP
request method/status/timing spans, not query contents, result streaming duration,
or a full distributed parent/child trace of driver internals.

## PostgreSQL dump and restore

Operator CLI:

```text
sift-server postgres dump --spec /absolute/path/postgres.json --archive /path/new.dump
sift-server postgres restore --spec /absolute/path/target.json --archive /path/new.dump
sift-server postgres restore --spec /absolute/path/target.json --archive /path/new.dump --apply
```

The specification contains `tools_directory` (absolute directory containing
`pg_dump`, `pg_restore` and `psql`), `host`, `port`, `database`, `user`, optional absolute
`password_file` (private libpq pgpass format), optional `ssl_mode` (defaults to
`verify-full`) and `timeout_seconds` (defaults to 3600, maximum 86400). Select
compatible PostgreSQL client tools explicitly. Connection strings, arbitrary
tool flags and inherited libpq configuration are not accepted.

Dump creates a private custom archive and publishes it without overwriting an
existing file. Restore takes a private snapshot of the input (maximum 16 GiB)
and decodes it without executing archive SQL by default. A read-only destination
query verifies database/user identity, an empty target, CREATE privilege and a
writable primary. The target major version must be at least both the archive's
source and dump-tool major versions (10+ release archives only). The archive
table of contents is bounded to 1 MiB. `--apply` restores to the explicitly
named, already-existing database in one transaction, without creating/dropping
the database or replaying ownership/ACLs. A non-empty destination is rejected.
Preflight is not proof of complete compatibility: extension availability,
encoding/collation differences and object-level privileges may still cause a
transactional restore failure. Keep the destination quiescent between preflight
and apply; the check is not a reservation against other administrators. Failure
rolls the transaction back; dump/restore never silently replaces existing data.

Only restore archives from trusted sources: PostgreSQL archives contain
executable SQL. Read the PostgreSQL [dump](https://www.postgresql.org/docs/current/app-pgdump.html)
and [restore](https://www.postgresql.org/docs/current/app-pgrestore.html) security
and compatibility guidance before production recovery. These are logical
single-database archives, not cluster/PITR backups. They are not encrypted by
this command; protect them as database contents.

Commands are audited against configured Sift metadata. Subprocesses have bounded
run time and are killed/reaped on timeout or Ctrl-C. Password bytes never enter
arguments or Sift metadata; raw subprocess output is suppressed because it can
contain credentials or SQL. Reports expose action, apply status, archive size
and validated destination metadata for restores.

## Automated acceptance

- Parquet type/null/empty/malformed tests and a real SQLite export/import,
  dry-run and duplicate-key rollback test.
- Policy due-time, retention, failed-upload checkpoint/recovery and conditional
  HTTP upload tests.
- Metrics encoding, authenticated scrape and local OTLP collector delivery tests.
- Disposable real PostgreSQL dump/restore, dry-run and rollback test:
  `cargo test -p sift-server --lib --features live-pg postgres_backup::tests::real_postgres`.
  It creates its own private cluster; it does not use your configured database.
