# Phase B Next Steps

Phase A is complete and the follow-up API audit fixes are committed. The next
work should move into Phase B reliability, starting with the pieces that keep a
bad driver call, long query, or host shutdown from breaking the server contract.

## Progress

- [x] 1. Per-query timeout and spawn discipline — commit `47c7db1`.
- [x] 2. Graceful shutdown contract (ADR-018) — commit `f190739`.
- [x] 3. Health and readiness split — commit `2d40aee`.
- [x] 4. Durable operation audit — commit `d20375f`.
- [x] 5. Correlation IDs — commit `f5e1df1`.
- [x] 6. Connection recovery behavior — commit `70ed1d6`.

All six reliability steps in this list are complete.

### Additional Phase B polish (beyond the six-step list)

- [x] Driver isolation — ADR-013 written; both engines meet the containment
      boundary (`a7b117f`).
- [x] Protocol version negotiation — ADR-016 written and enforced (`cf07da5`).
- [x] Correlation ID stamped into error responses (`ee4a205`).
- [x] Performance: durable audit writes moved off the request path to a
      background writer thread (`77dbd5d`).
- [x] Metadata admin operations capture the actor (`f183cdc`).

Still open in the broader Phase B backlog (`server-build-list-v2.md`):
secret backends (OS keychain), and JSONL replay-log redaction (the durable
audit is sanitized; the opt-in replay log still serializes full requests —
an ADR-009 replay-vs-audit tension, deliberately deferred).

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

6. **Connection recovery behavior** — done.
   - Decide the retry boundary for broken driver connections.
   - Implement one retry on safe reconnectable failures where the operation is
     known idempotent.
   - Surface `Code::ConnectionFailed` only after retry or when retry is unsafe.

   Retry boundary (implemented): only `Code::ConnectionFailed` is treated as
   reconnectable, and only the idempotent reads `ping`/`schema` are retried —
   once — after re-establishing the connection from the stored spec. Mutating
   work (execute, bulk insert, transactions, savepoints) is never auto-retried
   because a reconnect cannot know whether the first attempt's side effects
   already landed; those surface the error directly.

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
