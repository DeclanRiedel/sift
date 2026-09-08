# Backend data operations

These features are API/operator workflows; no desktop interaction is required.

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
`pg_dump` and `pg_restore`), `host`, `port`, `database`, `user`, optional absolute
`password_file` (private libpq pgpass format), optional `ssl_mode` (defaults to
`verify-full`) and `timeout_seconds` (defaults to 3600, maximum 86400). Select
compatible PostgreSQL client tools explicitly. Connection strings, arbitrary
tool flags and inherited libpq configuration are not accepted.

Dump creates a private custom archive and publishes it without overwriting an
existing file. Restore takes a private snapshot of the input (maximum 16 GiB)
and decodes it without executing SQL by default. This validates the archive,
not compatibility with the destination. `--apply` restores to the explicitly
named, already-existing database in one transaction, without creating/dropping
the database or replaying ownership/ACLs. Prefer an empty destination. Failure
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
contain credentials or SQL. Reports expose action, apply status and archive size.

## Automated acceptance

- Parquet type/null/empty/malformed tests and a real SQLite export/import,
  dry-run and duplicate-key rollback test.
- Policy due-time, retention, failed-upload checkpoint/recovery and conditional
  HTTP upload tests.
- Metrics encoding, authenticated scrape and local OTLP collector delivery tests.
- Disposable real PostgreSQL dump/restore, dry-run and rollback test:
  `cargo test -p sift-server --lib --features live-pg postgres_backup::tests::real_postgres`.
  It creates its own private cluster; it does not use your configured database.
