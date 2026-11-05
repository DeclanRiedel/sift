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

## Active Benchmark and Profile progress

`GET /v1/sessions/{session}/connections/{connection}/benchmark/{run_id}/progress`
and the corresponding `/profile/{run_id}/progress` endpoint return an optional
`PerformanceProgress`. The SDK exposes `performance_progress` with an explicit
Benchmark/Profile mode. These reads dispatch no work to the connected database
and use `no-store`.

Snapshots contain exact run identity, stage, monotonic server elapsed time and
cancellation-requested state. Benchmarks include requested and finished warm-up
and measured attempt counts and the last outcome. Failed/cancelled attempts can
count as finished; these counters are not a latency summary. Profile reports
setup, instrumentation and cleanup stages without an inferred percentage.
Progress elapsed time includes setup/cleanup and must not be compared with
benchmark sample clocks or native plan timings.

The existing session-owner boundary and current managed-profile policy apply.
Progress requires `read_performance_progress`, the original `benchmark_query`,
`benchmark_pair` or `profile_query`, and `execute_query`; active reads also recheck the frozen
SQL against current schema/read-only restrictions. Explicit operation allowlists
must include the new read action to expose progress. No SQL, parameters, result
rows or native diagnostics are returned or audited by this action.

`POST /v1/sessions/{session}/connections/{connection}/benchmark/pair` executes a
confirmed `BenchmarkPairRequest` under audited `benchmark_pair`. Both frozen
workloads require current ordinary execution permission at admission, each
attempt and progress reads. Limits count warmups/iterations per population;
progress counts aggregate A+B attempts. One root run UUID uses the existing
benchmark progress/cancel paths, one dedicated connection and one total budget.
Alternating rounds reset at the measured phase; randomized_v1 uses a reproducible
SplitMix64 round coin and records its seed and actual attempt trace. First failure,
timeout, cancellation or authority loss stops both populations. Fully drained rows
are discarded; reports retain no bind values. Pair context survives private saves
and JSON/text artifacts. Incomplete pairs withhold deltas even when one population
locally finished. Five-second cleanup observation never aborts native close;
connection quota and tunnels remain owned until cleanup settles. See ADR-144.

The registry retains only active jobs. Null means inactive, not registered yet,
or the wrong run UUID/mode; it does not prove success, cancellation or failure.
The original request supplies the authoritative result. The Vim panel polls at
most twice per second while awaiting that request, with a two-second progress
read timeout and one acknowledged UI update in flight. Progress errors do not
replace workload results. Run/source checks discard late or stale updates, and
updates do not move focus. Definition reruns show the same progress in their
review dialog. Completed sample lists remain virtualized; progress retains no
sample or result-row backlog.

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

The desktop lists the newest 100 unexpired quarantine reports in a workspace.
The metadata list requires room read access and the audited `ReadTransferRecipe`
operation; it contains no artifact bytes. Select a report to open the same
viewer used for a new import. Download remains the only content endpoint and
checks the active instance and workspace. Reports expire after seven days, and
the server may reject a download that expires after listing. New reports have a
dedicated content type so the history cannot mistake arbitrary JSON exports
for rejected rows; older generic-JSON reports remain available by ID or from
their original import result. The history does not offer a general artifact
browser, pagination past 100, or pinning.

## Private benchmark source context

`save_benchmark_run` optionally accepts `source_context`, retained immutably
beside the saved report. The thin SDK uses the same optional typed field. Legacy
requests and saved payloads default to no context. Context is client-reported
metadata, not execution authority, a server attestation, workspace access or
proof of database/data equivalence. Portable measurement artifacts keep their
existing format and omit context; imported artifacts cannot recreate it.

For workspace-bound SQL documents, the desktop captures the instance ID,
workspace ID/revision and relative document path before workload review. Cached
Git observations retain their own timestamp/revision, head, branch, dirty or
unknown state and truncation flag. A truncated empty status never claims clean.
Reusable-definition runs retain the same frozen source context. Changing the
workspace during review, execution or saving does not replace it; scratch tabs
and unavailable workspace snapshots have no inferred source context.

In the Vim saved-run browser, `[o] Include source context` is off by default.
Review the displayed capture and cached-Git timestamps before enabling it, then
save normally. The server validates bounded identifiers, normalized relative
paths, Git hashes and printable labels. Context collects no native root paths,
remote URLs, diffs, file contents or bind values. It lives only in the encrypted
owner-private snapshot payload, within existing 4-MiB snapshot/64-MiB owner
quotas. SQLite indexes, audit records, list summaries and AI/MCP measurement
inspection omit it. Notes/tags updates preserve it; explicit private saved JSON
copy includes it. Saved-run retention follows the owner policy below; optional
private Profile attachments follow the same snapshot lifetime.

## Private benchmark Profile attachments

`save_benchmark_run` optionally accepts `profile`: capture time, exact SQL,
parameter count and a typed Profile response. SQL, engine and parameter count
must match the benchmark, with a separate Profile run ID. It is explicitly
caller-associated evidence, not server provenance, execution authority or proof
of equivalent data/cache conditions. Native Profile clocks never enter ordinary
benchmark samples. SQLite retains an estimated plan beside a measured read;
node runtime counters and native clocks remain unavailable.

Raw plan text is bounded to 1 MiB, the complete attachment to 2 MiB, normalized
trees to 4096 nodes/64 levels, and attribute traversal to 65536 values/64 levels.
Negative/non-finite native clocks and counters are refused. Existing 4-MiB
snapshot and 64-MiB owner quotas still apply. Legacy snapshots have no attachment.
Plans are encrypted beside the snapshot; notes/tag updates preserve them and
retention/deletion removes them together. No raw plan enters SQLite or audit.

The desktop offers a Profile only after confirmed Benchmark/Profile executions
with matching frozen SQL, values, server/profile and exact connection generation.
Ephemeral salted fingerprints are never persisted. In the Vim save browser,
`f` toggles private Profile inclusion, off by default; the offered capture is
frozen when the save dialog opens. Raw plans can expose names, literals and bind
values despite omission of an explicit bind list. Inspect the displayed capture
identity and privacy notice before opting in.

On an opened saved run, `v` displays its attached Profile through the existing
Performance Plan view in an open query tab. It executes no SQL and refuses to
replace an active Benchmark/Profile job. Server/tenant changes refuse the action.
The view labels historical caller-associated evidence and keeps native and
Sift-observed clocks separate. Private snapshot JSON copy includes plans and
says so; portable measurement artifacts, numeric exports, list summaries and
AI/MCP measurement projections omit them. Baseline reuse remains measurement-only.

## Private saved Profile plan comparison

Pin an opened saved run as A (`a`), open B, and press `t` to compare their
attached Profile plans. The bounded comparison runs on the background executor;
Escape cancels pending presentation. The view virtualizes normalized node rows;
`j`/`k` selects a row, Enter toggles bounded selected-node JSON, `y` copies private
comparison JSON, and Escape returns to the saved-run browser. Changed requests,
source or records invalidate a pending result. Changing server/tenant refuses
navigation/copy; refreshed records clear the cached comparison. No workload or
network action is dispatched by the local comparison.

CLI `sift performance compare --baseline-id <uuid> --candidate-id <uuid> --plans`
adds a separate `plans` result after the existing two authorized private reads.
Regression limits, if supplied, still apply only to ordinary benchmark medians.
Without `--plans`, output keeps its existing measurement-only shape; AI/MCP
measurement views do not gain plan access.

Both valid attachments must have the same engine and estimated/analyzed state.
Within each paired parent, unique sibling `(operator, relation)` keys use ordered
LCS alignment. Unmatched gaps are replacements/additions/removals. Duplicate keys
or the global one-million-cell alignment budget use visibly labelled positional
comparison. Moves can appear as removal/addition. Each input retains the existing
4096-node/64-level bounds; output never exceeds the sum of input nodes. Paths are
structural addresses; correspondence is heuristic, not semantic identity.

Rows retain A/B operators, relations, reported counters, child counts and native
attributes. Missing counters remain null/unavailable, distinct from zero. Costs
and node times are never summed, and loop/rebind semantics are not normalized.
Capture IDs/times, native clocks and Sift clocks stay separate. SQL/environment
and benchmark-compatibility differences remain warnings, not causal regression
claims. No raw-plan, query-SQL or bind-list field is copied into comparison JSON.
Native identifiers and attributes may still contain sensitive SQL/literals;
copy/output is private and explicit. Untimed result equivalence remains separate.

## Private saved-benchmark retention

Retention defaults to disabled. Each tenant member can configure only their own
saved-run policy. Set an optional maximum age (1–36500 days since the original
server save timestamp), an optional count (1–500 newest saves), or both. Empty
limits disable automatic removal. Counts order by save timestamp and then stable
snapshot ID. Either limit can remove a snapshot, including its notes, tags and
source context; annotation edits do not reset age. Existing 4-MiB snapshot and
64-MiB active-owner quotas remain.

The thin SDK exposes policy read, preview and update methods. HTTP paths are
`GET /v1/metadata/tenants/{tenant}/benchmark-retention`,
`POST /v1/metadata/tenants/{tenant}/benchmark-retention/preview` and
`PUT /v1/metadata/tenants/{tenant}/benchmark-retention`. Preview takes the desired
policy and expected revision and returns current removal count/bytes, a server
cutoff and a five-minute review digest. Update requires explicit confirmation
and that exact revision, policy, cutoff and digest. Changed library rows,
annotations or policy revisions return conflict. The digest fences state
changes; authentication and current membership remain authoritative.

In the disconnected Vim saved-run browser, `p` opens retention. `i` edits age,
Tab switches fields, and Escape returns to commands. `v` or field Enter previews;
`x` explicitly activates the reviewed policy and cleanup. `r` reloads the current
policy while preserving draft values; `z` resets the draft to stored values.
Edits invalidate review. Conflicts preserve drafts, and changing server/tenant
refuses dispatch. Close note/tag drafts before opening retention.

Activation removes reviewed entries atomically with the policy update. Future
saves enforce the policy in their insert transaction before checking hard quotas;
a full retained library can accept a newer run. A server worker visits up to 32
enabled owner policies per minute in fair order; large installations may need
multiple cycles. Embedded server users must call the metadata maintenance API
if they need expiry between saves. Revocation deletes the owner's policy;
rejoining does not silently resume it. Logical removals have typed persisted
prune audits containing identifiers/counts, never snapshot text or handles.

Deleted or superseded payload handles enter the durable secret-cleanup queue.
The shared cleanup worker guards current references and retries backend failures,
including abandoned saves and annotation writes that lose to expiry. Logical
removal can precede encrypted-payload deletion. Backup copies have their own
lifecycle. Full backups and same-ID tenant recovery preserve policies and original
save timestamps: enabled policies can expire restored old snapshots when the
server starts. Retention is not an archival or secure-erasure guarantee.

## Tabular recipe column selection

CSV, XLSX and Parquet upload-to-table recipes with `format_version: "2"`
accept ordered selection:

```json
{"column_projection":[{"source":"customer_name","target":"name"},{"source":"id","target":"id"},{"source":"id","target":"external_id"}]}
```

Only listed fields survive, in array order. Repeat a source to copy its value to
several distinct destinations. Sources match exact, unambiguous source column names;
unselected duplicate headers can be dropped. Targets must be distinct, nonempty,
trimmed, NUL-free names of at most 128 bytes. A projection has 1–256 outputs;
source names are limited to 4096 bytes. Input and rewritten output each have a
64 MiB limit, enforced during output generation. All original records must have
consistent width, including fields that will be dropped.

The existing `column_mappings` object renames headers while retaining every
column and its original order; CSV retains version-1 compatibility. XLSX and
Parquet require version 2 for either option. They cannot be combined. Exports
and other formats reject column selection. No expressions or value conversions are
provided. Existing provider type validation and `NULL` semantics still apply.

Edit options in the Vim recipe editor's multiline JSON field (Shift+Enter inserts
a line), and choose import version 2 for projection. The recipe commands include
`i` name, `f` format, `e` version, `o` options, `t` target, `w` sheet, `m` direction,
`c` create-table, and `s` save; Escape returns from a field to commands.
Dry-run preview, inference, type choices, apply and quarantine for CSV-backed
imports all use the projected columns. Dropped values
never enter those previews or quarantine reports.

XLSX requires a selected sheet and retains text-cell-to-CSV conversion, including
CSV null and type rules. Archive parts, XML nodes, cell coordinates and generated
CSV are bounded; conversion runs off the async runtime. Its durable resume is
unsupported; existing stateless CSV-style resume remains available.

Parquet projects source roots before decoding: omitted nested fields do not need
to be supported. Selected fields retain native scalar types, nullability, and
NULL versus empty text/binary values; repeated fields copy those values. Encoded
input, original source metadata and decoded copy expansion retain 64 MiB bounds.
Parquet rejects type overrides, resume and non-abort conflict policies. Its
transactional dry-run/apply workflow uses the projected column names and order.
SQLite refuses output names that collide under ASCII identifier folding for all
three import formats. The direct column-review grid remains CSV-only; XLSX and
Parquet selection is edited in recipe JSON.

The desktop CSV review also edits the projection directly: `h`/`l` selects an
output, `H`/`L` moves it, `d` drops it, `c` copies it and `R` resets the original
columns. `i` edits the target name; `t` edits a CREATE TABLE or native type.
Escape returns to commands and Tab moves between fields. The sample grid shows
only selected outputs in their final order. Generated CREATE TABLE queries and
SQLite native rows use the same selection. Saving a recipe chooses version 2
and exact original source headers; an ambiguous selected header must be fixed
before saving. Run the recipe with the original file, not the locally mapped CSV.

Desktop review accepts at most 256 source/output columns, 4096-byte source
headers, 128-byte target names, and 64 MiB original and mapped files. Samples
contain at most 20 rows with 256-byte value prefixes; inference samples 200 rows.
Full-file projection and native conversion run in the background. Native SQLite
retains its tighter 128-column, 10,000-row and 8-MiB typed-payload limits.
Mapping/type edits invalidate prior dry-run or native review. Imports bind the
original server, tenant, profile, session and connection; reopening the file is
required after reconnecting. Cancellation during preparation dispatches no
import. Once a request starts, local cancellation cannot guarantee that already
committed rows were rolled back; inspect the original target before retrying.
Expressions, other-format mapping and wider desktop source files remain pending.

Durable projection imports also fingerprint the original upload. Changing even
a dropped field returns HTTP 409 on resume; use a new run UUID for changed input
or a changed recipe. Legacy rename-only checkpoint identity is unchanged.

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
The Vim CSV import workflow retains the checkpoint table and run UUID in its
recipe; retry with the same recipe and source to resume. Quarantine-mode resume
and Parquet resume are not supported.

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

## SQL Server backup and restore

`POST /v1/sessions/{session}/connections/{connection}/recovery/sql-server` and
SDK `sql_server_recovery` require a SQL Server connection to `master` with
`VIEW ANY DATABASE` plus native backup/restore privileges. Both actions default
to preview when `apply` is omitted:

```json
{"action":"backup","database":"app","archive_path":"/var/opt/mssql/backup/app.bak"}
```

Backup uses COPY_ONLY and CHECKSUM and **appends** a backup set if the media file
already exists (`NOINIT`, `NOSKIP`). It never formats or overwrites existing
backup sets. Preview checks the source and returns SQL without creating a file;
it does not prove the server can write the selected directory.

```json
{"action":"restore","database":"app_recovered","archive_path":"/var/opt/mssql/backup/app.bak","backup_set":1,"moves":[{"logical_name":"app","destination":"/var/opt/mssql/data/app_recovered.mdf"},{"logical_name":"app_log","destination":"/var/opt/mssql/data/app_recovered.ldf"}]}
```

Restore preview reads HEADERONLY and FILELISTONLY and runs VERIFYONLY with the
explicit MOVE destinations. It requires an undamaged full backup with checksums,
a compatible server major version, an absent target with a name distinct from
the archive source (server-collation comparison), and exactly one destination
for every data/log file. No system databases, partial/log/PITR restore, FILESTREAM,
automatic single-user mode, database deletion or `REPLACE` is offered. The native
distinct-name/no-REPLACE safeguard also prevents overwriting a database created
between preflight and apply.

Paths are absolute **SQL Server-side** paths, not paths on the Sift client/server.
Keep the archive immutable and destinations quiescent during preview/apply;
preview is not a filesystem reservation or proof of complete data integrity.
Cancellation/failure can leave a new database in RESTORING state or a partial
backup set. Inspect it explicitly; Sift never automatically deletes the new
database, files or media. Backups are not encrypted by this command.

Each action is audited and uses ExecuteQuery policy admission and the configured
request timeout/cancellation. Read-only or schema-restricted policies fail closed
when they cannot authorize native commands. See Microsoft's
[BACKUP](https://learn.microsoft.com/en-us/sql/t-sql/statements/backup-transact-sql?view=sql-server-ver17)
and [RESTORE safeguards and VERIFYONLY/MOVE](https://learn.microsoft.com/en-us/sql/t-sql/statements/restore-statements-arguments-transact-sql?view=sql-server-ver17).

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

## Tenant-selective Sift recovery

Stop the server and select the original tenant ID from a trusted encrypted
Sift-state archive. Both commands require the offline maintenance lock:

```text
sift-server backup restore-tenant --archive /path/state.sift-backup --key-file /path/archive.key --tenant-id 2
sift-server backup restore-tenant --archive /path/state.sift-backup --key-file /path/archive.key --tenant-id 2 --apply
```

Preview builds and validates a private staged merge, then removes it. Its only
destination writes are audit records; it does not change tenant data or secrets.
Apply replaces that tenant's subtree and reports row counts and an encrypted
pre-restore rescue archive. The existing durable restore journal installs
secrets before metadata and rolls back incomplete installation.

This is same-installation, same-ID recovery, not cross-instance migration:
installation identities must match (including both absent), schemas must match,
and all referenced principals must already have matching IDs/external identities.
An existing tenant must have the same name and kind. Unknown tables, ID conflicts,
cross-tenant ownership references, selected extension storage and pending approvals
for selected members refuse recovery. Existing unrelated tenants, principal/auth
state, global API tokens, instance manifests and append-only ledgers are preserved.
Selected tenant invitations and tenant-scoped API tokens are revoked.

Restored definitions include rooms/documents, workspaces/checkpoints, queries,
connection profiles/credentials, vaults, DDL, run history and transfer recipes.
Schedules and filesystem projections are disabled; repository network access and
credentials are cleared. Artifacts and projection reconciliation state are not
restored. Interrupted work becomes terminal; no connected database or checkout
is modified. Review these definitions before explicitly re-enabling execution.

Portable file-secret recovery copies only selected connection/vault values to
new opaque handles in a staged copy of the destination secret store. Destination
authentication keys and unrelated credentials remain intact. Old unreferenced
secrets are retained, not garbage-collected. At most 10,000 distinct selected
secret handles (1,024 bytes each) are accepted. Memory-secret mode requires no
selected secret references; keychain-backed selective recovery is unsupported.
This command does not provide failover, cross-instance identity remapping or
external database disaster-recovery orchestration.

## Automated acceptance

- Parquet type/null/empty/malformed tests and a real SQLite export/import,
  dry-run and duplicate-key rollback test.
- Policy due-time, retention, failed-upload checkpoint/recovery and conditional
  HTTP upload tests.
- Metrics encoding, authenticated scrape and local OTLP collector delivery tests.
- Tenant recovery: two-tenant encrypted preview/apply with credential remapping,
  preserved global authentication/unrelated data, scoped-token revocation,
  checkpoint blobs/ID high-water marks and identity/ID/FK/schema refusal.
- Disposable real PostgreSQL dump/restore, dry-run and rollback test:
  `cargo test -p sift-server --lib --features live-pg postgres_backup::tests::real_postgres`.
  It creates its own private cluster; it does not use your configured database.
- Disposable SQL Server backup/append/new-name restore, integrity and resume:
  `cargo test -p sift-server --lib --features live-mssql sql_server_recovery`.
  Requires Docker and the locally installed SQL Server 2022 image. It creates
  and removes a container with no host volumes; no configured database is used.


### AI chat lifecycle

Chat history remains until owner/admin deletion unless a tenant retention
preference or instance ceiling applies. `GET /v1/ai/tenants/{id}/retention`
reports the stored preference, instance maximum, and effective expiry. Tenant
admins use `PUT` with `{"retention_days": 30}` to opt in, or `null` to clear the
preference; a configured instance ceiling still applies. Values must be within
1–36500 days and cannot exceed the instance maximum. Scoped tokens remain
restricted to their tenant. Policy updates are audited, including failures.

`server.ai.max_retention_days` is omitted by default. Maintenance runs every
`server.ai.cleanup_interval_secs` (default 300) with
`server.ai.cleanup_batch_size` (default 100, maximum 1000). Expiry uses last
chat activity and skips active runs. Turns beyond `server.ai.max_run_secs`
become interrupted and receive a durable stopped event. System expiry and
interruption are audited without copying chat bodies. Deletion transactionally
queues opaque handles; cleanup retries failures, survives tenant/room deletion,
and preserves any handle still referenced by restored metadata. The operation
audit and database change ledger keep independent retention.

Tenant admins can rotate keys through
`POST /v1/ai/tenants/{id}/content-key/rotate`. Partial rotations resume the same
new generation; existing chat handles stay stable. AI-bearing backups require
portable file secrets and include authenticated encrypted bodies. See
[AI recovery](PLANS/state-backup-restore.md#ai-content-recovery-adr-100).


Room owners review `GET /v1/ai/rooms/{id}/publication/preview` before posting
`CreateAiRoomPublicationRequest` to `/v1/ai/rooms/{id}/publication`. The request
pins the preview's profile and scope digest; row publication defaults to false.
Members can read the current grant with `GET`; owners revoke future database
reads with `DELETE`. Public tools use the room's bound shared credential
connection and still require the initiating member's room/profile/vault access.
Changing the binding, credentials, configuration or policy requires a fresh
review and a new turn. Published messages remain room-readable after revocation.
Restored publication grants are revoked until an owner reviews them again.

## PostgreSQL retained deadlock history

`GET /v1/sessions/{session}/connections/{connection}/deadlocks` and Monitor →
History can read an operator-supplied PostgreSQL JSON log. SQL Server continues
using its native `system_health` history. PostgreSQL file ingestion is disabled
by default. The operator must enable PostgreSQL `jsonlog` and make its chosen
regular log file readable by the Sift server account. Sift does not configure
PostgreSQL logging, ship remote logs, or manage rotation.

Register sources in the locked instance manifest under
`server.drivers.postgres_deadlock_logs`, or under `drivers.postgres_deadlock_logs`
in development server configuration. Each entry requires `tenant_id`,
`profile_id`, `host`, `database`, and an absolute server-local `path`. `port`
must match the profile's explicit port; omit it only when the profile omits it.
Use actual managed profile IDs and connection values. At most 32 sources are
allowed, with one source per tenant/profile. The corresponding optional local
env setting is `SIFT_DRIVERS__POSTGRES_DEADLOCK_LOGS`, listed in `.env.example`.
HTTP clients never supply or receive log paths.

The caller still needs current tenant/vault/profile access and an upstream
PostgreSQL superuser or effective `pg_read_all_stats` membership. Ad-hoc and
schema-restricted connections are refused. The current database must match the
registered database; log records from other databases are ignored. Authority
is checked again after file IO, including profile-policy changes and revocation.

Each read examines at most the final 4 MiB, skips incomplete/malformed lines and
lines over 64 KiB, and returns at most the last 50 matching deadlock summaries,
ordered newest first. Only SQLSTATE `40P01` records are selected. Timestamps
must use UTC or an explicit numeric offset. Responses contain the timestamp,
victim and participant process IDs, and whitelisted numeric transaction,
relation, virtual-transaction or tuple resources and known lock modes. SQL,
arbitrary DETAIL, context, usernames and raw logs are never returned or stored
in SQLite. Unrecognized/localized wait descriptions yield an incomplete
participant list rather than exposing their text. Wait duration is unavailable.

Two blocking workers bound file IO; each waiter has a five-second timeout.
Timed-out work retains its admission permit until IO ends. Source errors omit
paths and underlying filesystem messages. This is bounded retained-file
inspection, not a durable deadlock archive or a complete log search facility.

Native acceptance uses the existing opt-in provider feature and a disposable
socket-only cluster, without touching configured databases:

```sh
cargo test --workspace --features sift-server/live-pg --test postgres_deadlock_live
```

## SQLite connected-main restore

The existing `POST /v1/sessions/{session}/connections/{connection}/maintenance/sqlite`
workflow accepts `{"action":{"action":"restore","path":"backups/source.db"}}`.
Use the SDK `sqlite_maintenance` method with a managed writable profile. The
backup must be an existing SQLite file in the connected database's configured
root; absolute/cross-root paths, symlinks, hardlinks, protected sources,
self-restore and schema-restricted profiles are refused.

Preview validates the backup and returns a one-use token without replacing
data. Review the root, connected main target and backup path: apply replaces
all main schema/data. Apply repeats the same action with `apply`,
`confirm_write` and `backup_verified` true plus the returned `preview_token`.
The backup acknowledgment means a separate current pre-restore backup has been
verified. Target and backup main/WAL identities must still match. Source
verification uses a pinned read-only snapshot; an unfinished native copy rolls
back. Preflight bounds each side’s main/WAL inputs to 1 GiB; copying has
cancellation and deadline checks. Native page-size/journal-mode incompatibilities fail rather than
being converted automatically. A request timeout does not prove no commit.

The Vim SQLite maintenance view selects Restore with `s`, uses a relative
backup path, shows the overwrite warning and requires the exact phrase
`RESTORE root/target FROM backup`. It refreshes schema state after reported
success. Deletion, cross-root restore and native Windows/macOS acceptance
remain separate work (ADR-125).

## SQL Server Agent and configuration inspection

`GET /v1/sessions/{session}/connections/{connection}/agent/jobs` is an audited,
read-only Agent snapshot. Non-sysadmins see only owned jobs, even when their
native Agent Reader/Operator role permits broader visibility. SQLAgentUserRole,
ReaderRole and OperatorRole can use the role-visible catalog and native history
procedure without extra SELECT grants on underlying Agent tables. Sysadmins
retain cross-owner inspection. No job is started, stopped or edited.

Responses contain at most 100 jobs and a truncation flag. History means the
latest whole-job (`step_id=0`) record, never the latest individual step. Missing
history stays absent; times are server-local, and duration is seconds, including
runs longer than an hour. Restricted readers inspect local history; remote-target
history can remain unavailable. Steps, commands, descriptions, history messages
and operator metadata are not returned to Sift.

`GET /v1/sessions/{session}/connections/{connection}/settings/sqlserver` returns
at most 200 numeric configuration entries plus truncation, preserving configured
and effective values separately. It casts native sql_variant numbers before
transport and does not change configuration. Native permission failures return
`permission_required` with empty items; other failures remain errors. Permission
needs vary by server version and explicit grants/denies, so Sift follows native
authority rather than inferring permission solely from a version string.

Both reads use a disposable connection cloned from the authorized source, with
one additional tenant connection reservation. Private session setup ignores
editor ROWCOUNT and leaves editor database/session state intact. Existing query
limits, supervision and timeouts apply; source authority is rechecked before
returning data. Read-only connection profiles are supported; schema-restricted
profiles are refused because these reads cross schema/database boundaries.
Responses use `Cache-Control: no-store` and existing operation audit records.

[Native SQL Server acceptance](PLANS/sqlserver-inspection-acceptance.md) covers
roles, revocation, local whole-job history, truncation, editor-state preservation,
redaction, configured/effective values and explicit catalog denial/recovery.

Inspection connection setup preserves typed session variables and replays only
supported literal SET startup statements: ROWCOUNT, LOCK_TIMEOUT, DATEFIRST,
DEADLOCK_PRIORITY and the listed boolean session options (NOCOUNT, ANSI_NULLS,
ANSI_PADDING, ANSI_WARNINGS, ARITHABORT, ARITHIGNORE, CONCAT_NULL_YIELDS_NULL,
QUOTED_IDENTIFIER, NUMERIC_ROUNDABORT and XACT_ABORT). Empty entries are ignored.
Arbitrary batches, expressions, impersonation and unknown setup are refused
before another connection opens; they are never silently discarded. Use typed
session variables for configured context. A source with a write-bearing startup
batch remains usable for its original explicitly opened connection, but these
read-only inspection endpoints will not replay it.


## SQL Server database-user review

The existing SQL Server security preview/apply API and SDK accept four user
actions: `create_database_user` (`name`, `login`), `remap_database_user` (`name`,
`login`), `set_user_default_schema` (`name`, `schema`) and `drop_database_user`
(`name`). They use the same explicit confirmation and current-state precondition
as other security changes. The principal report also includes nullable
`mapped_login` and `default_schema`; null login does not prove an orphan when
metadata visibility is limited.

These actions support existing SQL-authenticated logins and ordinary SQL users.
Create explicitly chooses dbo as initial default schema. Remap repairs a
login-orphaned instance user or transfers its mapping to another existing login,
preserving user name, principal ID, schema and rights. WITHOUT LOGIN users can
change schema or be dropped, but cannot be remapped. Built-ins and other
identity-provider/credential families are refused. Names use native quoting;
SQL login-mapped user names cannot contain a backslash.

Native CREATE USER/ALTER ANY USER, CONTROL, ALTER-on-user/self and ALTER ANY USER
rights apply to create, remap, default-schema change and drop respectively.
Create/remap/drop also require complete database security metadata without
metadata DENYs. Preview binds native database/actor/principal/login/schema
identity and direct security state, and apply rechecks current permissions and
Sift policy. Changed native SIDs, context, login enablement or affected security
rows invalidate review. A concurrent change after recheck can still cause a
native refusal; preview does not reserve locks.

Drop affects only the database user; it preserves the login and refuses known
owned securables. No cascade, automatic ownership transfer or object deletion
is attempted. SQL Server validates additional ownership families. Remap does
not enable a disabled login, reset credentials or invalidate existing session
tokens. Review warns about unexpanded inherited/fixed-role rights.

In Vim Monitor → Security, `a` opens Edit security, Esc leaves text editing,
`m` cycles actions, `i` returns to fields, and Enter previews. Review the exact
SQL/warning, then Enter confirms. Tab navigates fields. Source changes invalidate
the form and reviewed apply; no login/password fields are added.

SQL Server security inspection and edits require a database-wide Sift connection
scope; a schema-only profile, including one scoped to sys, cannot authorize
these database-wide actions. A read-only Sift profile rejects Apply before
catalog work or SQL parsing. Native permissions remain independently required.


## PostgreSQL explicit role memberships

PostgreSQL 16+ provides `GET .../postgres/role-memberships` on the session/connection
API. Pages default to 100 rows, cap at 200, and allow offsets through 10000. Rows
show role, member, named grantor and ADMIN/INHERIT/SET flags. Each grantor's edge
is separate; this is not a complete effective privilege matrix.

Use typed `grant_role_membership` or `revoke_role_membership` actions through the
existing PostgreSQL object preview/apply endpoints. Grants require role, member,
grantor and all three option booleans; revoke requires the three names and uses
RESTRICT. Preview checks current identities and authority, captures at most 200
relevant native edges, and returns exact SQL, warning and precondition. Apply
requires both confirmation flags and revalidates the observations. Native errors
for permissions, cycles or dependent grants remain visible. Revalidation does
not lock catalogs against later concurrent changes. Passwords and role settings
are neither read nor edited.

In Vim, use the command palette's **Open PostgreSQL Role Memberships** command,
or select Memberships in Monitor. `j/k` selects, `n/p` pages, `r` refreshes and `a`
opens the selected membership's form. Edit role/member/grantor with Tab between
fields. Escape returns to commands; `m` switches grant/revoke, `i` edits, and
`1/2/3` toggles ADMIN/INHERIT/SET for grants. Enter in a field or `p` previews.
Review the SQL and cluster-wide effect, then `x` explicitly confirms apply;
Escape cancels. Other grantors may preserve effective membership after revocation.
Older PostgreSQL versions report unsupported membership options without changing
the existing Roles view. Native acceptance currently covers PostgreSQL 17.10 on
Linux; other versions and native Windows/macOS are not claimed as tested.


## Late connection opening and cleanup

When a connection finishes opening after a request timeout, caller cancellation,
or session closure, the server retains the unregistered handle and closes it.
Pending opens and late cleanup share a 256-slot admission bound. This is separate
from established-connection and tenant limits. Tenant connection quota and any
managed tunnel remain owned until registration transfers them or native cleanup
finishes. New opens can report pool exhaustion while these slots remain occupied.

A five-second cleanup observation deadline logs a pending state; it does not
restart cleanup, abort the native close, or release its retained admission.
Native close failure is logged without secret bytes. This handles returned late
results; providers remain responsible for their own partial-open cleanup before
they return a handle. Runtime teardown is the final fallback if cleanup can no
longer be scheduled. Existing request timeouts and provider supervision remain.

## Reviewed SQL Server generated-column imports

CSV uploads and saved CSV/XLSX/Parquet imports can opt into native reviewed
column modes on an existing ordinary SQL Server table. Set recipe option
`sql_server_mode` to `{"identity":"generate","computed":"omit"}` (or use
`identity: "reject" | "preserve"`, `computed: "reject"`). Each execution also
supplies matching `sql_server.mode` and a fresh UUID in `sql_server.run_id`.
First execute with `dry_run: true`; inspect `sql_server_review.imported_columns`,
`omitted_columns`, native database/schema/table and warning. Apply with the same
input and modes and `sql_server.confirmation` containing the returned
`precondition` and `confirmed: true`. Confirmations belong to execution requests,
never saved recipe options. Source, native metadata, actor/authority, data,
parsing rules and recipe changes require a fresh review.

Generate omits identity input; preserve requires explicit identity input and
native ALTER authority. Computed omit lets SQL Server calculate values. Native
INSERT authority is required. Generated-always, hidden, column-set and rowversion
inputs are refused. The workflow requires at least one writable input, atomic
Abort policy, no resume, no type overrides and no target creation. CSV retains
its null rules; Parquet keeps typed values. Unsupported native scalar types and
textual binary conversions are refused.

Writes use one transaction and bounded parameterized batches on a disposable
connection with normalized strict settings. Arbitrary startup SQL is refused;
editor ROWCOUNT, IDENTITY_INSERT and other session settings are not inherited.
Identity counters and trigger/external effects can survive rollback. Accepted
input counts do not prove final target population or trigger equivalence.

`GET /v1/sessions/{session}/connections/{connection}/imports/{run}/progress`
returns an active snapshot or null. Its attempted and confirmed committed counts
are separate. `POST` to the same run's `/cancel` returns `cancel_requested`;
false means that exact active run was not found, and true acknowledges a request,
not proven rollback. Both operations are audited and bound to the original
source. At most four owned runs/cleanup tasks and one active run per source are
admitted. Native calls and late opens that outlive a waiter also remain under
the shared bounded driver-task/opening admission, separately from coordinator
slots. Cleanup can outlive its five-second observer; resource admission is
retained until native close finishes. Source closure cancels its imports.

Cancellation or timeout during COMMIT, lost native acknowledgement and supervisor
failure return `import_outcome_unknown` (HTTP 409); the change ledger retains
`outcome_unknown` with no confirmed row count. Inspect the target before retrying.
A successful report's `cleanup_pending` signals that native close is still pending.

In the Vim CSV review, `g` enables these modes, `1` cycles identity choices, `2`
changes computed handling, `v` reviews and `x` explicitly applies. Changing a mode
invalidates review. Escape requests cancellation while retaining the original
request and its terminal result. Saved upload recipes use their normal preview
and execute controls, show imported/omitted columns, and poll stage/counters with
at most one acknowledged update in flight. Windows/macOS native acceptance is
deferred; Linux-host SQL Server evidence is recorded with the implementation.

## PostgreSQL public role lifecycle

PostgreSQL object preview/apply accepts `create_role_with_attributes`,
`rename_role`, `alter_role_attributes` and `drop_role`, in addition to the existing
`create_role` NOLOGIN action. New actions require PostgreSQL 16+. Public patches
use optional `login`, `inherit`, `create_database`, `create_role`, `superuser`,
`replication`, `bypass_rls`, `connection_limit` and `valid_until` fields. Omitted
values keep existing attributes on update or native defaults on create. Limits
accept -1 (unlimited) or a nonnegative integer. Validity is
`{"kind":"infinite"}` or `{"kind":"until","at":"2030-01-01T00:00:00Z"}`;
password values/hashes are not accepted. Role catalog rows include public detail.

Use the normal object preview, inspect exact SQL/impact, then apply with both
`confirmed` and `production_confirmed` and the current precondition. Tokens bind
source, database/version, native actor/target identities, attributes and bounded
membership/dependency state. Changes require a fresh preview. Review refuses more
than 200 memberships or shared dependency addresses; native PostgreSQL can still
reject concurrent changes and DROP dependencies. The tool does not issue
ownership reassignment, DROP OWNED or cascades.

Roles have cluster scope. Native CREATEROLE/ADMIN and privileged-attribute rules
are checked; the database remains authoritative. Existing sessions are not
terminated by NOLOGIN or connection limits. Password expiry does not control all
authentication methods. INHERIT changes defaults for new memberships; use the
membership editor for existing INHERIT/SET options. Rename can clear an MD5
password, which the preview discloses without inspecting hashes. Current/session
actors must be renamed or dropped from another connection.

In Vim Monitor → Roles, `a` opens the form; Escape returns from a field to command
keys, `m` cycles create/rename/attribute/drop, and `1`–`7` cycle public flags through
keep/on/off. Tab visits optional limit/expiry fields. `p` previews; `x` explicitly
confirms the reviewed action. Source changes invalidate drafts/reviews. Credential
management, role session defaults and dependency remediation remain separate work.
