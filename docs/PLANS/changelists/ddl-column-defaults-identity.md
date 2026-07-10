# DDL column defaults and identity

## Issue

Column defaults, generated expressions, collation, and Postgres identity metadata are not represented in `ColumnMetadata`, and the DDL formatter ignores even the existing `auto_increment`/facet signals. Table DDL silently loses important column behavior.

## Current proof

- `crates/protocol/src/column.rs` has no `default_expr`, generated expression, or collation field.
- `crates/driver-postgres/src/schema.rs` queries `default_expr` and identity, but only derives `auto_increment`; it stores `facets: Default::default()`.
- `crates/server/src/ddl.rs` emits only `name type` and `NOT NULL`.
- SQL Server reports default constraints as table constraints, not column defaults, which changes replay shape.

## Failure mode

`created_at timestamptz DEFAULT now()` round-trips as `created_at timestamptz`; `id bigint GENERATED ALWAYS AS IDENTITY` round-trips without identity behavior.

## Changelist

- Add protocol fields for `default_expr`, generated expression, identity mode, and collation.
- Populate Postgres fields from `pg_attrdef`, `attidentity`, `attgenerated`, and collation catalog joins.
- Populate SQL Server fields from `sys.default_constraints`, computed columns, identity columns, and column collation.
- Render these fields in `format_table_ddl` in engine-native order.
- Extend `ddl_round_trip.rs` to include defaults, identity, generated columns, and non-default collation.
