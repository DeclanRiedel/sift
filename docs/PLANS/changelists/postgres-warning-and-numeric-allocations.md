# Postgres warning and NUMERIC allocation pressure

## Issue

Postgres streaming can retain an unbounded warning per decode error, and NUMERIC decoding allocates heavily per cell.

## Current proof

- `crates/driver-postgres/src/stream.rs` pushes every stream or cell decode warning into `Vec<DriverWarning>` until `Page::Done`.
- `crates/driver-postgres/src/decode.rs` `decode_numeric` collects digit groups into a `Vec`, uses `to_string`/`format!` per group, and inserts a leading minus sign into the final string.

## Failure mode

A million-row result with one unsupported/errored column can allocate a million warning strings. Numeric-heavy result sets do avoid correctness bugs but still burn allocations on the per-cell hot path.

## Changelist

- Cap warnings per cursor and emit a final suppressed-count warning.
- Pre-size NUMERIC output and write base-10000 groups without `format!`.
- Avoid `out.insert(0, '-')` by writing the sign first.
- Add decode benchmarks for large numeric result sets.
