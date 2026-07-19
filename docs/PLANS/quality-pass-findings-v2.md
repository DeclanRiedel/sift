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
   cell for column names only used in error arms, repeated candidate
   string construction per keystroke. Individually small, together
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

---

## P1 — perf or correctness under the documented workload

### Hot-path allocation (server)

### Sync I/O on async path

### Schema cache

### Completion hot path (the "Zed-class snappiness" goal)

#### P1-comp-9. `push_keywords` allocates `label`/`insert` Strings even for static `&'static str` — RESOLVED
- File: `crates/completion/src/rank.rs:94-101`
- Detail: `kw.to_string()` for static string literals, forced by
  `CompletionCandidate.label`/`.insert` being `String`
  (`protocol/src/completion.rs:48-49`).
- **Why it mattered:** for a 90-keyword list with 20 prefix matches per
  keystroke, 40 allocations that could be zero.
- **Fix applied:** `CompletionCandidate.label`/`.insert` are now
  `Cow<'static, str>`. `push_keywords` hands back `Cow::Borrowed(*kw)`
  for both fields and `push_functions` borrows the label
  (`insert` stays `Cow::Owned` for the `f(` form) — zero allocation for
  every keyword match and every function label. Dictionary-derived
  candidates (schemas, columns, tables) construct `Cow::Owned` via
  `.into()`, identical cost to before. `Cow<'static, str>` round-trips
  through serde (deserializes to `Owned`) and `schemars` (delegates to
  the `str` schema), so the wire format and OpenAPI are unchanged.
  Covered by the existing completion/autocomplete suites (green).

### Drivers — new findings (all 11 v1 items confirmed fixed)

### Metadata scalability ceiling

#### P1-meta-1. Single `Connection` behind `std::sync::Mutex` serializes all metadata access — RESOLVED
- File: `crates/metadata/src/lib.rs`
- Detail: one SQLite connection, one mutex. Every read and every write
  across every spawn_blocking task contended on this lock. Concurrent
  blocking tasks serialized once they reached the metadata store.
- **Why it mattered:** SQLite in WAL mode supports concurrent readers, but
  the single-connection design forfeited that entirely. A long-running read
  (`list_operation_audit(limit=...)` over a growing table) blocked every
  write and every other read for its duration. Under a burst of
  `GET /v1/metadata/rooms` the latency floor was `(N requests) ×
  (per-request query time)`, not `(N / R) × query time`.
- **Fix applied:** file-backed stores now use a small hand-rolled WAL
  connection pool (`ConnectionPool`); each metadata call checks out its own
  connection for the duration of the operation, so reads run concurrently
  and writers serialize only via SQLite's own `busy_timeout`. The `idle`
  mutex is held only to pop/push a connection, never across a query.
  Connections are created on demand (checkout never blocks) with up to
  `MAX_IDLE_CONNECTIONS` (16) kept warm; live connections are naturally
  capped by Tokio's bounded blocking pool. In-memory stores keep the single
  mutex-guarded connection (a second `open_in_memory` is a different empty
  DB, so it cannot be pooled). A `Backend`/`ConnHandle` deref shim keeps the
  ~45 call sites backend-agnostic. Covered by
  `pooled_store_writes_visible_across_connections` and
  `pool_reuses_idle_connections`.
  - Chose a hand-rolled pool over `r2d2_sqlite`/`deadpool-sqlite` to avoid a
    second `rusqlite` version in the tree (the workspace pins 0.32 with the
    `bundled` feature) and because the logic needed is ~60 lines. `deadpool`
    is async; metadata runs in `spawn_blocking`, so a sync pool is the fit.

#### P1-meta-4. Audit row not written in the same transaction as the mutation — RESOLVED
- Files: `crates/metadata/src/lib.rs` mutating methods; audit goes
  through `server/session.rs` / `server/http.rs`
- Detail: every mutating method committed its own tx and returned. The
  server then called `push_metadata_operation`, which sent
  `NewOperationAudit` over an mpsc to the audit-writer thread — separate
  connection, separate tx.
- **Why it mattered:** if the process crashed between commit and audit
  write, the audit trail had a gap for a mutation that did happen.
  Auditable in *intent* but not *durably recorded*. The window was small
  but real.
- **Fix applied** (scope: the three security-critical mutations, per
  ADR-019):
  - `delete_connection_profile`, `set_per_user_credential`, and
    `revoke_api_token` now take a `NewOperationAudit` and `INSERT` it
    **inside the same SQLite transaction** as the mutation, via a shared
    `insert_operation_audit_row` helper (`revoke_api_token` was wrapped in
    an explicit tx; the other two already had one). The mutation and its
    audit row commit atomically or not at all.
  - The audit INSERT reuses the exact statement the async writer uses, so
    the persisted row is identical regardless of which path wrote it.
    `record_operation_audit` was refactored onto the same helper.
  - On success the HTTP handlers call the new
    `SessionStore::push_operation_local` (gated by an internal
    `DurableAudit::AlreadyWritten` flag), which records the in-memory ring
    + JSONL replay entry but **skips** the async durable enqueue — the row
    is already durable, so enqueuing again would double-write it.
    Exactly-once holds because the two paths are mutually exclusive per
    operation. `correlation_id` is captured in the request task before any
    `spawn_blocking` hop (it would not survive the thread change).
  - Failure behavior is unchanged: the tx (audit row included) rolls
    back, and the handler's `?` short-circuits before recording, matching
    prior behavior (these mutations did not audit failures before).
  - The broader "make *every* mutation transactional" option (outbox) was
    considered and deferred; the tradeoff is documented in
    **ADR-019** (`docs/DECISIONS.md`). Covered by the existing metadata
    and server suites (green).

#### P1-meta-5. Audit-writer thread shares the single connection via unbounded mpsc — RESOLVED
- Files: `crates/server/src/session.rs`,
  `crates/metadata/src/lib.rs` (`record_operation_audit`)
- Detail: same mutex, different thread. Channel was `std::sync::mpsc` —
  unbounded.
- **Why it mattered:** (1) under load, if the writer stalled on the mutex,
  the channel grew without bound — memory growth under pressure.
  (2) When the audit writer ran its INSERT, every request-path
  metadata call blocked behind it. Compounded P1-meta-1.
- **Fix applied:**
  - The audit writer's INSERT runs on its own pooled connection (it checks
    one out per call, like every other file-backed metadata call — see
    P1-meta-1), so in WAL mode it no longer contends on the request path.
    (In-memory stores share the single connection — no separate DB is
    possible, and contention is a file-backed/production concern only.)
    An initial fix used a dedicated `reopen()` connection; that was
    subsumed by the P1-meta-1 pool and removed.
  - `set_audit_store` now uses a bounded `sync_channel(1024)`; the send
    site uses `try_send` and drops+counts on overflow (logged via
    `audit_dropped`), so a stalled writer can't grow the queue.

### Memory bounds / task supervision

### Lock contention

#### P1-lock-1. Global audit/operations Mutex serializes every operation — RESOLVED
- File: `crates/server/src/session.rs`
- Detail: `audit: Mutex<Vec<AuditEntry>>`, `operations: Mutex<OperationLog>`
  — both process-global. `list_audit` and `list_operations` cloned the
  entire Vec under the lock.
- **Why it mattered:** for the 10,000-entry cap that was 10,000 clones
  while every concurrent operation waited. Worse, at the cap each push did
  a `Vec::drain(0..1)` that memmoved ~10k elements. Every operation across
  every session acquired `operations`.
- **Fix applied:** introduced `RingLog<T>` = `Mutex<Arc<VecDeque<T>>>`.
  Append is O(1) amortized (`push_back` + a single `pop_front` at the cap);
  a read clones the `Arc` under the lock and materializes the `Vec`
  *outside* it, so a `list` never blocks appends for the length of the
  copy. `Arc::make_mut` copies once on the first append after a snapshot is
  handed out — reads are rare (admin endpoints), so that copy is paid at
  most once per read. The JSONL operation-log writer moved out of the
  ring's mutex into its own immutable field, so a `list_operations`
  snapshot never contends with it. Covered by `ring_log_*` unit tests plus
  the existing read-while-write stress test.

#### P1-lock-2. `select_victims` full clone + N mutex locks on every `wrap` at cap — RESOLVED
- File: `crates/server/src/cursors.rs:398-420`
- Detail: cloned the cursor-id list, then acquired `last_ack` Mutex once
  per cursor.
- **Why it mattered:** only fired at the per-session cap, but for a
  session at the 32-cursor cap that was 32 mutex acquisitions plus a
  Vec clone on the open path.
- **Fix applied:** `CursorState.last_ack` is now an `AtomicU64` LRA rank
  instead of a `Mutex<Instant>`. A registry-wide `Inner::clock`
  (`AtomicU64`) hands out a monotonic tick on cursor creation and on
  every `touch`; the lowest rank is the LRA victim. `select_victims`
  reads each rank with a single relaxed atomic load and no longer clones
  the id list — it iterates the `per_session` shard guard directly and
  drops it before the caller's `evict` takes `get_mut` on the same map.
  Relaxed ordering is sufficient (the value only picks a victim, it does
  not synchronize). `touch` drops from a mutex to one relaxed store.
  Covered by `per_session_cap_evicts_oldest` and `touch_updates_lra_rank`
  (both green; ordering now comes from the monotonic clock rather than
  wall-clock `Instant`, so the tests no longer depend on sleep timing).

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
  wrong-key decrypt path is test-covered.
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

1. ~~**P1-lock-1** (reduce global operation-log lock scope)~~ — DONE
   (RingLog snapshot); eliminated a global serialization point.
2. ~~**P1-comp-9** (protocol `Cow`)~~ — DONE; keyword/function candidates
   no longer allocate per keystroke.
3. ~~**P1-meta-1** (metadata connection concurrency)~~ — DONE (WAL
    connection pool); was the scalability ceiling for multi-user.
4. ~~**P1-lock-2** (cursor LRA atomic)~~ — DONE; lock-free victim
    selection on the cursor-open path.
5. ~~**P1-meta-4** (transactional audit for security-critical
    mutations)~~ — DONE; see ADR-019. Closes the crash window for
    profile-delete / set-credential / token-revoke.
6. **Refactor splits** (http.rs / mssql lib.rs / metadata lib.rs) —
    mechanical, unblock future review. All P1s are now resolved; these
    P2 refactors are the remaining large items.

The two themes (sync I/O on async, per-row allocation) are worth
graduating into ADRs in `docs/DECISIONS.md` so the patterns don't
recur: an "async-boundary discipline" ADR codifying where `spawn_blocking`
is required, and a "hot-path allocation budget" ADR codifying that the
row-streaming path must not allocate per cell.
