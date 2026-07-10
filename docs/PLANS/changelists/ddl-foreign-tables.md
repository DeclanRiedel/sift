# DDL foreign tables

## Issue

`ObjectKind::ForeignTable` is routed through the normal table formatter, so the emitted DDL starts with `CREATE TABLE` and omits `SERVER` and `OPTIONS`.

## Current proof

- `crates/server/src/ddl.rs` matches `ObjectKind::Table | ObjectKind::PartitionedTable | ObjectKind::ForeignTable` into `generate_table_ddl`.
- `format_table_ddl` always writes `CREATE TABLE`.
- `crates/driver-postgres/src/schema.rs` shallow introspection maps PG relkind `f` to `ObjectKind::ForeignTable`.

## Failure mode

Exporting DDL for a foreign table produces a local-table definition that is semantically wrong and often not replayable against the same database.

## Changelist

- Split `ForeignTable` out of the table branch.
- For Postgres, generate from `pg_foreign_table`, `pg_foreign_server`, and `pg_options_to_table`; use `pg_get_foreign_table_ddl` only if the minimum supported PG version guarantees it.
- For SQL Server, return `UnsupportedForEngine` unless a real equivalent is added.
- Add a live PG round-trip fixture with `postgres_fdw` or a narrow unit test over mocked catalog rows.
