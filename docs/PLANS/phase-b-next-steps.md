# Phase B Next Steps

Phase A is complete and the follow-up API audit fixes are committed. The next
work should move into Phase B reliability, starting with the pieces that keep a
bad driver call, long query, or host shutdown from breaking the server contract.

## Progress

- [x] 1. Per-query timeout and spawn discipline — commit `47c7db1`.
- [x] 2. Graceful shutdown contract (ADR-018) — commit `f190739`.
- [x] 3. Health and readiness split — commit `2d40aee`.
- [x] 4. Durable operation audit — commit `d20375f`.
- [x] 5. Correlation IDs — commit pending.
- [ ] 6. Connection recovery behavior — next.

Note (step 4 follow-up, done): actor is now captured on both the query path and
metadata admin operations (`push_metadata_operation` threads `principal_id`).

## Order of Work

1. **Per-query timeout and spawn discipline** — done (`47c7db1`).
   - Route HTTP execute, schema, and other synchronous driver calls through a
     bounded spawned task.
   - Wire `config.timeouts.request_secs` into the execution path.
   - On timeout, cancel the active cursor/driver work where possible and return
     `Code::QueryTimedOut`.
   - Add tests proving a wedged or slow driver does not block the handler.

2. **Graceful shutdown contract** — done (`f190739`).
   - Write ADR-018 before implementation.
   - Define the sequence: stop accepting new work, mark readiness false, drain
     in-flight queries until deadline, cancel remaining cursors, flush room
     snapshots, then exit.
   - Implement the drain gate and tests for new-session rejection during drain.

3. **Health and readiness split** — done (`2d40aee`).
   - Keep `/v1/health` as process liveness.
   - Add `/v1/ready` for readiness: metadata reachable, runtime not draining,
     and configured drivers registered.
   - Add OpenAPI coverage and client SDK support.

4. **Durable operation audit** — done.
   - Move operation audit out of memory-only state into metadata storage.
   - Capture actor, target resource, result code, row count where available,
     and failure details that do not leak bind values or secrets.
   - Ensure success and failure paths use one helper so future operations are
     hard to forget.

5. **Correlation IDs** — done.
   - Accept or generate a request correlation ID for HTTP and WebSocket work.
   - Echo it in responses, tracing spans, and audit records.
   - Add protocol fields only where the public wire contract needs them.

6. **Connection recovery behavior**
   - Decide the retry boundary for broken driver connections.
   - Implement one retry on safe reconnectable failures where the operation is
     known idempotent.
   - Surface `Code::ConnectionFailed` only after retry or when retry is unsafe.

## First Task to Start

Start with **per-query timeout and spawn discipline**. It is narrow, directly
testable, and satisfies the repository rule that a wedged driver cannot freeze
the server. It also gives graceful shutdown a concrete drain/cancel mechanism
to build on instead of designing shutdown around unbounded work.

## Done Criteria for the First Task

- `config.timeouts.request_secs` is used by the server.
- HTTP execute and schema calls cannot run driver work inline in the handler.
- Slow or wedged mock driver tests return `Code::QueryTimedOut` within the
  configured deadline.
- Cancellation is attempted after timeout, and SQL Server's discard-on-cancel
  rule remains intact.
- `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace` pass.
