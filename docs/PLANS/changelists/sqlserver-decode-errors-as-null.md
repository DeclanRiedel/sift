# SQL Server decode errors become NULL

## Issue

SQL Server cell decoding swallows conversion errors and falls back to `Value::Null`, including for native types that metadata maps to non-null primitive types.

## Current proof

- `crates/driver-sqlserver/src/lib.rs` `ms_value` uses `.ok().flatten().map(...).unwrap_or(Value::Null)` for every type.
- `Money`, `DatetimeOffset`, `SmallDateTime`, and `SqlVariant` lack explicit decode arms.
- `ms_type_ref` maps `Money | Money4` to `PrimitiveType::Decimal`, so metadata says decimal while values can render as null.

## Failure mode

Real data can appear as NULL without any warning. This is a data correctness bug, not just incomplete type coverage.

## Changelist

- Return `Value::Engine { display_text: "<decode error>" }` plus warnings for decode failures, matching the Postgres contract.
- Add explicit arms for money, datetimeoffset, smalldatetime, and sql_variant.
- Distinguish actual SQL NULL from decode failure.
- Add live MSSQL tests with non-null values for the missing types.
