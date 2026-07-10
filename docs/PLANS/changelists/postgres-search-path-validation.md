# Postgres search_path validation

## Issue

Postgres `search_path` entries are interpolated into startup `options` without identifier validation or quoting.

## Current proof

- `crates/driver-postgres/src/lib.rs` formats `-c search_path={}` with `search_path.join(",")`.
- `crates/driver-postgres/src/conn.rs` does the same for pooled connections.
- `PgConnectionSpec.search_path` is user/config input.

## Failure mode

A schema name containing spaces or option-like text can inject additional libpq startup flags, changing GUCs such as `statement_timeout` for the session.

## Changelist

- Validate each `search_path` entry as a safe identifier or quoted identifier before formatting options.
- Prefer a post-connect `SET search_path` using quoted identifiers if startup options cannot quote safely.
- Add unit tests for spaces, quotes, commas, and option-looking schema names.
