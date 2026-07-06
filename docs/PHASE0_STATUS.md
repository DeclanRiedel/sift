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
  parameterized execute, transactions, SQL Server CSV bulk insert, HTTP audit,
  durable replayable operation log, and generated OpenAPI.
- WebSocket streaming with ACK-gated backpressure and SDK E2E proof.
- WebSocket Postgres notification fanout for `LISTEN/NOTIFY`, with SDK proof.
- Rust SDK covering HTTP and WS, including parameterized execute and bearer
  auth propagation.
- Postgres driver with pooled connections, streaming, params, schema,
  transactions, cancel, advisory locks, COPY import/export, LISTEN/NOTIFY, and
  live container tests.
- Postgres `VerifyCa` / `VerifyFull` TLS modes use rustls with native trust
  roots. `VerifyCa` is intentionally strict and performs hostname verification.
- SQL Server driver via `tiberius` with params, streaming, schema, transactions,
  savepoints, CSV bulk insert, cancel-by-abort isolation, close/cancel cleanup,
  connect timeouts, and live container tests.
- Postgres binary decoding for numeric/decimal and month-free intervals.

## Verified

- `cargo test -q`
- `cargo clippy --workspace --all-targets -- -D warnings`
- Server API smoke: `17` tests pass against `MockDriver`.
- Live Postgres: `13` tests pass with `live-pg`.
- Live SQL Server: `5` tests pass with `live-mssql`.

## Implemented With Known Limits

- OpenAPI is published with generated protocol schemas.
- Operation audit is replayable from `/v1/operations` and can be persisted as
  JSONL via `SIFT_AUDIT__OPERATION_LOG_PATH`.
- SQL Server cancel uses task abort/drop-connection semantics with session
  cleanup, not TDS ATTENTION.
- SQL Server MARS is declared but unsupported; requests with `mars: true` fail
  early instead of silently opening a non-MARS connection.
- SQL Server bulk insert is exposed through the public HTTP API and SDK for
  headered CSV via bounded batched INSERTs; native BCP format is still
  unsupported.
- Postgres `LISTEN/NOTIFY` uses a dedicated listener connection, bounded
  in-process notification delivery, and public WebSocket fanout.
- Month-aware Postgres intervals intentionally surface as engine-native values
  because `chrono::Duration` cannot represent calendar months.

## Remaining Phase 0 Gaps

- No known Phase 0-critical API surface remains unimplemented.
- SQL Server true TDS ATTENTION, MARS concurrency, and native BCP are deferred
  backend-depth improvements, not blockers for the Phase 0 headless API.

## Current Direction After Phase 0

The active post-Phase 0 direction is headless, collaboration-native
infrastructure. Rooms and CRDT-backed documents supersede the earlier
workspace/tab planning model. See `docs/PLANS/headless-collab-infra.md`.
