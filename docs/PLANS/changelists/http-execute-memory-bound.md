# HTTP execute memory bound

## Issue

Synchronous HTTP execute materializes all rows up to the configured cap before responding, then axum serializes the full JSON response.

## Current proof

- `crates/server/src/session.rs` `drain_stream` accumulates `Vec<Row>` until `Done`.
- `crates/server/src/http.rs` returns the response as `Json(resp)`.
- The current dirty tree improves JSON value size accounting, but the response is still held as rows plus serialized JSON.

## Failure mode

At the default 10k row / 16 MB cap, each concurrent HTTP execute can hold tens of MB during serialization. Many concurrent HTTP executes can pressure memory even when each request is under cap.

## Changelist

- Lower HTTP execute defaults or reserve HTTP for small preview results.
- Pre-reserve `rows` where possible and stream larger responses through the WebSocket or an HTTP streaming body.
- Add a per-session or global concurrent HTTP execute limit.
- Add load tests around max-row and max-byte responses.
