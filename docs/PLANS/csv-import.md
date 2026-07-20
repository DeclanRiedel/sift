# CSV import contract

Status: accepted (ADR-029).

## Surface

`POST /v1/sessions/{session_id}/connections/{connection_id}/import/csv`
accepts a JSON request containing the target table, CSV bytes, header and
delimiter options, a null marker, whether to create the table, and a conflict
policy. The response reports inferred columns, creation status, inserted rows,
and skipped rows.

Imports require a header row. Header names must be non-empty and unique. The
server rejects malformed CSV, ragged records, empty data, and payloads over 64
MiB before touching the database.

## Inference

The server samples at most 1,000 data records. Ignoring the configured null
marker, each column widens monotonically through:

`boolean -> int64 -> decimal -> date -> timestamp_tz -> text`

Mixed incompatible values become text; an all-null column becomes text. The
mapping is `boolean/bigint/numeric/date/timestamptz/text` on Postgres and
`bit/bigint/decimal(38,10)/date/datetimeoffset/nvarchar(max)` on SQL Server.
Inference is returned for preview on every import but is applied only when
`create_table=true`. Existing table types are always authoritative.

## Conflict policy

- `abort` uses Postgres `COPY FROM STDIN CSV` or the SQL Server bulk extension.
  The engine path is transactional and any error aborts the ingest.
- `skip` uses parameterized row inserts. Postgres appends `ON CONFLICT DO
  NOTHING`; SQL Server catches only duplicate-key errors 2601/2627. Other
  errors stop the import. Successfully inserted earlier rows remain committed,
  and the response reports duplicate rows skipped.

`create_table=true` rejects an existing table. The endpoint does not drop or
replace schema.

## Audit and secrets

CSV bytes never enter the operation log. The audited `ImportCsv` operation
contains only session, connection, target table, create flag, and conflict
policy. Driver tracing records byte counts, never payload contents.
