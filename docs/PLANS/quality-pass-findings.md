# Repo-wide quality pass — findings

Read-only review across `crates/server`, both driver crates, `crates/metadata`,
`crates/protocol`, `crates/doc`, `crates/core`, plus cross-cutting checks
(CI vs live tests, OpenAPI vs router, client SDK vs server, dead schema,
feature-flag coherence, doc citations, cargo-deny). Every finding is
anchored to `file:line` with a concrete failure scenario. Ordered by
severity within each tier.

## P0 — real correctness bugs (fix before Phase C)

### 1. WS `Cancel` never reaches the driver
- File: `crates/server/src/http.rs:2596-2603`
- Detail: `WsClientMessage::Cancel` received during an active stream is
  silently rejected out of `wait_for_ack` with "cancel during active
  stream must use HTTP cancel endpoint". The handler drops the receiver;
  `driver.cancel` is never invoked. `Message::Close` at `http.rs:2615`
  has the same class of miss.
- Failure scenario: WS client streams a large MSSQL query, sends
  `Cancel` mid-stream. The server tears down the socket but never calls
  the driver-level cancel, so MSSQL's documented abort+discard invariant
  (see `session.rs:664-670`) is silently violated. The connection stays
  registered in the driver's internal map until the mpsc drop eventually
  reaches the driver — different lifetime and different semantics from
  the abort+discard the plan claims.

### 2. PG stream panic wedges the ConnHandle
- File: `crates/driver-postgres/src/stream.rs:80-94`
- Detail: `catch_unwind` catches the panic and emits `Page::Error`, but
  `finish()` — which removes the cursor entry and restores the conn
  slot — is only invoked inside `run_query_inner`. On panic the cursor
  stays in `inner.cursors` forever and the slot stays `ConnState::Taken`.
- Failure scenario: any decode/panic during streaming permanently
  wedges that ConnHandle. Every subsequent op on it returns "connection
  is busy with another op". A later `cancel(cursor)` tries to open a
  fresh socket with the stale `CancelToken`.

### 3. `execute_http` timeout leaks a runtime task if driver hangs pre-cursor
- File: `crates/server/src/session.rs:556-579`
- Detail: `execute_http` spawns the driver call under a deadline. On
  timeout it detaches the task, expecting it to "reach a safe point on
  its own". `cancel_after_timeout` runs, but on timeout paths *before*
  `slot` is set (i.e. the task is still inside `driver.execute` and
  hasn't returned the stream) there is no cursor yet, so no cancel is
  issued.
- Failure scenario: driver hangs inside `execute` before returning the
  stream. The spawned task lives on the runtime indefinitely, holding
  the connection handle and the `_query_guard` — which blocks
  `Shutdown::await_drain` and defeats graceful shutdown. The
  `wedged_execute_times_out_within_deadline` test doesn't exercise this
  case.

### 4. `cancel_query` doesn't authenticate the caller
- File: `crates/server/src/http.rs:2179-2198`
- Detail: auth middleware only checks the static bearer / loopback
  bypass. There is no principal, room, or connection-owner check inside
  `cancel_query`. Cursor ids are numeric, monotonic, and small.
- Failure scenario: an authenticated attacker enumerates recent
  cursor ids and cancels other users' queries. For MSSQL this also
  removes the connection (see `session.rs:664-670`). `execute_query` at
  `http.rs:1987` has a similar shape but at least records history
  against the caller; `cancel_query` records nothing tenant-scoped.

### 5. PG cancel always uses `NoTls` regardless of open's SSL mode
- File: `crates/driver-postgres/src/lib.rs:132`
- Detail: `token.cancel_query(tokio_postgres::NoTls)` is hard-coded.
- Failure scenario: a server configured with `hostssl` (TLS required
  even for cancel sockets) rejects the cancel; the client observes
  "cancel succeeded" but the query keeps running server-side. The
  higher-level `Reused/Reopened` reconnect story does not paper this
  over.

### 6. MSSQL cancel leaves driver conn map with no live connection
- File: `crates/driver-sqlserver/src/lib.rs:236-275`
- Detail: `task.abort()` drops the `MssqlConn` owned by the spawned
  future. Nothing reinserts. The ConnHandle is still registered from
  the driver's internal perspective, though the server does remove its
  own `session.connections` entry.
- Failure scenario: after cancel, `take_conn(&handle)` returns
  `ConnectionFailed { "no conn for handle" }` for any future op on that
  handle. The server side has removed the session-level entry, so this
  only surfaces via layered code paths that still hold a handle
  reference — but the driver-internal invariant is broken.

## P1 — behaves-wrong-but-not-catastrophic

### 7. PG shallow schema filter partially applied
- File: `crates/driver-postgres/src/schema.rs:42-84`
- Detail: only `filter.name_pattern` is pushed down.
  `filter.schemas` and `filter.kinds` are silently ignored.
- Failure scenario: `SchemaFilter { schemas: ["app"], kinds: [Table] }`
  returns every non-system schema and every relkind (views, sequences,
  materialized views), not "tables under app". Contrast MSSQL's
  `schema_filter_matches` at `crates/driver-sqlserver/src/lib.rs:526,
  1170` which does honor these.

### 8. PG `unlisten` is a no-op
- File: `crates/driver-postgres/src/lib.rs:189-195`
- Detail: validates channel names, then returns `Ok(())` without
  issuing `UNLISTEN`.
- Failure scenario: caller believes it has unsubscribed; the LISTEN
  side of the connection continues to receive and buffer notifications.

### 9. MSSQL `bulk_insert` collapses empty strings to `NULL`; no first-row cap
- File: `crates/driver-sqlserver/src/lib.rs:1044-1049`
- Detail: `mssql_literal("")` returns `"NULL"`, so an empty string is
  unrepresentable. All values are wrapped as `N'…'` regardless of
  column type; numeric/binary/date columns rely on implicit cast.
  `BULK_INSERT_MAX_SQL_BYTES` isn't consulted before the first row is
  flushed, so a >512 KiB single row bypasses the cap.
- Failure scenario: importing CSV with legitimate empty fields
  silently substitutes NULL; a single very-large row escapes the byte
  cap and can OOM the driver.

### 10. File secret backend does blocking IO inside async trait method
- File: `crates/metadata/src/secrets/file.rs:143-162`
- Detail: `SecretStore::{put,delete}` are `async` but call
  `std::fs::write` / `std::fs::rename` synchronously.
- Failure scenario: under tokio these stall the worker thread. On slow
  filesystems (encrypted home dir, network mount) this blocks other
  tasks. Should use `spawn_blocking` or move the whole store off async.

### 11. File secret backend never fsyncs
- File: `crates/metadata/src/secrets/file.rs:79-84`
- Detail: writes a tmp file and renames, but never fsyncs the file or
  its parent directory.
- Failure scenario: crash between write and dirent flush can revert or
  drop the secret store. Not a plaintext leak, but a durability hole
  for the ADR-008 "no secrets in SQLite" guarantee.

### 12. Client SDK re-declares nine request types instead of using `sift-protocol`
- File: `crates/client-sdk/src/lib.rs:20-80` vs
  `crates/server/src/http.rs:349-410`
- Detail: `CreateRoomRequest`, `AddRoomMemberRequest`,
  `CreateDocumentRequest`, `UpdateDocumentSnapshotRequest`,
  `UpsertConnectionProfileRequest`, `SetCredentialRequest`,
  `OpenConnectionFromProfileRequest`, `IssueTokenRequest`,
  `IssueTokenResponse` are declared parallelly on both sides. Fields
  match today.
- Failure scenario: a server-side field change (add/rename/type)
  silently breaks the wire — no compile error because SDK and server
  don't share the definition. Coverage gap on top of drift: the SDK
  has no methods for the five savepoint routes (`http.rs:148-159`) or
  `close_connection` (`http.rs:122-125`).

## P2 — defer / document only

- **PG deadpool cache grows unbounded** (`crates/driver-postgres/src/conn.rs:37, 89-139`).
  `DashMap<String, Arc<Pool>>` keyed on full serde-JSON of
  `ConnectionSpec` (including password), no eviction. A long-lived
  server that opens many distinct specs leaks memory and keeps
  passwords resident.
- **PG `restore_after_op` silently downgrades `InTx` → `Free`**
  (`crates/driver-postgres/src/conn.rs:255-261`). Contract-only
  invariant; a caller invoking `ping`/`schema` on an InTx conn would
  lose its tx binding without an error.
- **PG NUMERIC decoder trusts `weight`** (`crates/driver-postgres/src/decode.rs:94-122`).
  `int_group_count = weight + 1` bounds a loop with no validation
  against `ndigits`. A hostile payload with `weight = 32766` allocates
  ~130 KiB of `"0000"` before trimming. Not exploitable via a
  well-behaved server; still unvalidated input.
- **MSSQL close ↔ task-finish race** (`crates/driver-sqlserver/src/lib.rs:277-298`
  vs `:449-450`). `close` removes the map entry then aborts; if the
  task's final `conns.insert(conn_id, conn)` slipped past abort, a
  stale entry lives for the process's lifetime.
- **Missing `ConnectInfo` treated as loopback** (`crates/server/src/http.rs:186-192`).
  If `into_make_service_with_connect_info` is dropped by a future
  refactor, remote clients get authenticated as `local:1` under the
  default `loopback_bypass=true`. Live main path is fine; footgun for
  future refactors.
- **`cancel` doesn't call `driver.close(handle)` on the dropped conn**
  (`crates/server/src/session.rs:656-673`). Relies on `ConnHandle::Drop`
  to close the backend session; if the driver's Drop is a no-op this
  is a slow FD/socket leak.
- **`Operation::Metadata { action, target, id }` are free-form strings**
  (`crates/protocol/src/operation.rs:73-77`). Audit consumers treating
  them as enums will drift. No bounded vocabulary.
- **Stale `file:line` citations in the plan doc**
  (`docs/PLANS/server-build-list-v2.md`). Line 84 says 38 routes;
  actual is 39 (lines 63-164). Lines 90, 94, 98, 101 cite `ws_session`,
  `ws_room`, `auth_middleware`, `ensure_tenant` at positions that have
  since shifted (actual: 2200, 2218, 274, 503). Doc hygiene only.

## Cross-cutting checks that came back clean

- CI vs live-test env vars: `SIFT_{PG,MSSQL}_*` in workflow match what
  `tests/live_pg.rs` and `tests/live_mssql.rs` read; feature flags
  `live-pg` / `live-mssql` correctly gated.
- OpenAPI vs router: both sides have 39 paths; set-difference (after
  normalizing `:param` → `{param}`) is empty in both directions.
- Dead schema: `principal_key`, `keypair_challenge`, `saved_query`
  still have zero non-migration references — no regression, no
  progress.
- Feature-flag coherence: every declared feature is used; every
  `cfg(feature = …)` references a declared feature.
- `cargo deny check`: advisories, bans, licenses, sources all ok.
- Metadata: refinery migrations ordered, V011 columns populated
  end-to-end from the session write path; bind values never persisted;
  ChaCha20-Poly1305 uses a fresh 96-bit random nonce per persist; the
  wrong-key decrypt path is test-covered; Argon2id parameters meet
  OWASP tiers.
- `crates/core` is genuinely empty.
- `crates/doc` is a non-CRDT apply-op wrapper as documented — not
  broken, just deferred convergence.
- Redaction / SQL fingerprinting behaves as claimed
  (`operation_trail_is_fingerprinted_and_secret_free`).
- Drain gate, `/v1/ready`, protocol-version middleware, constant-time
  bearer, correlation-id propagation on WS handler tasks — all behave
  as claimed and are test-covered.

## Suggested sequencing

- **P0 (#1–#6)** fix before Phase C. Three of them (#1, #3, #6)
  undermine the driver-isolation and abort+discard invariants the plan
  claims are in place; #4 is a plain auth bypass.
- **P1 (#7–#12)** batch into a follow-up commit series; several touch
  the same driver files as the incoming cursor-registry work.
- **P2** documented here; fix opportunistically or when the
  surrounding code is next touched.
