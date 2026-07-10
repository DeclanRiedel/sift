# DDL routine signatures

## Issue

Postgres routine DDL uses `pg_get_functiondef('<qualified_name>'::regprocedure)`, but `ObjectPath` only carries catalog/schema/name/kind. Overloaded or non-nullary functions require the full `schema.name(argtype, ...)` regprocedure signature, so routine DDL fails or resolves ambiguously.

## Current proof

- `crates/protocol/src/schema.rs` `ObjectPath` has no argument-type field.
- `crates/server/src/ddl.rs` formats only `qualified_name(object, engine)` into the `regprocedure` cast.
- `crates/driver-postgres/src/schema.rs` shallow introspection does not enumerate `pg_proc` routines, so it cannot provide signatures.

## Failure mode

`generate_ddl` for `public.add_one(integer)` emits a lookup for `public.add_one`, which PostgreSQL rejects with invalid regprocedure input or resolves incorrectly when overloads exist.

## Changelist

- Extend the protocol with a routine signature field, preferably `ObjectPath.routine_args: Option<Vec<String>>`.
- Teach Postgres shallow schema introspection to enumerate `pg_proc` routines and populate the argument type list from `pg_get_function_identity_arguments`.
- Update `generate_routine_ddl` to cast `schema.name(args)` for Postgres and keep SQL Server on `OBJECT_ID`.
- Add live PG coverage for a nullary function, one-arg function, and overloaded functions.
