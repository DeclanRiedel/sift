# sift — Phase 0 Status

## Project Goal

Build `sift` as a server-first DB IDE substrate. Phase 0 is complete when a
third party can run the headless server, read the versioned API contract, and
build a UI against Postgres and SQL Server without private guidance.

## Implemented

- Versioned protocol crate with stable HTTP/WS serde envelopes and error codes.
- Headless axum server with sessions, connections, auth hook, audit rows, and
  protocol-version response header.
- HTTP v1 surface for health, sessions, connections, schema, execute, cancel,
  transactions, HTTP audit, durable replayable operation log, and generated
  OpenAPI.
- WebSocket streaming with ACK-gated backpressure and SDK E2E proof.
- Rust SDK covering HTTP and WS, including bearer auth propagation.
- Postgres driver with pooled connections, streaming, params, schema,
  transactions, cancel, advisory locks, COPY import/export, LISTEN/NOTIFY, and
  live container tests.
- Postgres `VerifyCa` / `VerifyFull` TLS modes use rustls with native trust
  roots. `VerifyCa` is intentionally strict and performs hostname verification.
- SQL Server driver via `tiberius` with params, streaming, schema, transactions,
  savepoints, cancel-by-abort isolation, close/cancel cleanup, and live
  container tests.
- Postgres binary decoding for numeric/decimal and month-free intervals.

## Verified

- `cargo test -q`
- `cargo clippy --workspace --all-targets -- -D warnings`
- Live Postgres: `13` tests pass with `live-pg`.
- Live SQL Server: `4` tests pass with `live-mssql`.

## Remaining Phase 0 Gaps

- OpenAPI is published with generated protocol schemas.
- Operation audit is replayable from `/v1/operations` and can be persisted as
  JSONL via `SIFT_AUDIT__OPERATION_LOG_PATH`.
- SQL Server cancel uses task abort/drop-connection semantics with session
  cleanup, not TDS ATTENTION.
- SQL Server MARS and bulk insert extension methods are declared but unsupported.
- Postgres `LISTEN/NOTIFY` uses a dedicated listener connection and bounded
  in-process notification delivery; server route/WebSocket fanout remains to
  be exposed if UI needs direct subscription endpoints.
- Month-aware Postgres intervals intentionally surface as engine-native values
  because `chrono::Duration` cannot represent calendar months.
