# Postgres COPY export discards data

## Issue

`CopyOp::Export` counts bytes from `COPY TO STDOUT` but discards all chunks. `CopyResult` has no data or stream field.

## Current proof

- `crates/driver-postgres/src/lib.rs` folds `copy_out` chunks into a byte count.
- `sift_driver_api::CopyResult` only reports `bytes` and `rows`.

## Failure mode

The API reports a successful export with `N` bytes but gives the caller no exported payload.

## Changelist

- Redesign COPY export as a streaming API returning `Stream<Item = Bytes>`.
- If a synchronous result is still needed, add a bounded `Bytes` payload with a strict max size.
- Add tests that verify exported bytes match table contents.
