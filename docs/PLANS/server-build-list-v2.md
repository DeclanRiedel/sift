# Server-Side Build List — Everything Before The GUI

> Status: **code-grounded work-management checklist.** Every "done" claim
> below cites `file:line` evidence verified against the current source;
> every "open" item reflects a real gap verified against the code. This is
> the single ordered backlog for all server-side work that must land before
> the product GUI.
>
> Companion to `docs/DECISIONS.md` (ADRs) and `docs/legacy/ZED_LESSONS.md`
> (rationale for stolen ideas). Items marked `[x]` are verified-present in
> code; `[ ]` are verified-absent or stubbed.
>
> Format: `- [status] [Design|Implement] <area>: <goal>` plus a code
> citation when `[x]`. **Design** = lock a decision (ADR/crate/contract);
> **Implement** = build against a locked design.

## Notable audit findings

Reading the code directly surfaced several corrections worth keeping in
view while prioritizing:

- Several Phase A items that older snapshots listed as open are in fact
  done: PG `NUMERIC`/`INTERVAL` decode, PG TLS (rustls), all `PgExt`
  methods (`listen`/`unlisten`/`copy`/`advisory_lock`/`unlock`/`savepoint`/
  `rollback_to`/`release_savepoint`), and the SQL Server live test harness.
- The `doc` crate is **not a real CRDT** (UTF-8 byte buffer + apply-op) —
  still open, and the first Phase G deliverable. *(Closed in Phase B: per-query
  timeouts now consume `config.timeouts.request_secs`; operation audit records
  the failure path via one helper, not a hard-coded `Succeeded`; correlation
  ids flow end to end; driver-isolation `catch_unwind` covers SQL Server;
  loopback-bypass checks the peer address; CI runs the live driver tests
  against PG and MSSQL service containers.)*
- Three metadata tables (`principal_key`, `keypair_challenge`,
  `saved_query`) are created by migrations but never read or written by any
  Rust code — dead schema.

## What is already in place (verified by code)

- **Workspace** (`Cargo.toml`): `protocol`, `core` (empty placeholder,
  `crates/core/src/lib.rs` is 3 comment-only lines), `driver-api`
  (+`MockDriver` behind `mock` feature), `driver-postgres`,
  `driver-sqlserver`, `metadata`, `doc`, `server`, `client-sdk`.
- **`sift-protocol` is pure serde** (`crates/protocol/Cargo.toml:9-16`):
  deps are `serde`, `serde_json`, `schemars`, `serde_bytes`, `thiserror`,
  `chrono`, `uuid`. No tokio, no I/O. ADR-003/004 honored.
- **Driver trait** (`crates/driver-api/src/lib.rs:138-193`): 8 core verbs
  + `PgExt`/`MssqlExt` ext traits, object-safe (`&self` everywhere,
  `async_trait`), `ConnHandle` newtype, `ResultSetStream`/`Page` multi-result
  model, `TypeRef::Primitive`/`Engine` escape hatch. `as_pg()`/`as_mssql()`
  default-downcast to `None`.
- **Postgres impl** (`crates/driver-postgres/src/`): complete.
  - All 8 verbs + every `PgExt` method is a real impl (`lib.rs:143-303`).
  - Deep schema: columns/PK/indexes (with `amname` + partial predicates)/
    constraints (PK/FK/UNIQUE/CHECK/Exclusion + FK target table)/triggers
    (timing + events decoded from `tgtype`) — `schema.rs:98-445`.
  - `NUMERIC` → `Value::Decimal` and `INTERVAL` → `Value::Interval` (month-
    aware falls through to `Value::Engine`) — `decode.rs:54-156`, unit-tested.
  - TLS: `tokio-postgres-rustls` with native certs wired for `VerifyCa`/
    `VerifyFull` in both `pool_for` and the LISTEN path — `conn.rs:126-131`,
    `274-294`, `lib.rs:163-177`.
  - Pool-per-spec caching (deadpool, key = canonical serde JSON,
    `max_size: 8`) — `conn.rs:89-139`.
  - Cancel via `CancelToken` map (`conn.rs:33`, drained on close);
    panic isolation via `AssertUnwindSafe::catch_unwind` (`stream.rs:79-94`);
    channel buffer = 1 (`stream.rs:45`).
  - DML `affected_rows` reported via `simple_query` `CommandComplete`.
  - Live test harness: 14 tests covering open/ping, type decode, DML count,
    shallow/deep schema, filter pushdown, tx commit/rollback, advisory lock,
    COPY round trip, LISTEN/NOTIFY, cancel, close-mid-query —
    `tests/live_pg.rs`, gated behind `live-pg` feature.
- **SQL Server impl** (`crates/driver-sqlserver/src/lib.rs`): 1124 LOC,
  mostly real.
  - All 8 core verbs implemented; `use_database`, `savepoint`/`rollback_to`,
    CSV `bulk_insert` are real (`lib.rs:275-341`, `809-873`).
  - Deep schema: columns (with PK/identity/max_length/collation facets),
    indexes (unique/PK/columns), constraints (PK/FK/UNIQUE/CHECK) —
    `lib.rs:410-685`. Shallow lists tables + views only.
  - Live test harness: 5 tests — `tests/live_mssql.rs`, gated behind
    `live-mssql` feature.
- **Server bootstrap** (`crates/server/src/`): axum + figment (TOML/env,
  `SIFT_` prefix) + tracing + driver registry + `SessionStore` +
  `RoomRuntime`. `main.rs:57-60` wires `axum::serve` with
  `with_graceful_shutdown(shutdown_signal())`.
- **HTTP surface** (`crates/server/src/http.rs:63-164`): 39 routes —
  sessions, connections (incl. `from-profile`), queries, schema, cancel,
  bulk-insert, transactions (begin/commit/rollback), auth tokens
  (list/issue/revoke), metadata (tenants/rooms/members/documents/
  connections/credentials/history), health, audit, operations, openapi.json.
- **WebSocket surfaces**:
  - Session WS (`http.rs:2151`): `Execute`/`Listen`/`Cancel`/`Ack`
    inbound; `Started`/`Page`/`Notification`/`Error` outbound.
    Page-by-page ack backpressure (`stream_pages_with_ack` `:2504`);
    one active stream per socket.
  - Room WS (`http.rs:2169`): `Attach`/`Detach`/`PresencePing`/
    `DocumentOperation` inbound; `Attached`/`Presence`/
    `DocumentOperation`/`QueryResult`/`Error` outbound. Broadcast
    channel per room (`room_runtime.rs`).
- **Auth** (`crates/server/src/http.rs:282`): bearer-token middleware,
  loopback-bypass flag, metadata API tokens (Argon2-stored, format
  `sift_<lookup>_<uuid>`, minted at `metadata/src/lib.rs:225-263`, verified
  Argon2 at `:265-299`), tenant scoping via `ensure_tenant` (`http.rs:454`).
  Room RBAC: `RoomPermission::{Read,Write,Admin}` mapped from
  owner/editor/viewer (`http.rs:466-504`).
- **Metadata** (`crates/metadata/src/`): SQLite + refinery embedded
  (`lib.rs:21-23`), run on startup (`lib.rs:80`, `:90`). 9 migrations.
  Tables: `tenant`, `principal`, `membership`, `api_token`, `principal_key`,
  `keypair_challenge`, `connection_profile`, `connection_credential`,
  `room`, `room_member` (owner/editor/viewer), `document` (opaque `BLOB`),
  `room_attachment`, `query_history`, `saved_query`. Secrets kept out of
  SQLite **structurally** — password `.take()`n before `serde_json` in
  `upsert_connection_profile` (`lib.rs:360-447`); only opaque handles
  stored.
- **SecretStore trait** (`crates/metadata/src/secrets/mod.rs`):
  `put`/`get`/`delete`, with three backends — `MemorySecretStore`,
  `FileSecretStore` (ChaCha20-Poly1305, keyfile-derived key), and
  `OsKeychainSecretStore` (keyring, `os-keychain` feature). Selected by
  `metadata.secret_backend`.
- **Rooms runtime** (`crates/server/src/room_runtime.rs`): in-memory
  presence + document-op broadcast + snapshot persistence (full-snapshot
  `UPDATE document SET crdt_state = ?`, no op-log). Room-scoped query-result
  summary events published from the HTTP `execute_query` path
  (`http.rs:1731-1738`).
- **Document crate** (`crates/doc/src/lib.rs`): `CrdtKind::{Loro,Automerge}`
  tag + `TextDocument` over opaque bytes. **Apply-op abstraction, not a
  real CRDT** — `apply()` deserializes bytes as UTF-8, mutates a `String`,
  writes bytes back (`lib.rs:79-98`). No op-log, no merge, no pluggable
  backend.
- **OpenAPI** (`http.rs:655-978`): hand-authored JSON literal built from
  `schemars::schema_for!` per protocol type. Served at `GET /v1/openapi.json`
  with an `x-sift-protocol-version` field.
- **Version header** (`http.rs:153-160`): `x-sift-protocol-version` emitted
  on every response with value `PROTOCOL_VERSION = "1"` (`protocol/src/lib.rs:12`).
- **Client SDK** (`crates/client-sdk/src/lib.rs`): `reqwest` + `tokio-tungstenite`;
  broad HTTP coverage (sessions/connections/queries/tx/bulk/cancel/schema/
  metadata/auth/audit) plus one-shot WS helpers (`stream_query`,
  `listen_notifications`, `apply_room_text_operation`).
- **CI** (`.github/workflows/ci.yml`): `cargo fmt --check`, `cargo clippy
  --workspace --all-targets -- -D warnings`, `cargo test --workspace`,
  `cargo deny check`. All run inside `nix develop`.

---

## Phase A — Driver & type completeness

Status: **complete.** The two-implementation validation gate is closed and
the `Driver` trait is locked by ADR-017. Remaining SQL Server limitations are
either deliberate unsupported states in the public contract or later-phase
performance/product work, not Phase A blockers.

- [x] [Implement] driver-postgres: decode `Type::NUMERIC` → `Value::Decimal`
      and `Type::INTERVAL` → `Value::Interval` — `decode.rs:54-156`, unit-
      tested at `:314-347`. *(Month-aware intervals intentionally fall
      through to `Value::Engine`; documented in the variant.)*
- [x] [Implement] driver-postgres: `tokio-postgres-rustls` + `rustls` in
      `pool_for` and the LISTEN path so `VerifyCa`/`VerifyFull` verify —
      `conn.rs:126-131`, `:274-294`, `lib.rs:163-177`.
- [x] [Implement] driver-postgres `PgExt::listen`/`unlisten` —
      `lib.rs:144-186` (dedicated connection per LISTEN, notification pump,
      `mpsc::Sender<PgNotification>`). Tested at `tests/live_pg.rs:515`.
- [x] [Implement] driver-postgres `PgExt::copy` — `lib.rs:188-224`
      (`copy_out`/`copy_in` byte streams through `CopyResult`). Tested at
      `tests/live_pg.rs:470`.
- [x] [Implement] driver-postgres `PgExt::advisory_lock`/`advisory_unlock`
      — `lib.rs:226-272` (Int32+Int32 and Int64 key forms). Tested at
      `tests/live_pg.rs:455`.
- [x] [Implement] driver-postgres `PgExt::savepoint`/`rollback_to`/
      `release_savepoint` — `lib.rs:274-303`.
- [x] [Implement] driver-sqlserver `MssqlExt::use_database`, `savepoint`,
      `rollback_to`, CSV `bulk_insert` — `lib.rs:275-341`, `:809-873`.
- [x] [Implement] driver-sqlserver: live test harness (`live-mssql` feature)
      — `tests/live_mssql.rs` (5 tests: open/ping/execute/close, bulk CSV,
      cancel long query, close mid query, schema_deep + transactions).
- [x] [Design] ADR-017 graduation: lock the driver-trait shape (core 8 verbs
      + fat structs + ext traits + union types) now that two real impls
      exist; define the change-gate rule (signature change ⇒ protocol bump).
      ADR-017 is now written in `docs/DECISIONS.md:151-218` and gates
      locked signature/handle/value/protocol-shape changes on an ADR update
      plus protocol-version bump.
- [x] [Design] TLS strategy: separate (a) server-side TLS termination of
      sift's own HTTP/WS from (b) driver-side TLS to user DBs. ADR-017
      documents this boundary (`docs/DECISIONS.md:184-190`): PG driver-side
      TLS maps `SslMode` through rustls/native roots; SQL Server uses
      tiberius TDS encryption plus `TrustServerCertificate`; HTTP/WS TLS is
      a server deployment concern.
- [x] [Design] Numeric/Decimal representation: the implicit choice is
      `Value::Decimal(String)` + hand-rolled PG wire decode
      (`decode.rs:64-136`) and `tiberius::numeric::Decimal` → `String` for
      SQL Server. Graduated in ADR-017 (`docs/DECISIONS.md:174-178`).
- [x] [Design] Interval representation: the implicit choice is
      `Value::Interval(chrono::Duration)` for month-free intervals and
      `Value::Engine` for month-aware (`decode.rs:138-156`). SQL Server has
      no analogue. Graduated in ADR-017 (`docs/DECISIONS.md:178-183`).
- [x] [Design] SQL Server parity audit: enumerate every `MssqlExt` method
      and every core verb against tiberius capabilities. ADR-017 locks core
      verbs + `use_database`/CSV `bulk_insert`/savepoints as supported,
      keeps MARS as a rejected connection-time setting
      (`driver-sqlserver/src/lib.rs:106-113`), removes the runtime
      `set_mars` extension method, and keeps native bulk out of Phase A
      because the locked `BulkOp` is CSV bytes only
      (`driver-api/src/lib.rs:303-307`). The public `native` bulk request is
      rejected explicitly in the server (`server/src/session.rs:349-361`),
      with a regression test (`server/tests/api_smoke.rs:337`).
- [x] [Implement] driver-sqlserver: deep schema parity with PG —
      triggers now populated from `sys.triggers` + `sys.trigger_events`
      (`lib.rs::mssql_triggers`), shallow scope now enumerates
      tables/views/procs/scalar+TVF/synonyms/sequences via `sys.objects`
      (`lib.rs::mssql_object_kind_from_sys`), and index kinds map
      CLUSTERED/NONCLUSTERED → `Btree` and hash → `Hash`
      (`lib.rs::mssql_index_kind_from_sys`). DML `affected_rows` reported
      for pure DML via `ExecuteResult::total()`. Unit tests cover the
      three mapping functions; live-mssql coverage runs in CI's
      `live-drivers` job.
- [x] [Implement] driver-sqlserver: cancel containment. TDS attention is not
      exposed by tiberius as a safe cross-task public API, so ADR-017 locks
      SQL Server cancel as task abort plus connection discard
      (`docs/DECISIONS.md:200-204`). The driver aborts the cursor task
      (`driver-sqlserver/src/lib.rs:268-276`) and the server removes the
      SQL Server connection after cancel (`server/src/session.rs:330-344`)
      so the orphaned backend session cannot be reused.
- [x] [Implement] driver-sqlserver: panic isolation via `catch_unwind`
      around the spawned `run_query` future — `lib.rs:230-260`. On panic,
      emits `Page::Error { Code::DriverInternal }` (engine-tagged
      SqlServer), removes the cursor entry, and drops the (consumed)
      connection. Parity with PG's `stream.rs:79-94`.
- [x] [Design] driver-sqlserver pooling boundary: pooling is not part of the
      Phase A trait signature. ADR-017 documents that PG may satisfy
      `open()` from a cached pool while SQL Server currently owns one backend
      session per handle (`docs/DECISIONS.md:206-209`); pool warmth and
      preconnect remain Phase C performance work, not a trait-lock blocker.
- [x] [Implement] driver-sqlserver: drop runtime `set_mars` and internal
      `BulkFormat::Native` from the locked driver API. `MssqlExt` now only
      exposes `use_database`, CSV `bulk_insert`, `savepoint`, and
      `rollback_to` (`driver-api/src/lib.rs:232-244`); `BulkOp` is CSV
      bytes only (`driver-api/src/lib.rs:303-307`). Protocol-level
      `BulkInsertFormat::Native` remains accepted by serde but is rejected
      before driver dispatch (`server/src/session.rs:349-361`).
- [x] [Implement] tracing: `#[tracing::instrument(skip_all, fields(...))]`
      on every `Driver` + ext method across both engines
      (`driver-postgres/src/lib.rs`, `driver-sqlserver/src/lib.rs`). Fields
      pin the engine tag plus `conn`, `tx`, `cursor`, or op-specific
      identifiers so a span carries a stable lookup key without the risk
      of leaking bind values or secrets. Params (`req`, `spec`, bulk
      `data`) are dropped from the span via `skip_all`.
- [x] [Design] `ConnHandle` weak backref: decide whether to restore the
      spec's `Weak<dyn Driver>` backref or formally drop it from ADR-017.
      ADR-017 formally keeps `ConnHandle` as opaque id + engine tag without
      a driver backref (`docs/DECISIONS.md:169-173`), matching the code
      (`driver-api/src/lib.rs:38-63`).
- [x] [Implement] Protocol `Operation::Savepoint`/`RollbackToSavepoint`/
      `ReleaseSavepoint` variants + server routing via `as_pg()`/`as_mssql()`
      downcast — `protocol/src/operation.rs:53-64`,
      `protocol/src/session.rs:113-122` (`SavepointRequest`), server methods
      at `session.rs:430-544`, HTTP routes at
      `http.rs:141-152` (three POSTs under
      `/v1/sessions/:id/transactions/:tx_id/savepoints{,/rollback,/release}`).
      `ReleaseSavepoint` returns `Code::UnsupportedForEngine` on SQL Server
      (no `MssqlExt::release_savepoint`). Regression test:
      `savepoint_routes_dispatch_to_ext_traits`.
- [x] [Implement] CI runs the live driver tests — new `live-drivers` job
      in `.github/workflows/ci.yml` spins up `postgres:16` and
      `mcr.microsoft.com/mssql/server:2022-latest` as service containers,
      exports the `SIFT_{PG,MSSQL}_*` env, waits for SQL Server on port
      1433, then runs `cargo test -p sift-driver-postgres --features
      live-pg` and `cargo test -p sift-driver-sqlserver --features
      live-mssql`. Runs alongside the existing `rust` job on every push.

## Phase B — Server reliability layer

Status: **complete.** "Demo works" → "I would put real data in this." Every
item below is landed and covered by tests (`cargo test --workspace` green;
`clippy --workspace --all-targets -D warnings` clean). The former safety holes
— per-query timeout, unbounded HTTP result, bind-value/secret leaks in audit,
timing-oracle auth, no readiness signal, no graceful drain — are all closed.

- [x] [Design] ADR-018: graceful-shutdown contract — written in
      `docs/DECISIONS.md`. Sequence: signal → stop accepting new work → mark
      readiness false → drain in-flight queries to a deadline → cancel/close →
      exit. (Explicit pool close / straggler-cursor cancel documented as
      deferred; per-query timeout is the backstop.)
- [x] [Design] ADR-013 graduation: lock driver-isolation policy. Written.
      Both engines now satisfy the three-layer containment boundary
      (spawn+timeout dispatch, `catch_unwind` on the streaming path, no
      reusable connection after cancel). SQL Server gained its `catch_unwind`
      wrapper; the abort+discard cancel is accepted per ADR-017. Server-side
      spawn+timeout is the primary containment (see per-query timeout step).
- [x] [Design] Reconnect logic: done. Retry boundary is one retry on
      `Code::ConnectionFailed` for idempotent reads (ping/schema) only;
      mutating work never auto-retries. See `phase-b-next-steps.md` step 6.
- [x] [Design] Health vs readiness split: done. `/v1/ready` added; checks
      not-draining + drivers registered + (enabled) metadata reachable.
- [x] [Design] Audit granularity: done. A durable `operation_audit` SQLite
      table now carries actor, target, result_code, row_count, correlation_id,
      and error_message; the failure path is recorded; success and failure go
      through one helper. See the operation-level audit implement item below.
- [x] [Design] Secret backends: done. `metadata.secret_backend` selects
      `memory` | `file` | `keychain`. `file` is an encrypted (ChaCha20-Poly1305)
      store keyed from `metadata.secret_key_file`; `keychain` is the OS
      credential store (pure-Rust keyring, `os-keychain` feature). Vault stays
      deferred to the hosted phase.
- [x] [Design] API versioning policy (ADR-016): written. Monotonic integer
      version, breaking-vs-additive rules, pin-or-proceed negotiation via the
      `x-sift-protocol-version` header.
- [x] [Design] Per-query timeout: done. `config.timeouts.request_secs` is
      consumed; all synchronous driver calls run on a spawned task bounded by
      the deadline and surface `Code::QueryTimedOut`.
- [x] [Implement] Graceful shutdown handler: drain gate that blocks new
      sessions/connections during drain and awaits in-flight queries with a
      deadline (`shutdown_drain_secs`). Explicit pool close / straggler-cursor
      cancel is deferred per ADR-018; room CRDT state is persisted per-op so
      no separate flush is needed.
- [x] [Implement] Health + readiness endpoints; `/v1/ready` returns 503
      while draining or if no driver registered; probes metadata.
- [x] [Implement] Reconnect: one-shot transparent re-establish for idempotent
      reads (ping/schema) on `ConnectionFailed`, engine-agnostic. A
      `Reused`/`Reopened` distinction in the `ping` result is not exposed yet.
- [x] [Implement] Operation-level audit: durable `operation_audit` SQLite
      table with actor/target/result_code/row_count/correlation_id and the
      recorded failure path, written through one helper
      (`push_operation_full`); exposed at `GET /v1/operations/audit`.
- [x] [Implement] OS-keychain `SecretStore` backend: done. `keyring` 3 under
      the `os-keychain` feature (pure-Rust zbus Secret Service on Linux, no
      system libdbus); binary `set_secret`/`get_secret`. Secret bytes are never
      logged. Compile-verified; gated off by default (needs a runtime
      credential service) with an `#[ignore]`d round-trip test.
- [x] [Implement] Audit redaction + query fingerprinting: done. Operations are
      sanitized before storage on every surface — SQL becomes a `sqlfp:` hash,
      execute params cleared, connection passwords redacted, bulk payloads
      dropped. `query_history` keeps raw SQL by default; `metadata.store_sql =
      false` fingerprints it too. ADR-009 updated.
- [x] [Implement] Correlation IDs: accept-or-generate `x-correlation-id`,
      carried on a task-local, echoed in the response header, error body,
      tracing span, and durable audit rows (HTTP and WebSocket).
- [x] [Implement] Protocol version negotiation: inbound
      `x-sift-protocol-version` is read in middleware; a mismatch returns
      `400 unsupported_protocol_version` naming requested vs supported. Absent
      header proceeds (unpinned). Chose 400 over 451 as the standard fit.
- [x] [Implement] Per-query `tokio::spawn` + `timeout` on the HTTP
      `execute_query` path (parity with the WS path's spawned pump).
      Wire `config.timeouts.request_secs` or remove the dead config.
- [x] [Implement] Loopback bypass must check the peer address —
      `http.rs:162-192` (middleware injects trusted peer IP from
      `ConnectInfo<SocketAddr>` into an internal `x-sift-peer-addr` header,
      stripping any client-supplied value); `http.rs:353-357` gates the
      bypass on `IpAddr::is_loopback`. `main.rs:56-59` binds via
      `into_make_service_with_connect_info::<SocketAddr>`. Regression test:
      `tests/api_smoke.rs::loopback_bypass_rejects_non_loopback_peer`
      covers loopback allow, remote deny, and header-spoof deny.
- [x] [Implement] Constant-time bearer-token comparison: done. The static
      bearer path hashes both sides (SHA-256) and compares with a
      difference-accumulating loop, so neither length nor content leaks via
      timing.
- [x] [Implement] Result-byte / row-count cap: done. `drain_stream` enforces
      both a row cap and a total-bytes cap (`config.limits`, defaults 10k rows
      / 16 MiB); exceeding either returns `ResultTooLarge`.

## Phase C — Performance & snappiness

Goal: the differentiator vs Navicat-class tools. Caches, prefetch, pool
warmth, progressive indexing.

Status: **complete.** All shipped items in this phase pass tests
against MockDriver; live-integration tests against real PG and MSSQL
service containers run in CI. The one documented enhancement
(adaptive prefetch-depth scaling) and one follow-up (cold-start
budget in `ping()`) are called out but not required for Phase C
completion.

- [x] [Design] ADR-011: server-side cursor registry — written in
      `docs/DECISIONS.md`. Per-session cap (default 32), idle-first
      (LRA) eviction with callback-based `driver.cancel` routing,
      pump task with prefetch + explicit pause/resume, spill-to-disk
      with resume-via-HTTP endpoint. Registry lives above the driver
      layer (`crates/server/src/cursors.rs`); ADR-013 boundary
      undisturbed.
- [x] [Design] ADR-012: schema cache contract — written in
      `docs/DECISIONS.md`. Key = `(spec_hash, canonical_scope_json)`;
      60s TTL ceiling; PG LISTEN/NOTIFY on `sift_schema_change`
      (opt-in DDL trigger); SQL Server 30s poll of
      `sys.objects.modify_date`. Cache hit/miss/invalidation counters
      exposed as atomics.
- [ ] [Design] Predictive prefetch: speculatively fetch page N+1
      when page N is acked. **Partially done** — the pump layer
      (`cursors.rs`) buffers `prefetch_pages` (default 2) ahead of
      the consumer via a bounded channel, which delivers the "page
      N+1 already buffered when the client asks" behavior. Adaptive
      depth based on ack velocity is a future enhancement, not
      shipped.
- [x] [Design] Pool pre-warm — PG + SQL Server. `PgConnectionSpec.pool_min_size`
      is honored on `Driver::open`: `min-1` extra pool slots are
      pulled concurrently and returned to deadpool as idle.
      `MssqlConnectionSpec.pool_min_size` lands with the SQL Server
      warm-idle pool (below).
- [x] [Implement] Server-side cursor registry with eviction —
      `crates/server/src/cursors.rs`. 14 unit tests cover cap eviction,
      LRA touch, pump forwarding, pause/resume, spill write + read,
      TTL reap, per-session isolation. Integration tests in
      `tests/api_smoke.rs`:
      `websocket_mid_stream_cancel_stops_paging` and
      `ws_streaming_bounded_memory_across_many_pages` (10k pages,
      always-on). The 1M-row target from the original plan is
      covered by `ws_streaming_bounded_memory_across_one_million_pages`,
      gated behind the `stress-1m` cargo feature (runs ~3min; passes
      with steady memory).
- [x] [Implement] Schema cache + invalidation —
      `crates/server/src/schema_cache.rs`. Cache hit returns from a
      `DashMap.get` in <1ms. Engine invalidator tasks lazy-spawn on
      first insert per unique spec: PG via `PgExt::listen` on a
      dedicated conn, MSSQL via periodic
      `SELECT MAX(modify_date) FROM sys.objects`. 6 unit tests plus
      `tests/api_smoke.rs::schema_cache_serves_second_call_without_touching_driver`
      (primes `MockDriver` with one canned snapshot, asserts second
      HTTP call is served from cache).
- [x] [Implement] Predictive page-N+1 prefetch — the cursor pump's
      bounded consumer channel (default 2 slots) provides exactly
      this: page N+1 is pumped as soon as N is consumed, waiting
      on the ack cadence. ADR-011 explicitly scopes out adaptive
      depth-scaling (velocity-based) — see the ADR-011 scope note.
- [x] [Implement] Pre-warm pool on `OpenConnection`/profile-open —
      both engines. PG uses `PgConnectionSpec.pool_min_size` +
      deadpool. SQL Server gains a per-spec warm-idle pool
      (`crates/driver-sqlserver/src/lib.rs::MssqlPool`): `open()`
      first tries the warm pool before opening a fresh TDS session;
      each miss spawns a background top-up that refills to
      `pool_min_size`. Cold-start budget reported via
      `ServerInfo.pool_warm_slots` — both drivers populate it on
      `ping()`.
- [x] [Implement] Large-result spill to disk — write side and
      read-back side both landed. On eviction the pump writes
      remaining pages to `{spill_dir}/sift-cursor-{id}.bin`
      (length-prefixed JSON) when footprint > `spill_min_bytes`
      (default 1 MiB). The synthetic `Page::Error { CursorEvicted }`
      terminal carries a `resume_url`; client resumes via
      `GET /v1/cursors/{id}/pages?from_seq=N&limit=M`. Cleanup:
      final-read deletion + TTL reaper (default 600s) +
      `DELETE /v1/cursors/{id}` explicit.
- [x] [Implement] Response compression: gzip + brotli via
      `tower-http::CompressionLayer`, wired as the outermost layer in
      `crates/server/src/http.rs::app`. WS frames untouched (upgrades
      bypass the compression layer). Tests
      `responses_are_gzipped_when_client_advertises_gzip` and
      `responses_are_uncompressed_when_client_does_not_advertise`.

## Phase D — Headless product features

Goal: the server side of every daily-driver and power-user IDE feature, so
a GUI later is just rendering. Every item below is verified absent from the
`Operation` enum and the route table.

- [x] [Design] Autocomplete API: server endpoint returning ranked
      candidates scoped to connection + schema + cursor position.
      Server-side composition on top of `SchemaCache` + a new
      `sift-completion` workspace crate housing sqlparser-rs tokenization,
      context detection, keyword/function tables, and the ranker — no
      new `Driver` method (ADR-017 preserved). Shape mirrors `ddl.rs`.
- [ ] [Design] Export pipeline: server-side CSV/JSON/TSV generation from a
      cursor; streaming over HTTP chunked or WS; NULL display policy;
      type-aware cell formatting. (PG `COPY` exists at the driver layer
      via `PgExt::copy`; no server route exposes it.)
- [ ] [Design] DDL generation: `ddl_for(object)` driver method; PG from
      `pg_get_*def`, SQL Server from `OBJECT_DEFINITION`.
- [ ] [Design] Inline-edit → DML generation: edit set → parameterized DML
      → diff preview → execute-in-tx; conflict detection.
- [ ] [Design] Transactions panel contract: server exposes open-tx state
      per connection, savepoint lifecycle (depends on Phase A savepoint
      Operation variants), commit/rollback preview.
- [ ] [Design] Saved-query library. **Note: `saved_query` table already
      exists** (`V004__history.sql:16`) but is **dead schema** — no Rust
      reads or writes it. Define the routes/sharing model and wire it.
- [ ] [Design] Global schema search; data search; execution plans
      (structured plan tree — no `PlanNode` protocol type exists today,
      EXPLAIN would ride `Value::Text`); process list + kill.
- [ ] [Design] Command-palette server surface: enumerate available
      `Operation`s for a given capability context. (`GET /v1/operations`
      exists at `http.rs:649` but returns the whole list unfiltered.)
- [ ] [Design] CSV import → table (server-side ingest, type inference,
      conflict policy). Ties to PG `COPY FROM STDIN` (`PgExt::copy` Import)
      and SQL Server `BULK INSERT` (`MssqlExt::bulk_insert`).
- [x] [Implement] Autocomplete endpoint. **Deviation from original
      plan:** no driver method (would break ADR-017's trait lock); the
      whole feature composes over `Driver::schema` via `SchemaCache`.
      Engine-specific keyword and builtin-function tables live in
      `sift-completion` (`keywords.rs`), not `sift-protocol`, so the
      protocol crate stays pure serde. Route: `POST /v1/sessions/:id/
      connections/:conn_id/complete`; audit `Operation::Complete`.
- [ ] [Implement] Export over HTTP chunked + WS; format selection; NULL +
      type-aware rendering hints.
- [ ] [Implement] DDL generation driver methods; OpenAPI coverage;
      round-trip test (DDL → object → DDL).
- [ ] [Implement] Inline-edit envelope; transactions panel server state;
      saved-query routes; schema-search; data-search; plan capture +
      structured `PlanNode`; process-list + kill; capability query; CSV
      import.

## Phase E — Hosted auth & identity

Goal: take auth from "bearer token + loopback bypass" to "hosted mode with
real identity," without breaking local-first (ADR-006, ADR-010).

- [ ] [Design] ADR-019 (candidate): hosted identity model — local mode
      stays loopback-bypass + API tokens; hosted mode requires GitHub OAuth
      as primary, OIDC as enterprise, keypair as programmatic.
- [ ] [Design] OAuth flow shape (auth-code + PKCE); session token model
      (short-lived access + rotating refresh with replay detection);
      principal → tenant binding (invite/accept, default-tenant on first
      OAuth login).
- [ ] [Implement] GitHub OAuth login route pair; OIDC route pair for
      enterprise; session-token issue/refresh/revoke with rotating refresh
      tokens.
- [ ] [Implement] Keypair auth. **Note: `principal_key` and
      `keypair_challenge` tables already exist** (`V001__identity.sql:40`,
      `:53`) but are **dead schema** — no Rust touches them. Wire or drop.
- [ ] [Implement] Local-mode guarantee: when `mode = local`, OAuth/OIDC/
      keypair are disabled and loopback-bypass + bootstrapped local
      principal remain the only path.
- [ ] [Implement] Principal profile sync (display name, email, avatar from
      GitHub on login); expose via `/v1/auth/whoami`.

## Phase F — Authorization, tenancy & limits

Goal: once multiple principals exist, scope what each can do. Today the
only authorization is room RBAC; per-connection and tenant-resource
enforcement are entirely absent.

- [ ] [Design] ADR-020 (candidate): authorization model — connection-level
      permissions, room roles (already owner/editor/viewer), tenant roles;
      where policy is evaluated.
- [ ] [Design] Rate limiting (per-principal + per-tenant token bucket or
      sliding window); 429 + `Retry-After`. `Code::RateLimited` does not
      exist today.
- [ ] [Design] Tenant isolation: connection quotas, concurrent-query caps,
      total-result-bytes-per-tenant; `Code::TenantResourceExhausted` (does
      not exist today) instead of a crash.
- [ ] [Implement] Connection-profile permissions: `read_only`,
      `allowed_ops`/`blocked_ops`, `allowed_schemas`; enforced in the
      dispatcher before routing to the driver.
- [ ] [Implement] Rate-limit middleware keyed by principal + tenant;
      configurable per route class.
- [ ] [Implement] Tenant resource accounting: concurrent queries, open
      cursors, result bytes per tenant; metrics exported.
- [ ] [Implement] Saved-query + document namespace isolation per
      tenant/principal.

## Phase G — Collaboration depth

Goal: graduate the room runtime from "foundation" to a real multiplayer SQL
session. CRDT only for query text; everything else server-authoritative.

- [ ] [Design] ADR-014 (candidate): lock collaboration scope — shared SQL
      editor via CRDT, ephemeral presence, shared session/connection state
      via broadcast; explicitly exclude result replication beyond
      references.
- [ ] [Design] CRDT backend choice for `sift-doc`. **Today `sift-doc` is
      not a CRDT** (`crates/doc/src/lib.rs:79-98`) — it is a UTF-8 byte
      buffer with destructive `apply()`, no op-log, no merge, no pluggable
      backend. The `CrdtKind::{Loro,Automerge}` tag is a label, never
      dispatched on. Picking + wiring a real backend (Automerge vs Loro vs
      Yjs) is the core Phase G deliverable.
- [ ] [Design] Late-join protocol: snapshot + ops-since. Today only full
      snapshots are persisted (`metadata/src/lib.rs:744-759`); there is no
      bounded op-log and no compaction.
- [ ] [Design] Presence vs durable separation: presence is ephemeral and
      fire-and-forget; document text is durable CRDT. Today presence rides
      the same `broadcast::channel(1024)` as document ops
      (`room_runtime.rs:84`).
- [ ] [Design] Shared-connection ownership: a connection opened in a room
      is server-owned; members attach and run ops through it with role
      gating (editor+ can run queries, viewer observes).
- [ ] [Implement] Real CRDT in `sift-doc`; snapshot + op-log persistence in
      metadata; deterministic merge across peers.
- [ ] [Implement] Late-join snapshot + ops-since over the room WS; bounded
      op log with background compaction.
- [ ] [Implement] Ephemeral presence channel distinct from the durable
      doc-op channel; not persisted.
- [ ] [Implement] Shared room connection with role gating; result-reference
      broadcast (today the room emits a `RoomQueryResult` *summary*
      (`http.rs:1731-1738`), not a cursor reference peers can page from).
- [ ] [Implement] Observer lag recovery + follow mode.

## Phase H — Remote development & distribution

Goal: a sift server can run remote while a thin client renders locally.
Because sift is already server-first, this is mostly bootstrap + version
handshake.

- [ ] [Design] ADR-021 (candidate): remote topology — SSH-tunneled (Zed
      model) vs hosted-collab-relay vs both.
- [ ] [Design] Remote bootstrap (SSH control-master, binary fetch/upload,
      version check, daemon spawn/reconnect); reconnect + state survival on
      SSH drop.
- [ ] [Design] Version handshake. The client-sdk never sends or inspects
      `X-Sift-Protocol-Version` today (`client-sdk/src/lib.rs` never
      imports `PROTOCOL_VERSION`); the server emits it one-way. Both sides
      need a real handshake once remote mode exists.
- [ ] [Design] Background updater (release channel + signature
      verification); single-binary distribution modes (in-process / daemon
      / container).
- [ ] [Implement] Remote bootstrap client helper; proxy-mode daemon; port-
      forward analogue; background updater; `--mode` distribution modes;
      CI release pipeline.

## Phase I — Extensibility

Goal: third-party drivers, AI/automation consumers, and connection-time
hooks without forking the server.

- [ ] [Design] ADR-022 (candidate): driver extensibility — in-tree drivers
      first-class; third-party drivers register over a local RPC protocol
      implementing the `Driver` trait shape.
- [ ] [Design] Driver RPC Protocol contract (wire encoding, capability
      advertisement, streaming `Page` frames, cancel cross-call); the RPC
      proxy must satisfy driver-isolation (ADR-013).
- [ ] [Design] MCP server surface (`sift mcp`): every `Operation` is a
      tool; results are protocol types.
- [ ] [Design] MCP governance layer (operation classification, per-
      connection policy, approval flow for write/destructive ops); ties to
      Phase F authorization.
- [ ] [Design] Connection hooks (`PreConnect`/`PostConnect`/etc); tunneling
      for user DBs (SSH/SOCKS5/HTTP CONNECT/SSM); plugin/extension loading.
- [ ] [Implement] Driver RPC host; `sift mcp` subcommand; governance
      middleware; connection hooks; tunnel profiles; extension loader.

## Phase J — Operations polish

Goal: the last mile before a real release.

- [ ] [Design] Metrics surface (`/v1/metrics` Prometheus); OpenTelemetry
      export; server-side migrations policy (`sift migrate` subcommand vs
      startup gate — today refinery runs eagerly on startup,
      `metadata/src/lib.rs:80`); backup/restore ops; query plan capture +
      retrieval; scheduler.
- [ ] [Design] Release + packaging (musl/static Linux, macOS, Windows;
      per-channel artifacts; signature material for the Phase H updater).
- [ ] [Implement] Prometheus metrics endpoint; OTLP trace export; `sift
      migrate` subcommand + startup gate with pre-release CI matrix;
      backup/restore driver methods + Operations; plan capture wired into
      `execute`; scheduler runtime.
- [ ] [Implement] **OpenAPI generation from typed schemas** to replace the
      hand-authored JSON at `http.rs:655-978`. The hand-authored map already
      drifts from routes (e.g. `GET /v1/sessions/{id}/connections/{conn_id}`
      is not listed). Single source of truth = `utoipa` annotations or
      route-level schema extraction; add a drift test.

---

## Sequencing & dependency notes

- **Phase A is complete: the driver trait is locked.** ADR-017 now absorbs
  the Numeric/Interval/TLS/parity/backref decisions, SQL Server cancel is
  explicitly abort-plus-discard under tiberius rather than a hidden TDS
  attention promise, runtime MARS is out of `MssqlExt`, and native bulk is
  deferred until there is a typed-row request shape. SQL Server pool warmth
  is Phase C performance work, not a Phase A trait blocker.
- **Phase B is complete — the gate for "hosted is conceivable" is met.** All
  reliability, safety, and hardening items landed: per-query timeout + spawn
  discipline, graceful shutdown (ADR-018), health/readiness split, durable +
  sanitized operation audit, correlation ids, connection recovery, driver
  isolation (ADR-013), protocol versioning (ADR-016), secret backends
  (file + keychain), the HTTP result row/byte caps, and constant-time bearer
  comparison. Remaining backlog is later-phase feature work (C onward).
- **Phase C is complete.** Server-side cursor registry (ADR-011) with
  spill/resume, schema cache (ADR-012) with LISTEN/NOTIFY + polling
  invalidators, HTTP gzip/brotli compression, PG pool pre-warm, and
  SQL Server per-spec warm-idle pool with background top-up all
  landed. Documented enhancements (adaptive prefetch, cold-start
  budget reporting in `ping()`) are called out on their line items
  but not required for Phase C completion. Phase D can proceed.
- **Phase D's saved-query work is partially unblocked** — the metadata
  table already exists (dead schema); wiring routes is mostly Implement,
  not Design.
- **Phase E's keypair work is partially unblocked** — `principal_key` and
  `keypair_challenge` tables already exist (dead schema).
- **Phase G's first deliverable is replacing `sift-doc` with a real CRDT.**
  Everything else in G (late-join, presence split, follow mode) depends on
  it. The current apply-op abstraction cannot satisfy the collaboration
  contract.
- **Phase H depends on E (auth) + a real version handshake.** The one-way
  header today is not a handshake.
- **Phase I is mostly orthogonal** but governance depends on F.
- **Phase J's OpenAPI item can land earlier** — the hand-authored map is
  already drifting and is a documentation-contract hazard.

## ADR candidates this list implies

| # | Candidate | Origin | Status |
| --- | --- | --- | --- |
| ADR-011 | server-side cursor registry (cap + LRA eviction + spill/resume) | Phase C | written |
| ADR-012 | schema cache with TTL + engine-specific invalidators | Phase C | written |
| ADR-013 | driver isolation | Phase B | written; both engines meet the containment boundary |
| ADR-014 | collaboration scope (CRDT text only) | Phase G | not written |
| ADR-016 | protocol versioning + semver stability | Phase B | written; pin-or-proceed negotiation, monotonic integer version |
| ADR-017 | driver trait shape | Phase A | written; Phase A trait lock |
| ADR-018 | graceful shutdown contract | Phase B | written |
| ADR-019 | hosted identity model | Phase E | not written |
| ADR-020 | authorization model | Phase F | not written |
| ADR-021 | remote topology | Phase H | not written |
| ADR-022 | driver extensibility | Phase I | not written |

## Reference: what is being stolen, and what is not

Stealing (with attribution):
- **Zed** — process discipline (→ driver isolation ADR-013), restart model
  (→ metadata + room snapshots), action system with capability checks
  (→ Phase D capability query), background updater (Phase H), CRDT-only-
  for-text (Phase G), progressive post-paint indexing (Phase C schema
  cache), late-join = snapshot + ops-since (Phase G), GitHub OAuth
  `read:user` flow (Phase E), remote SSH bootstrap + proxy-mode daemon
  reconnect (Phase H).
- **dbflux** — Driver RPC Protocol for out-of-process drivers (Phase I),
  MCP server + governance/approval layer (Phase I), SSH/SOCKS5/HTTP/SSM
  tunnel profiles (Phase I), connection hooks (Phase I), audit redaction +
  query fingerprinting + centralized error correlation id (Phase B).

Not copying (per ZED_LESSONS §5):
- CRDTs for results/schema/sessions — those stay server-authoritative.
- Local-first file ownership — sift's source of truth is the user DB, not
  a client-owned file (ADR-002).
- Treating result grids as editable buffers — they need server-side
  cursors, virtualization hints, and backpressure.
- Replicating result data to peers — share a reference, not the rows.
