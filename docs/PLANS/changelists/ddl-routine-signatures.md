# DDL routine signatures

## Status

Implemented in the working tree after checkpoint `ef60127`.

## Issue

Postgres routine DDL used `pg_get_functiondef('<qualified_name>'::regprocedure)`, but `ObjectPath` only carried catalog/schema/name/kind. Overloaded or non-nullary functions require the full `schema.name(argtype, ...)` regprocedure signature, so routine DDL failed or resolved ambiguously.

## Original proof

- `crates/protocol/src/schema.rs` `ObjectPath` had no argument-type field.
- `crates/server/src/ddl.rs` formatted only `qualified_name(object, engine)` into the `regprocedure` cast.
- `crates/driver-postgres/src/schema.rs` shallow introspection did not enumerate `pg_proc` routines, so it could not provide signatures.

## Failure mode

`generate_ddl` for `public.add_one(integer)` emits a lookup for `public.add_one`, which PostgreSQL rejects with invalid regprocedure input or resolves incorrectly when overloads exist.

## Changelist

- Added `routine_args: Option<Vec<String>>` to `ObjectPath` and `ObjectInfo`.
- Taught Postgres shallow schema introspection to enumerate `pg_proc` routines and populate input argument type names.
- Updated `generate_routine_ddl` to cast `schema.name(args)` for Postgres and keep SQL Server on `OBJECT_ID`.
- Added live PG coverage for a nullary function, one-arg function, and overloaded functions.
