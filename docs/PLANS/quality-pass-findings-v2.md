# Repo-wide quality pass v2 — findings

Read-only re-review across the same surface as v1 (`crates/server`, both
driver crates, `crates/metadata`, `crates/protocol`, `crates/doc`,
`crates/core`) **plus the new, never-before-reviewed `crates/completion` +
`crates/server/src/autocomplete.rs` + `crates/protocol/src/completion.rs`**.
Every finding is anchored to `file:line` against HEAD `9640bf7` with a
concrete failure scenario. Ordered by severity within each tier.

Two systemic themes run through almost every P1 below:

1. **Synchronous work on the async worker thread.** `spawn_blocking` is
   used inconsistently. The metadata crate, the spill read/write paths,
   the audit log, and the keychain secret backend all do blocking I/O on
   tokio workers. This directly violates the AGENTS.md non-negotiable:
   *"A wedged driver cannot freeze the server — queries run in
   `tokio::spawn` with timeouts + cancel tokens, never inline in
   handlers."*
2. **Per-row / per-cell allocation on hot paths.** `Page::clone()` on
   every pump page, `format!` per NUMERIC digit group, `to_string()` per
   cell for column names only used in error arms, `to_ascii_lowercase()`
   per completion candidate per keystroke. Individually small, together
   they are the gap between the current code and the stated
   "Zed-class snappiness" product goal.

---

## Status of v1 findings

The superseded v1 quality pass listed 12 P0/P1 issues and 8 P2 items.
Commits `ab4b115`, `c2eb41b`, `a1112db`, `163acbb` addressed most of
them. Re-verified against current source:

| v1 # | Finding | Status | Current proof |
|------|---------|--------|---------------|
| 1 | WS Cancel never reaches driver | FIXED | `http.rs:3046-3057`, `session.rs:813` |
| 2 | PG stream panic wedges ConnHandle | FIXED | `stream.rs:86-101, 109-113` (`evict_after_panic`) |
| 3 | execute_http timeout leaks task | FIXED | `session.rs:676-695` (pre-cursor `task.abort()`) |
| 4 | cancel_query doesn't authenticate caller | **PARTIAL** | driver-layer check only (`lib.rs:146-152`, `mssql:336-342`); HTTP handler has zero caller-scoping |
| 5 | PG cancel always uses NoTls | FIXED | `Require`/`VerifyCa`/`VerifyFull` use TLS; cancel is timeout-bounded |
| 6 | MSSQL cancel leaves dead conn map entry | FIXED | `lib.rs:344-373` (defensive remove + stray-cursor sweep) |
| 7 | PG shallow schema filter partially applied | FIXED | `schema.rs:51-91` (LIKE + `ANY($2::text[])`) |
| 8 | PG unlisten is a no-op | FIXED | `lib.rs:245-278` (issues `UNLISTEN`) |
| 9 | MSSQL bulk_insert empty-string-to-NULL | FIXED | `lib.rs:1165-1171` (`N''`) |
| 10 | File secret backend blocking IO in async | FIXED | `file.rs:55-63` (`spawn_blocking`) |
| 11 | File secret backend never fsyncs | FIXED | `file.rs:102-120` (file + parent-dir fsync) |
| 12 | Client SDK re-declares request types | FIXED | `client-sdk/src/lib.rs:7-11` re-exports `sift_metadata::http` |
| P2 | PG deadpool cache unbounded | PARTIAL | soft cap `MAX_POOLS=64`; eviction best-effort (only `strong_count==1`) |
| P2 | PG restore_after_op InTx→Free | FIXED | `conn.rs:338-352` (rejects InTx in `take_for_op`) |
| P2 | PG NUMERIC trusts weight | FIXED | `decode.rs:98-105` (bounded loops) |
| P2 | MSSQL close↔task race | FIXED | `lib.rs:378-405` |
| P2 | Missing ConnectInfo treated as loopback | PARTIAL | `x-sift-peer-addr` spoofing closed; fallback-to-loopback still silently mis-authenticates under default `loopback_bypass=true` |
| P2 | cancel doesn't call driver.close | FIXED | `session.rs:818-831` (MSSQL-only, correct) |
| P2 | Operation::Metadata free-form strings | STILL | `operation.rs:84-88`; now documented-as-intentional |
| P2 | Stale file:line citations in plan doc | PARTIAL | route count fixed (39); four function-position citations re-staled to new wrong values |

The remaining **PARTIAL P0 residual (#4)** is worth re-opening — see
P0-6 below.

---

## P0 — real correctness / DoS / security (fix before any user ship)

### P0-6. `cancel_query` HTTP handler still has no caller-scoping (v1 #4 residual)
- File: `crates/server/src/http.rs:2541-2559`
- Detail: v1 #4 was "partially fixed" — the ownership check moved to
  the driver layer (cursor's recorded `conn_id` must match the passed
  `ConnHandle`). The HTTP handler records `Operation::CancelQuery` with
  the caller's `req` but performs no principal / room / connection-owner
  check.
- **Why it matters:** effective against the documented threat (a caller
  can only supply their own session's `ConnHandle`), but
  defense-in-depth-light. Any future bug that lets two sessions share a
  `ConnHandle` id silently passes the check. Cursor ids are numeric,
  monotonic, and small — enumeration is trivial.
- Fix: add an HTTP-level ownership assertion (cursor belongs to a
  session owned by the caller's principal).

---

## P1 — perf or correctness under the documented workload

### Hot-path allocation (server)

#### P1-alloc-1. Every page deep-cloned on the pump happy path
- File: `crates/server/src/cursors.rs:532`
- Detail: `let send_fut = consumer_tx.send(page.clone());` — the clone
  exists only so `page` survives into the rare cancel branch's
  `spillover.push(page)` (line 537). The clone happens on every page.
- **Why it matters:** a 5,000-row page × 10 `Value::Text(64B)` cells =
  ~3.3 MB of row data; the clone memcpy's that plus ~5,000 Vec header
  allocations + ~50,000 String clones. For a 1M-row query batched 5k/page
  that's ~200 clones ≈ **660 MB of transient allocation and ~10M small
  heap allocs per large query**.
- Fix: move `page` into the send future by value; clone lazily only in
  the cancel arm.

#### P1-alloc-3. Export path: per-row + per-cell allocations, no buffering
- Files: `crates/server/src/export.rs:141-184` (`encode_row`),
  `:193-214` (`value_to_text`), `:256-269` (`row_as_json`)
- Detail: `encode_row` allocates a fresh `String::new()` per row, grows
  it char-by-char, then `Bytes::from(out)` (heap copy #2 per row).
  `value_to_text` does `i.to_string()` per numeric cell.
  `row_as_json` clones every column name string as the JSON key for
  every row.
- **Why it matters:** a 1M-row CSV export with 10 columns allocates
  ~10M small Strings for cells + 1M row Strings + 1M Bytes conversions
  ≈ **12M heap allocations for the encoding alone**, on top of driver
  row production cost.
- Fix: reuse a `String` buffer across rows; write ints via `itoa`
  (zero-alloc); precompute column-name `Arc<str>` once for JSON.

#### P1-alloc-4. Full-page serialization to one String before every WS send
- File: `crates/server/src/http.rs:3078-3087`
- Detail: `send_json` does `serde_json::to_string(value)` then
  `sender.send(Message::Text(text))`.
- **Why it matters:** `WsServerMessage::Page { page: Page::Rows { rows:
  Vec<Row> } }` is serialized into one contiguous String before any byte
  hits the socket. For a wide 5,000-row page this is a multi-MB
  allocation, then a copy into the WS frame buffer. Lower priority than
  P1-alloc-1/2/3 (pages aren't per-row) but real on wide rows.
- Fix: serialize into a `BytesMut`, or use `serde_json::to_writer` over
  the socket sink.

### Sync I/O on async path

#### P1-io-1. Spill write does `serde_json::to_vec` + `write_all` + `sync_all` on the pump task
- File: `crates/server/src/cursors.rs:623-645`
- Detail: `write_spill` opens a file, JSON-encodes every page
  (`serde_json::to_vec(page)` — re-encoding data that was already
  JSON-serialized on the way in), writes it, and **`file.sync_all()`** —
  a hard fsync — all on the pump task with no `spawn_blocking`.
- **Why it matters:** fsync on a normal SSD is 5–30 ms; on EBS / NFS
  it's 50–500 ms. **The entire tokio worker thread is blocked for that
  window.** With N concurrent evictions, N workers stall simultaneously.
  Worse, the spill threshold check at `cursors.rs:617`
  (`rows.len().saturating_mul(64)`) underestimates wide-row pages by
  50–100x, so spills fire far more often than `spill_min_bytes` intends.
- Fix: wrap in `spawn_blocking`; use a binary format (postcard/bincode)
  to skip the re-encode and base64 expansion of `Value::Blob`; make
  `sync_all` opt-in.

#### P1-io-2. Spill read path fully synchronous, called from async HTTP handler
- File: `crates/server/src/cursors.rs:301-354`, called from
  `http.rs:2599-2601`
- Detail: `read_spill_pages` does `OpenOptions::open`, `file.seek`, and
  a `read_exact` loop with `serde_json::from_slice` per page — all
  blocking, no `spawn_blocking`. With `limit=256` pages × multi-MB pages
  this blocks a worker for seconds. The handler then wraps the result in
  `Json::from(json!({ "pages": pages, ... }))` — all 256 pages held in
  memory at once AND serialized as one giant JSON blob.
- **Why it matters:** for a spilled cursor (by definition a large one),
  multi-hundred-MB allocations are reachable. Worker stall on the read.
- Fix: `spawn_blocking` the file read; stream the response as NDJSON via
  `Body::from_stream`.

#### P1-io-4. Single 16-permit semaphore ceiling on every metadata op
- File: `crates/server/src/http.rs:40-41, 402-417`
- Detail: `MAX_METADATA_BLOCKING_TASKS = 16` gates every
  `metadata_blocking(...)` call — used for auth lookup on every
  authenticated request, room/profile permission checks, query-history
  writes, and the WS room upgrade path. The permit is acquired **before**
  `spawn_blocking`, so a slow SQLite write holds a permit during the
  entire blocking op.
- **Why it matters:** every `execute_query` calls
  `execute_metadata_context` → one permit held across the auth DB lookup.
  With auth-on-every-request + a few concurrent metadata writes, the
  server's effective concurrent-execute ceiling is ~16 regardless of
  worker count. Under load this manifests as request latency jumping
  from ms to seconds with no obvious CPU saturation.
- Fix: split into separate read/write pools, or drop the gate and let
  `spawn_blocking`'s own pool bound concurrency.

### Schema cache

#### P1-cache-3. PG invalidator silently dies on connection blip
- File: `crates/server/src/schema_cache.rs:298-333`
- Detail: when the LISTEN stream ends (driver dropped it after a
  transient failure, server restart, idle timeout), the loop exits and
  the task dies. But `invalidators` still holds the `InvalidatorHandle`,
  so `ensure_invalidator`'s fast-path `contains_key` check returns
  `true` and the task is **never restarted**. PG invalidation silently
  falls back to 60 s TTL for the rest of the process lifetime.
- **Why it matters:** silent staleness — DDL changes don't invalidate
  the cache for up to 60 s, with no operator-visible signal. Same
  defect in `mssql_poll_task` (`:356-378`): dead connection → every poll
  errors and `continue`s, task stays alive doing nothing forever.
- Fix: on loop exit remove the entry from `invalidators` so the next
  `insert` respawns it; add reconnect-with-backoff.

#### P1-cache-4. Unbounded connection fan-out + no max_entries on cache
- File: `crates/server/src/schema_cache.rs:256-291`
- Detail: `ensure_invalidator` spawns **one dedicated driver
  connection + one tokio task per spec, forever**. `entries: DashMap`
  has no max-entries bound.
- **Why it matters:** a multi-tenant server with M distinct connection
  specs holds M extra DB connections open purely for invalidation.
  Because there is no entry or invalidator cap, this remains an
  unbounded resource leak. 100 connections × ~10 deep-scope objects =
  1,000 cached snapshots with no eviction other than TTL.
- Fix: bound `entries` (LRU on top of DashMap, or `mini-moka`/`moka`);
  bound `invalidators`; share one poller per (host, port, database).

#### P1-cache-5. Invalidation is a full O(N) scan with key clones
- File: `crates/server/src/schema_cache.rs:182-196`
- Detail: `invalidate_spec_by_hash` iterates the entire `entries` map,
  filters, clones matching keys into a Vec, then removes.
- **Why it matters:** with 1,000 cached scopes and frequent DDL, this
  is 1,000 iterations + N allocations per invalidation event, holding
  DashMap shard read locks during iteration. PG can fire on every
  `NOTIFY`.
- Fix: maintain a secondary index `spec_hash → Vec<CacheKey>`.

### Completion hot path (the "Zed-class snappiness" goal)

#### P1-comp-1. `score_match` calls `to_ascii_lowercase()` once per candidate per keystroke
- File: `crates/completion/src/rank.rs:249`
- Detail: `let cl = candidate.to_ascii_lowercase();` heap-allocates a
  fresh String for case-folding, for every keyword (~90 PG / ~90 MSSQL),
  every function (~55 / ~55), every object, and every column on every
  keystroke.
- **Why it matters:** with a 1k-table schema and an unqualified-column
  walk (`ExpectingColumn { qualifier: None }` → `push_all_columns` →
  `push_columns` → `score_match` per column), this is easily 50k+
  allocations per keystroke. The lowercased form is invariant across
  keystrokes.
- Fix: precompute `name_lower` at Dictionary-build time on
  `ObjectEntry`/`ColumnEntry`; static keyword tables need no fold.

#### P1-comp-5. Full re-tokenize of preceding SQL every keystroke
- File: `crates/completion/src/context.rs:40-45`
- Detail: `Tokenizer::new(...).tokenize()` re-lexes the entire SQL
  preceding the cursor from offset 0 and collects into a fresh
  `Vec<Token>` on every keystroke.
- **Why it matters:** for a 50 KB query this is a lot of work, repeated
  once per keystroke, mostly producing the same token sequence as the
  last call. Single biggest CPU item on the keystroke path after the
  Dictionary rebuild.
- Fix: short term, memoize the tokenized prefix on the server keyed by
  `Arc<str>` of the SQL. Longer term, an incremental lexer.

#### P1-comp-8. Keyword/object scans are linear; no prefix index
- File: `crates/completion/src/rank.rs:89-167`
- Detail: every producer loops the entire candidate list calling
  `score_match` on each. `push_tables_and_views` walks all objects on
  every keystroke even if the user's prefix is "zzz" and nothing can
  match.
- **Why it matters:** for objects in a large schema this is the wrong
  complexity. The prefix case (score 1000/800) is the common case and
  the only case the user feels latency on.
- Fix: keep `objects` sorted by `name_lower`; use `partition_point` to
  find the prefix window in O(log n). For keyword tables, consider an
  `fst` automaton at build time.

#### P1-comp-9. `push_keywords` allocates `label`/`insert` Strings even for static `&'static str`
- File: `crates/completion/src/rank.rs:94-101`
- Detail: `kw.to_string()` for static string literals, forced by
  `CompletionCandidate.label`/`.insert` being `String`
  (`protocol/src/completion.rs:48-49`).
- **Why it matters:** for a 90-keyword list with 20 prefix matches per
  keystroke, 40 allocations that could be zero.
- Fix: change the protocol types to `Cow<'static, str>` (or
  `SmolStr`/`CompactString`). The protocol is brand-new and only
  consumed here — cheap breaking change now, expensive later.

### Drivers — new findings (all 11 v1 items confirmed fixed)

#### P1-driver-1. PG `close` while a query is in-flight leaks the conn back into the map
- Files: `crates/driver-postgres/src/conn.rs:256-282` (close) +
  `stream.rs:360-369` (`finish`)
- Detail: `close` removes the `ConnState::Taken` slot and the cursor,
  but **does not abort the spawned query task**. When that task later
  completes naturally, `finish` calls `inner.restore(conn_id,
  slot_kind, conn)`, which **re-inserts** `ConnState::Free(conn)` under
  the now-"closed" `conn_id`. The conn (a `deadpool_postgres::Object`)
  is orphaned in the map forever.
- **Why it matters:** the conn counts against `deadpool`'s `max_size`
  for the pool, is never returned to the idle queue, and accumulates
  over the process lifetime. This is the **exact symmetric race** MSSQL
  closed (by storing the `JoinHandle` and calling `task.abort()`). PG
  never stores the handle. Repeated `execute → close-mid-stream` cycles
  (typical of WS disconnect during streaming) grow `conns`
  monotonically and silently exhaust the pool.
- Fix: store the `JoinHandle` in the `cursors` DashMap value; `abort()`
  from `remove_conn`. Mirror MSSQL's `close`.

#### P1-driver-4. PG `CopyOp::Export` discards the exported data
- File: `crates/driver-postgres/src/lib.rs:285-295`
- Detail: `conn.copy_out(&sql).await?.try_fold(0_u64, |total, chunk|
  async move { Ok(total + chunk.len() as u64) }).await?;` — folds the
  COPY-out stream into a byte count and **throws away every chunk**.
  `CopyResult` has no field for the data.
- **Why it matters:** the export path is completely non-functional —
  the trait method exists and returns `Ok`, but the caller gets
  `"exported N bytes"` with no payload. Misleading API.
- Fix: extend `CopyResult` with `data: Bytes`, or — preferred for large
  exports — add a streaming variant returning `Stream<Bytes>`.

#### P1-driver-5. PG NUMERIC decoder is allocation-heavy on the per-cell hot path
- File: `crates/driver-postgres/src/decode.rs:64-150`
- Detail: per NUMERIC cell: `Vec<u16>` collect; `String::new()` then
  `push_str(&group.to_string())` and `push_str(&format!("{group:04}"))`
  per digit group; redundant `int_part.to_string()` after
  `trim_start_matches`; same per-group `format!` for `frac`; final
  `out.insert(0, '-')` is O(n).
- **Why it matters:** for a 1M-row result with one numeric column,
  roughly 5–10M heap allocations just for numerics. Worst per-cell
  allocation pattern in either driver.
- Fix: `arrayvec::ArrayVec<u16, 64>`; write digit groups into one
  pre-sized String with manual div/mul; drop the redundant copy.

#### P1-driver-7. MSSQL `pools` map is unbounded (same bug class as the fixed PG one)
- File: `crates/driver-sqlserver/src/lib.rs:49`
- Detail: `DashMap<String, Arc<Mutex<MssqlPool>>>` with no cap, no
  eviction. PG has `MAX_POOLS = 64` + best-effort eviction.
- **Why it matters:** every distinct `ConnectionSpec` creates a
  permanent entry, each holding `min_size` warm TCP connections. With
  many specs (multi-tenant, exploration tool, rotated credentials),
  the map grows without bound, each entry holding FDs.
- Fix: mirror PG — `MAX_POOLS` cap, evict `strong_count == 1` entries.

#### P1-driver-8. MSSQL warm pool has no validation on `pop_warm`
- File: `crates/driver-sqlserver/src/lib.rs:81-85`
- Detail: `pop_warm` just pops; no liveness probe. PG relies on
  `deadpool-postgres`'s `recycle` (ping on `get()`).
- **Why it matters:** after a backend restart, network blip, or
  server-side idle timeout, `open` returns a dead conn, the user's
  first query fails with an opaque transport error, and the conn is
  dropped — no retry. `pool_warm_slots_for` over-reports usable slots.
- Fix: issue `SELECT 1` (or use tiberius's lazy-reconnect) before
  handing the conn out; on failure discard and try the next.

#### P1-driver-10. MSSQL silently drops Money / DatetimeOffset / SmallDateTime / SqlVariant to NULL
- File: `crates/driver-sqlserver/src/lib.rs:974-1035` (`ms_value`)
- Detail: `ms_type_ref` (`:1206`) maps `Money | Money4` →
  `PrimitiveType::Decimal`, but `ms_value` has **no decode arm** for
  these. They fall through to the catch-all (`:1028-1032`):
  `row.try_get::<&str, _>(idx).ok().flatten().map(...).unwrap_or(Value::Null)`.
  tiberius can't satisfy `&str` for binary-typed money → `.ok()`
  swallows → `.flatten()` is `None` → cell becomes `Value::Null`
  silently.
- **Why it matters:** the column metadata claims `Decimal`/`TimestampTz`,
  so the client renders a NULL where there's actually data. **Data
  correctness bug masquerading as a type gap.**
- Fix: explicit arms for `Money`/`Money4`/`DatetimeOffset`. For
  `SqlVariant`, fall through to `Value::Engine`, not NULL.

#### P1-driver-12. PG `run_streaming` accumulates decode-error warnings without bound
- File: `crates/driver-postgres/src/stream.rs:166, 192-194, 329-334`
- Detail: every errored cell pushes a `DriverWarning::new(format!(…))`
  into `warnings: Vec<DriverWarning>`, held until `Page::Done`.
- **Why it matters:** for a 1M-row query with one unsupported column,
  this is 1M String allocations held in memory simultaneously. Pathological
  but realistic (`SELECT *` from a table with one undecoded column).
- Fix: cap at e.g. 100 entries; once full, increment a
  `suppressed_count` and emit a final summary warning.

### Metadata scalability ceiling

#### P1-meta-1. Single `Connection` behind `std::sync::Mutex` serializes all metadata access
- File: `crates/metadata/src/lib.rs:73`
- Detail: one SQLite connection, one mutex. Every read and every write
  across every spawn_blocking task and the audit-writer thread contends
  on this lock. `MAX_METADATA_BLOCKING_TASKS = 16` permits 16 concurrent
  blocking tasks but at most one can hold the mutex — the other 15 sit
  parked.
- **Why it matters:** SQLite in WAL mode supports concurrent readers, but
  this design forfeits that entirely. A long-running read
  (`list_operation_audit(limit=...)` over a growing table) blocks every
  write and every other read for its duration. Under a burst of
  `GET /v1/metadata/rooms` the latency floor is `(N requests) ×
  (per-request query time)`, not `(N / R) × query time`.
- Fix: `r2d2_sqlite`/`deadpool-sqlite` pool with WAL; or an actor model;
  or minimum: `parking_lot::Mutex` + split read-only connection.

#### P1-meta-2. Argon2 (default cost) on every bearer-authed request, plus a write per verify
- File: `crates/metadata/src/lib.rs:297-331` (`verify_api_token`)
- Detail: `Argon2::default()` is Argon2id with `m_cost=19 MiB,
  t_cost=2, p=1` — ~50–150 ms of CPU. Every authed request calls
  `verify_api_token` (consuming a blocking permit for ~100 ms) AND
  issues `UPDATE api_token SET last_used_at = ?1 … WHERE id = ?2`
  (`lib.rs:326-329`), turning every GET into a WAL write.
- **Why it matters:** sustained API throughput bounded by
  `1 / argon2_verify_time × num_blocking_permits` ≈ `16 / 0.1s` =
  ~160 req/s absolute ceiling, and the per-request UPDATE serializes
  through the mutex. A burst of 50 concurrent requests adds ~30 s of
  tail latency. Combined with P1-meta-1, the audit-writer thread blocks
  behind every verify's UPDATE.
- Fix: token verification should not use a password-hashing KDF — use
  HMAC-SHA256 keyed over `(lookup_prefix || random_part)`, constant-time
  compare. Decouple `last_used_at` from the verify path (debounce).

#### P1-meta-4. Audit row not written in the same transaction as the mutation
- Files: `crates/metadata/src/lib.rs:617-639` (`create_room`),
  `:481-522` (`delete_connection_profile`), `:680-695` (`add_room_member`),
  all mutating methods; audit goes through `server/session.rs:212-226`
- Detail: every mutating method commits its own tx and returns. The
  server then calls `push_metadata_operation` (e.g. `http.rs:1387,
  1406, 1445, 1653, 1675, 1705`), which sends `NewOperationAudit` over
  an mpsc to the audit-writer thread — separate connection, separate tx.
- **Why it matters:** if the process crashes between commit and audit
  write, the audit trail has a gap for a mutation that did happen.
  Violates AGENTS.md *"Every user-visible action is an Operation variant
  and is audited"* — auditable in *intent* but not *durably recorded*.
  The window is small but real.
- Fix: graduate the tradeoff to an ADR; for security-critical mutations
  (delete connection profile, set/revoke credential, revoke token),
  write the audit row inside the same tx.

#### P1-meta-5. Audit-writer thread shares the single connection via unbounded mpsc
- Files: `crates/server/src/session.rs:212-226`,
  `crates/metadata/src/lib.rs:873-903` (`record_operation_audit`)
- Detail: same mutex, different thread. Channel is `std::sync::mpsc` —
  unbounded.
- **Why it matters:** (1) under load, if the writer stalls on the mutex,
  the channel grows without bound — memory growth under pressure.
  (2) When the audit writer is running its INSERT, every request-path
  metadata call blocks behind it. Compounds P1-meta-1 and P1-meta-2.
- Fix: give the audit writer its own `Connection`; bound the channel
  and drop+count on overflow; or move audit into the per-method tx
  (P1-meta-4).

### Memory bounds / task supervision

#### P1-mem-1. HTTP execute materializes the entire result before responding
- File: `crates/server/src/session.rs:1385-1452` (`drain_stream`)
- Detail: accumulates `rows: Vec<Row>` up to `max_result_rows` (10k) /
  `max_result_bytes` (16 MB), then returns `ExecuteResponse { rows, … }`.
  The handler at `http.rs:2384-2395` wraps in `Json(resp)` — so the full
  result is held in memory **twice** (Vec + serialized JSON String).
- **Why it matters:** for a 10k-row × 1 KB-row result that's ~10 MB held
  twice = 20 MB per concurrent HTTP execute. With 50 concurrent
  executes that's 1 GB. Plus `rows.extend(r)` reallocs the destination
  on each grow.
- Fix: cap lower by default for HTTP (the WS path exists for big
  results); pre-reserve `rows`; consider `Body::from_stream`.

#### P1-mem-2. Wedged driver tasks accumulate with no bound
- File: `crates/server/src/session.rs:260-278` (`run_bounded`)
- Detail: on timeout the spawned task is detached (not aborted) so the
  driver reaches a safe point. But there's no bound on how many such
  detached tasks can exist.
- **Why it matters:** a driver that wedges (network partition, DB lock
  wait) under sustained load produces one detached task per timed-out
  request, each holding a `ConnHandle` and pinning driver state. The
  tokio blocking pool has a default cap of 512; once exhausted, new
  spawns compete for worker threads. Combined with the lack of a
  per-connection concurrency bound, a single wedged DB can starve the
  whole runtime.
- Fix: per-driver/connection semaphore; on timeout call `driver.cancel`
  (only `execute_http` does this at `session.rs:682`).

#### P1-mem-3. Pump task and eviction callbacks are detached with no supervision
- Files: `crates/server/src/cursors.rs:207` (`tokio::spawn(pump_task(…))`),
  `crates/server/src/session.rs:165` (`install_eviction_callback`)
- Detail: JoinHandle dropped. If the pump panics, `consumer_tx` drops,
  the consumer sees stream-end-without-terminal, but **the cursor entry
  in `self.inner.entries` is never removed** (only `cursors.remove()`
  removes it, called after a terminal arrives — which never does).
- **Why it matters:** the per-session cursor slot leaks permanently;
  after `max_per_session` (default 32) panics, the session can't open
  any new cursor. Same pattern in eviction callbacks.
- Fix: supervise pumps; on pump exit self-remove from the registry.
  Consider `JoinSet` owned by the registry.

### Lock contention

#### P1-lock-1. Global audit/operations Mutex serializes every operation
- File: `crates/server/src/session.rs:301-313, 388-390`
- Detail: `audit: Mutex<Vec<AuditEntry>>`, `operations: Mutex<OperationLog>`
  — both process-global. `list_audit` and `list_operations` clone the
  entire Vec under the lock.
- **Why it matters:** for the 10,000-entry cap that's 10,000 clones
  while every concurrent operation waits. Every operation across every
  session still acquires `operations`, even though disk persistence no
  longer happens in the lock body.
- Fix: shard by session; `parking_lot::Mutex` + `Arc<Vec>` snapshot
  (replace under lock, clone outside); feed the in-memory ring from the
  writer thread.

#### P1-lock-2. `select_victims` full clone + N mutex locks on every `wrap` at cap
- File: `crates/server/src/cursors.rs:398-420`
- Detail: clones the cursor-id list, then acquires `last_ack` Mutex once
  per cursor.
- **Why it matters:** only fires at the per-session cap, but for a
  session at the 32-cursor cap that's 32 mutex acquisitions on the open
  path.
- Fix: track LRA via a single `AtomicU64` per session, or a
  `BinaryHeap` in a Mutex.

---

## P2 — defer / refactor / hygiene / slow-under-extremes

### Server

- **`http.rs` is 3,087 lines in one file** (`crates/server/src/http.rs`).
  ~70 top-level fns spanning router, middlewares, auth/tenant helpers,
  metadata operation helpers, health, a giant hardcoded OpenAPI blob
  (400 lines of JSON-in-Rust at `:823-1224`), JSON-schema helpers, ~20
  metadata CRUD handlers, auth-token handlers, session/connection/tx
  handlers, spill handlers, and two WebSocket state machines.
  **Why it matters:** inhibits review (this audit was materially harder
  because of it), blocks parallel codegen, makes diff history
  unreadable. Split into `router.rs` / `middleware.rs` / `auth.rs` /
  `metadata_handlers.rs` / `session_handlers.rs` / `ws.rs` /
  `openapi.rs` (and generate the OpenAPI blob from `schemars`).

- **`approx_page_bytes` magic 64-byte-per-row estimate drives the spill
  threshold** (`cursors.rs:615-621`). For wide rows (50 columns) this is
  off by ~50x, causing spills to fire 50x more often than
  `spill_min_bytes` is tuned for. Make config-driven or `Value`-aware.

- **Export bypasses the cursor registry entirely** (`export.rs:34-49`).
  `run_export` calls `driver.execute` and consumes `stream.rows` directly.
  No per-session cursor cap enforcement (a client can spam exports to
  bypass the cap and exhaust DB connections); no `CancelToken` threaded
  through (client disconnect relies on stream drop); no request timeout.
  Violates *"queries run in tokio::spawn with timeouts + cancel tokens"*.

- **`room_runtime.rs:93-101` full clone + sort on every presence event.**
  `presence_for` clones every `RoomPresence` and sorts the Vec on every
  `attach`/`detach`/`PresencePing`, broadcast to every subscriber (who
  each get another clone). O(N log N) per event × N subscribers. Fine
  at small N; redesign if large rooms are a target.

- **`handle_ws` no concurrency within a single socket** (`http.rs:2840-2946,
  3058-3061`). `wait_for_ack` returns `BadRequest("concurrent execute on
  one websocket is not supported")`. Clients wanting parallel queries
  must open multiple WS connections, multiplying per-user server state.
  Worth noting in the protocol doc.

- **`reject_if_connection_has_tx` O(N) scan per execute**
  (`session.rs:1075-1102`). Fine for low tx counts; index by
  `connection_id` if many simultaneous txs are ever supported.

- **`close_session` fans out one spawn per connection** (`session.rs:400-408`).
  A session with 100 connections spawns 100 detached tasks —
  thundering-herd-on-close. Use a bounded `JoinSet`.

### Completion

- **O(N²) schema dedup** (`dictionary.rs:55-58`). Linear scan over an
  accumulating Vec. Low impact today (few schemas); quadratic if
  snapshot grows. Dedupe into a `HashSet`.

- **`format!` per matching column and per object candidate** (`rank.rs:182-186,
  234-236`). Forced by `Option<String>` in the protocol. Same fix as
  P1-comp-9 (`Cow<'static, str>`).

- **Unchecked `as u32` truncating casts** (`lib.rs:42-43`). If SQL ever
  exceeds 4 GiB, byte range silently wraps. Clamp or 400 on overflow.

- **`Tokenizer::tokenize().unwrap_or_default()` silently swallows lex
  errors** (`context.rs:40-43`). A tokenize failure produces an empty
  token Vec → classifies as `Statement` (wrong context, wrong
  candidates, no signal). At least `tracing::debug!(?err)`.

- **`ExpectingColumn { qualifier: Some(_) }` returns zero candidates
  when qualifier doesn't resolve** (`rank.rs:43-53`). For `SELECT foo.|`
  where `foo` is a CTE, alias, or temp table, the user gets an empty
  list — no keywords, no functions, nothing. Fall back to the
  unqualified-column path.

- **Over-eager `[` quote-absorption** (`context.rs:165-170`). For MSSQL,
  `[` is also used in `arr[0]` subscripts. Absorbing a stray `[`
  corrupts `replaced_range`. Also doesn't verify there's no closing
  quote ahead. Restrict to MSSQL engine, or check for close-quote.

- **No benchmarks for the keystroke path.** The product goal is
  "Zed-class snappiness" and there are zero benchmarks anywhere.
  criterion benchmarks for Dictionary construction at 1k/10k objects,
  `complete()` p50/p99 at 1/3/10-char prefixes, `detect_context` on
  1 KB / 50 KB SQL — should exist and run in CI against a regression
  budget.

- **Many test gaps** (`tests/completion.rs`, `server/tests/autocomplete.rs`).
  Only sunny-day flows covered. Not covered: `detect_context` direct
  tests; substring fallback (score 300); case-insensitive prefix (800);
  empty prefix; limit clamp; MSSQL-specific keyword/function tables
  (`TOP`, `OUTPUT`, `GETDATE`, `DATEADD`); `ExpectingObjectInSchema`
  follow-ups beyond the lowercase schema-qualified case; `Unknown`
  context; `ExpectingTable` after `INTO`/`UPDATE`/`TABLE`;
  `resolve_by_name` ambiguity;
  `resolve_qualified`; `quote_ident_if_needed` edge cases (PG identifier
  containing `"`, empty name); SQL inside string literals/comments. **Worst:** the
  deep-snapshot merge test `complete_dotted_returns_columns` does NOT
  verify the deep fetch ran — `MockDriver::schema` ignores its `_scope`
  parameter, so the test passes even if the deep-fetch+merge path is
  broken or removed.

- **Magic scoring constants** (`rank.rs:243-245`). Empty prefix returns
  `Some(500)`, between case-insensitive-prefix (800) and substring (300).
  Intent isn't documented relative to the bonus schedule, making future
  tuning error-prone. Promote to named `const`s.

- **Engine-agnostic ident grammar** (`context.rs:175-177`). `is_ident_byte`
  allows `c >= 0x80` per PG default identifier grammar, regardless of
  engine. Probably fine in practice but over-matches if MSSQL is ever
  more restrictive.

### Drivers

- **Per-cell column-name `String` allocation** (`driver-postgres/src/stream.rs:323`).
  `let col_name = row.columns().get(i).map(|c| c.name().to_string());`
  runs for every cell of every row but is only consumed in the rare
  `Err` arm. 10M throwaway allocs on a 10-col × 1M-row result. Move into
  the Err arm or hoist a `Vec<String>` out of the row loop.

- **`is_row_producing` / `is_pure_dml` misroute CTE-wrapped DML, losing
  `affected_rows`** (`driver-postgres/src/stream.rs:347-358, 122`;
  `driver-sqlserver/src/lib.rs:564-572`). PG: `WITH cte AS (...) INSERT
  INTO t SELECT …` routes through `query_raw` which doesn't surface
  `CommandComplete`. MSSQL: `" OUTPUT "` substring check is
  space-literal, so `INSERT\tOUTPUT\t` evades detection and routes
  through `execute()`, losing returned rows entirely.

- **MSSQL `ms_value` swallows decode errors as NULL** (`driver-sqlserver/src/lib.rs:974-1035`).
  Every arm is `.ok().flatten().map(...).unwrap_or(Value::Null)`. PG
  surfaces decode errors as `Value::Engine { display_text:
  "<decode error>" }` + `DriverWarning`; MSSQL silently substitutes
  NULL. A `nvarchar(MAX)` column failing UTF-8 conversion decodes as
  NULL with no diagnostic. Match PG's contract.

- **MSSQL `bulk_insert_csv` not wrapped in a transaction**
  (`driver-sqlserver/src/lib.rs:1070-1140`). If batch 3 of 5 fails,
  batches 1-2 are already committed — caller gets a partial-insert error
  with no rollback. `BulkResult { rows_inserted }` also reflects rows
  attempted, not committed.

- **PG type coverage gaps** (`driver-postgres/src/decode.rs:34-62`).
  Arrays (`TEXT_ARRAY`, `INT4_ARRAY`, …), `JSONPATH`, network types
  (`CIDR`/`INET`/`MACADDR`/`MACADDR8`), range types, `XML`,
  `MONEY`, `HSTORE`, `TIMETZ` all fall through to
  `Value::Engine { display_text: "<undecoded X>" }`. `pg_type_category`
  *recognizes* XML/MONEY for categorization but `decode_value` has no
  arm — inconsistent.

- **PG `prewarm_pool` runs synchronously inside `Driver::open`**
  (`driver-postgres/src/lib.rs:45-52`, `conn.rs:301-332`). For
  `pool_min_size = 16`, `open` blocks the caller on 16 concurrent TCP+
  TLS+PG handshakes before returning the `ConnHandle`. Prewarm is by
  definition background work. `tokio::spawn(prewarm_pool(…))` from
  `open`, return the handle immediately.

- **NULL parameters typed as `TEXT` server-side**
  (`driver-postgres/src/stream.rs:286`, `driver-sqlserver/src/lib.rs:1041`).
  `None::<String>` sends `oid = TEXT`, forcing implicit `text →
  <column type>` casts per parameter per query. Measurable overhead; can
  fail outright for types with no implicit text cast (`bytea`,
  composite).

- **Parameterized DML loses `affected_rows`** (`driver-postgres/src/stream.rs:122`).
  `if !job.params.is_empty() || is_row_producing(&job.sql)` — any
  parameterized statement routes through `query_raw`. `INSERT INTO t
  VALUES ($1)` reports `affected_rows: None`.

- **Prepared-statement cache unmanaged for ad-hoc workloads** (PG
  `Client` / MSSQL `tiberius`). sift is an IDE — almost every query is
  unique ad-hoc SQL. Both backends accumulate prepared-statement
  metadata until conn close; 8 pooled PG conns × 1000 statements ×
  complex plans = substantial backend memory. PG:
  `set_default_stmt_cache_capacity(64)`.

- **MSSQL cancel permanently orphans the ConnHandle**
  (`driver-sqlserver/src/lib.rs:344-374`). After cancel, the next op on
  the same handle returns `"no conn for handle"` — caller may interpret
  as a driver bug rather than "you canceled, conn is dead by contract."
  Surface `Code::QueryCanceled` or document explicitly.

- **PG `cancel_query` has no internal timeout** (`driver-postgres/src/lib.rs:167, 171`).
  `token.cancel_query(tls).await` opens a fresh TCP connection just to
  send a 16-byte CancelRequest. Network partition → await never returns
  → cancel future hangs forever. Wrap in `tokio::time::timeout(5s, …)`.

- **MSSQL `close`+`abort` ordering uses `yield_now` as a synchronization
  primitive** (`driver-sqlserver/src/lib.rs:394-402`). Works in practice
  but fragile. Replace with `task.await.ok()` after `abort()`.

- **Mock driver can't assert on `sql` or `params`** (`driver-api/src/mock.rs:295-300,
  343-346, 413-418`). Records only method names, not arguments. Tests
  cannot assert "execute was called with this SQL." Real drivers reject
  cross-conn cancel and reject savepoints on missing tx; the mock
  accepts everything. `MockDriver::savepoint` returns `TxId(0)` rather
  than `t.tx_id` — a test that subsequently calls `rollback_to(savepoint)`
  against the real driver would fail.

- **MSSQL `ensure_warm` `refilling` flag isn't reset on panic**
  (`driver-sqlserver/src/lib.rs:90-137`). If the spawned top-up task
  panics, `guard.refilling = false` never runs. Pool stuck
  `refilling = true`, subsequent `ensure_warm` calls bail — pool goes
  permanently cold with no error surfaced. Drop-guard or `catch_unwind`.

### Metadata

- **Secret delete errors swallowed → orphaned secrets** (`lib.rs:463-468,
  470-477, 518-520, 562-566, 567-571`). `let _ = self.secrets.delete(…).await;`.
  If the secret delete fails (disk error, D-Bus hiccup), the DB has no
  remaining reference but the secret persists in the store. At minimum
  `tracing::warn!`; better: write a `secret_orphan` row for a startup
  sweep to retry.

- **`FileSecretStore` write amplification is O(N) per mutation**
  (`secrets/file.rs:55-122`). Every `put`/`delete` clones the entire
  `HashMap`, serializes the whole thing to JSON, encrypts, writes,
  fsyncs, renames, fsyncs the parent dir. Bulk-import of 1000 profiles
  = 1000 full rewrites, each O(1000) — 1M entry-serializations + 1000
  fsyncs. Fine at single-tenant IDE scope; flag the scaling cliff.

- **No prepared-statement cache** (`lib.rs:245, 360, 647, 699, 760, 836,
  924, 942, 1039, …`). Uses `prepare` everywhere; rusqlite has
  `prepare_cached` available. Hot paths (`health_check`,
  `verify_api_token`, `list_saved_queries`) re-compile SQL on every
  call. With one shared connection (P1-meta-1) the cache hit rate would
  be 100% after warmup.

- **`list_saved_queries` dynamic SQL with mixed `?N` and `?` placeholders**
  (`lib.rs:1003-1041`). Binding correctness is implicit on push order
  exactly matching SQL append order. No test pins this; a refactor that
  re-orders appends silently misbinds. Use named parameters or a small
  query-builder.

- **`fts_pattern` collapses pure-punctuation input to `"*"` (match-all)**
  (`lib.rs:1404-1425`). `"***"` or `"@#$"` returns `"*"`, silently
  bypassing the search filter — endpoint returns the entire tenant's
  saved queries. UX bug + minor information disclosure.

- **Dead `principal_key` / `keypair_challenge` schema**
  (`migrations/V001__identity.sql:40-58`). Created by V001, never
  referenced in `lib.rs`. Adds confusion. Drop in a new migration or
  implement keypair auth.

- **V006 is a destructive migration with no backout** (`migrations/V006__rooms.sql:1-3`).
  `DROP TABLE IF EXISTS tab/session_snapshot/workspace`. Pre-release
  this is likely fine; document before any beta user has a DB they care
  about.

- **`update_saved_query` non-atomic read-modify-write** (`lib.rs:1047-1077`).
  No tx, no `BEGIN IMMEDIATE`. Two concurrent updates lose data — and
  the `Option<Option<…>>` shape implies partial updates, so an update
  that only changes `tags` can clobber a concurrent update that only
  changed `sql_text`. Merge in SQL with COALESCE or `BEGIN IMMEDIATE`.

- **`MetadataError::SecretStore(String)` and `From<io::Error>` collapse
  unrelated errors** (`lib.rs:67-68, 1491-1495, 80-82`). `From<io::Error>`
  labels every `io::Error` as `SecretStore`, but the only direct call
  site is `std::fs::create_dir_all` for the **SQLite DB parent
  directory**. Operator sees `"secret store error: Permission denied"`
  while the actual failure is the SQLite path being unwritable.
  Add `MetadataError::Io(#[from] io::Error)`; reserve `SecretStore`
  for actual secret-store failures.

- **Broker credential mode accepted at upsert, rejected at resolve**
  (`lib.rs:401-407` vs `:599-601`). Profile is storable but unusable.
  Reject `CredentialMode::Broker` at upsert until broker auth lands.

- **`create_principal` / `create_tenant` / `revoke_api_token` don't wrap
  INSERT+SELECT in a tx** (`lib.rs:174-195, 197-213, 346-356, …`).
  Inconsistent with `create_room` (`:624-638`). If SELECT fails after
  INSERT, caller gets an error and retries, creating a duplicate
  (mitigated by UNIQUE where present; `create_tenant` has none).

- **`MetadataStore` derives `Clone` but is a shared serialized handle**
  (`lib.rs:71-75`). The `Clone` implies independent handle semantics
  that don't reflect reality: every clone shares one connection mutex.
  Document the semantics or wrap in a builder returning
  `Arc<MetadataStore>`.

- **`detach_room` is a quiet no-op for already-detached rows**
  (`lib.rs:819-832`). `COALESCE(detached_at, ?1)` updates the row even
  if already detached. For presence tracking a duplicate detach
  shouldn't republish. Return a `bool` indicating new detach.

- **Bind-value / SQL-text leak into audit rows: VERIFIED CLEAN.** The
  audit schema (`schema.rs:242-261`) carries only `action`, `target`,
  `target_id` (i64), `status`, `result_code`, `row_count`,
  `error_message`, `correlation_id`. No column for SQL text or bind
  parameters. `record_query_history` stores `sql_text` separately,
  gated by `metadata.store_sql` config. The AGENTS.md rule holds. The
  only place user-controllable strings reach the audit row is
  `error_message` — the contract comment ("Sanitized failure message")
  is correct.

### Refactor — large files

- **`crates/driver-sqlserver/src/lib.rs` (1,526 LoC)** should mirror
  PG's 5-module split: `conn.rs` / `stream.rs` / `decode.rs` /
  `schema.rs` / `bulk.rs` / `quoting.rs`. Coupling is minimal
  (`MssqlInner` is the only shared mutable state); the split is
  mechanical but unblocks parallel review/editing and makes diff
  history readable.

- **`crates/metadata/src/lib.rs` (1,922 LoC)** has obvious cohesion
  boundaries: `identity` / `connections` / `rooms` / `documents` /
  `history` / `audit` / `saved_queries`. The repeated `*_from_row` and
  `*_by_id_locked` helpers (~120 lines) are nearly identical; a
  `#[derive(FromRow)]` macro or a `fn get_by_id<T>(table, id, mapper)`
  would compress them.

- **`crates/client-sdk/src/lib.rs`** still missing methods for some
  routes despite the DTO-sharing refactor. Audit reach.

---

## Cross-cutting checks that came back clean

- **Clippy + tests:** `cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo test --workspace` both green at HEAD.
- **Bind values never persisted** to audit rows (see P2-metadata note).
- **ChaCha20-Poly1305** uses a fresh 96-bit random nonce per persist;
  wrong-key decrypt path is test-covered; Argon2id parameters meet
  OWASP tiers (though Argon2id is the wrong algorithm for token verify
  — see P1-meta-2).
- **Constant-time bearer compare**, **correlation-id propagation on WS
  handler tasks**, **drain gate** (`/v1/ready`), **protocol-version
  middleware** — all behave as claimed and are test-covered.
- **`crates/core` is genuinely empty**, **`crates/doc` is a non-CRDT
  apply-op wrapper** as documented — not broken, deferred convergence.
- **Redaction / SQL fingerprinting** behaves as claimed
  (`operation_trail_is_fingerprinted_and_secret_free`).
- **CI vs live-test env vars**, **OpenAPI vs router (39 paths each,
  empty set-difference)**, **feature-flag coherence**, **cargo-deny**
  — all clean.

---

## Suggested sequencing

1. **P1-lock-1** (reduce global operation-log lock scope) —
   eliminates a global serialization point.
2. **P1-alloc-1** (`page.clone()` in pump) — single-line change,
   removes ~10M allocs per large query.
3. **P1-comp-1 / P1-comp-5 / P1-comp-9** (lowercase precompute +
   memoize tokenize + protocol `Cow`) — the difference between current
   autocomplete and "Zed-class."
4. **P1-io-1 / P1-io-2** (spill I/O `spawn_blocking`) — prevents worker
   stalls on evicted cursors.
5. **P1-driver-1** (PG close-leak) — silent resource leak on
   close-mid-stream cycles.
6. **P1-meta-1 / P1-meta-2** (connection pool + HMAC tokens) — the
    scalability ceiling for any multi-user deployment.
7. **Refactor splits** (http.rs / mssql lib.rs / metadata lib.rs) —
    mechanical, unblock future review. Do last.

The two themes (sync I/O on async, per-row allocation) are worth
graduating into ADRs in `docs/DECISIONS.md` so the patterns don't
recur: an "async-boundary discipline" ADR codifying where `spawn_blocking`
is required, and a "hot-path allocation budget" ADR codifying that the
row-streaming path must not allocate per cell.
