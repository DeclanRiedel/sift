# SQL Server error mapping

## Issue

SQL Server driver errors are all collapsed to `Code::DriverInternal`.

## Current proof

- `crates/driver-sqlserver/src/lib.rs` `ms_err` returns `DriverError::new(Code::DriverInternal, e.to_string())` for every `tiberius::error::Error`.
- The Postgres driver has a more granular `pg_err` mapping.

## Failure mode

The server and clients cannot distinguish login failure, syntax error, duplicate object, constraint violation, deadlock, or backend disconnect. Retry behavior and UI messaging become incorrect.

## Changelist

- Match `tiberius::error::Error` variants and token error numbers.
- Map common SQL Server codes: 18456 auth, 208 undefined object, 2627/2601 duplicate key, 2714 duplicate object, 547 constraint violation, 1205 deadlock.
- Preserve raw SQL Server code in `DriverError.engine_code`.
- Add unit tests for representative error-code mapping.
