# sift — Driver Layer: Status & Forward Plan

> Status snapshot after commit `83ce9cf` (driver layer scaffolding). Companion
> to `DRIVER_TRAIT.md` (the spec) and `PHASE0.md` (the build order). Scope of
> this doc is **only the driver layer** (`protocol` + `driver-api` +
> `driver-postgres` + `driver-sqlserver`); server substrate, HTTP/WS surface,
> sessions, OpenAPI, and client-sdk are out of scope here and tracked in
> `PHASE0.md`.

## What's solid (proven or structurally sound)

| Property | Evidence |
| --- | --- |
| Trait is object-safe | `Box<dyn Driver>` / `Arc<dyn Driver>` used in the spec's registry pattern; no method generics, no associated types in signatures. |
| Anti-JDBC-LCD shape | Core trait holds only the 8 verbs every engine supports; engine-specific ops (LISTEN/NOTIFY, MARS, advisory locks, savepoints) live on `PgExt` / `MssqlExt` declared in `driver-api`. No engine-specific type appears in the core trait signature. |
| Type escape hatch | `TypeRef::Primitive` (IDE render vocab, 17 variants) + `TypeRef::Engine { engine, name, category }` (carries native name verbatim). Categorization uses `tokio_postgres::types::Kind`, not name-suffix heuristic. |
| Streaming + backpressure | `execute()` returns `ResultSetStream` wrapping `tokio::sync::mpsc::Receiver<Page>` (buffer 64). Producer task blocks on send when consumer is slow. |
| Cross-task cancel | `cursors: DashMap<CursorId, CancelToken>` populated at execute start, drained at execute end. `cancel()` reads by cursor id and fires — does not need to coordinate with the execute task. ADR-013 satisfied. |
| Error model | `pg_err()` maps SQLSTATE class prefixes (`08*` conn, `28*` auth, `57014` cancel, `42601` syntax, `42P01/42704/42883/42P02` undefined, `42P04/42710/42701/42723` duplicate, `22*` data, `57/58/XX*` internal) to stable `Code`. Raw text preserved in `message`; native code preserved in `engine_sqlstate`. |
| Progressive schema | `SchemaScope { depth: Shallow|Deep }` — Shallow = one pg_catalog round-trip (names+kinds, system schemas excluded); Deep = `information_schema.columns` for one object. Matches Zed §2.2. |
| Identifier safety | `validate_ident()` gates every engine-specific op (savepoint names) against SQL injection; pure-function unit-tested. |
| Workspace hygiene | 7-crate workspace, shared deps via `[workspace.dependencies]`, `protocol` has zero I/O deps (ADR-004 honored). clippy `-D warnings` clean in dev and release. |
| Pure-function test coverage | 8/8 unit tests pass: `validate_ident` accept/reject paths, `begin_sql` isolation/access synthesis (incl. Snapshot→RepeatableRead mapping), `pg_type_to_type_ref` primitive collapse + Engine fallback + Kind-based Array category. |

## Gaps, prioritized

### P0 — blocks any claim of "the driver works"

| Gap | Why it matters | Fix |
| --- | --- | --- |
| **No integration tests against real PG** | Entire PG impl is structurally checked, not behaviorally. Never executed a query. | PHASE0 step 14: testcontainers-based `#[ignore]` tests gated behind a `live-pg` feature or env var. Cover: open, ping, schema Shallow, schema Deep, begin/commit, begin/rollback, savepoint/rollback_to, execute single-statement SELECT, execute DML (verify row count once affected_rows lands), execute multi-statement batch, cancel mid-query, close. |
| **`take_in_tx` two-lock race** | `tx_index.lock().await.remove(&tx_id)` releases before `conns.lock().await` acquires. Another caller could observe inconsistent state. Single-user Phase 0 hides it; multi-user surfaces it. | Either unify into a single `Mutex<HashMap<conn_id, ConnState>>` with `tx_id` indexed inside ConnState, or use a `RwLock` over a unified structure. Prefer the former; the perf cost is negligible at Phase 0 conn counts. |

### P1 — erodes correctness or feature parity

| Gap | Why it matters | Fix |
| --- | --- | --- |
| **`affected_rows: None` on every `Page::Done`** | DDL/DML results lose their row count. The protocol field exists; the impl never fills it. | For non-SELECT queries, route through `simple_query()` to capture `SimpleQueryMessage::CommandComplete`'s row count. Heuristic: branch on statement leading keyword, or always try simple_query first and fall back to `query_raw` if a row stream is observed. Surface via `Page::Done { affected_rows: Some(n), .. }`. |
| **Numeric/decimal surfaces as `Value::Engine`** | PG `numeric` is the canonical arbitrary-precision type. Clients render `<undecoded numeric>` for every decimal cell. | Enable `tokio-postgres`'s `with-bigdecimal-04` (or `rust_decimal` if preferred). Extend `decode_value()` for `Type::NUMERIC` → `Value::Decimal(string)`. |
| **Per-open ad-hoc pool** | Each `Driver::open()` builds a fresh deadpool-postgres pool (max_size 8) that lives until the conn closes. Two opens of the same spec = two pools, no sharing. Wasteful and breaks warm-pool semantics (BACKEND Tier 1 #15). | `PgDriverInner.pools: DashMap<SpecHash, Arc<deadpool_postgres::Pool>>`. `open()` looks up first, builds if missing, increments a refcount. `close()` decrements; pool drops when refcount hits 0. |
| **`primary_key` always false** | `col_to_metadata` and `col_from_info_schema_row` both hardcode `primary_key: false`. Schema tree shows no PK info. | For Deep pass, join `pg_constraint` (contype = 'p') + `pg_attribute` to set the flag on PK columns. For Shallow, leave as false (out of scope). |
| **PK / FK / UNIQUE / CHECK / indexes / triggers missing from Deep pass** | `ObjectInfo.columns` is the only populated field. The Deep pass should be a complete object description. | Extend `ObjectInfo` with `indexes: Vec<IndexInfo>`, `constraints: Vec<ConstraintInfo>`, `triggers: Vec<TriggerInfo>` (add these to `protocol`). Query `pg_constraint`, `pg_indexes`, `pg_trigger` scoped to the target object. |
| **SSL `VerifyCa` / `VerifyFull` downgraded to `Require`** | Security regression. Without cert verification, MITM attacks succeed. | Enable `deadpool-postgres`'s TLS feature (`rustls` or `native-tls`) in the workspace dep, map our `SslMode::VerifyCa`/`VerifyFull` to the matching variant. Backend Tier 0 #12 (TLS termination) depends on this. |
| **Decode errors silently swallowed** | `stream.rs`: `row.try_get(i).unwrap_or(None)` — a decode failure becomes `Value::Null` with no diagnostic. Hides driver bugs. | On `try_get` error, log via `tracing::warn!`, emit `Value::Engine { display_text: "<decode error: ...>" }`, optionally add a `DriverWarning` to the page stream. |

### P2 — design debt / future-proofing

| Gap | Why it matters | Fix |
| --- | --- | --- |
| **ConnHandle dropped `Weak<dyn Driver>` backref** | Documented deviation from spec. Server-side registry is in scope everywhere a ConnHandle is used today, so this is latent. If a future server design has tasks holding ConnHandles without the registry, this regresses. | Add `OnceLock<Weak<dyn Driver>>` to `ConnHandleInner`; server sets it after wrapping in `Arc<dyn Driver>`. Add a test that exercises the backref round-trip. |
| **Sequential-per-conn access** | `take_for_op` returns "conn busy" if another op is in flight on the same conn. PG enforces this anyway (one query per conn at a time); SQL Server with MARS allows concurrency. Server-side session manager must queue per-conn ops, not parallelize blindly. | Document the contract in `Driver::execute`. For MARS later, `MssqlExt::set_mars` toggles a per-conn concurrent-execution mode that relaxes the `Taken` slot constraint. |
| **Foreign tables, partitioned tables collapse to `ObjectKind::Table`** | PG `relkind = 'f'` (foreign) and `'p'` (partitioned) both map to Table. Lossy. | Add `ObjectKind::ForeignTable`, `ObjectKind::PartitionedTable` to protocol; map in `shallow_tree`. |
| **Schema filter not pushed down** | `SchemaScope.filter` is parsed but never used in the introspection SQL. Client filters happen in Rust post-query. | For Shallow, push `name_pattern` into a `WHERE c.relname LIKE $1` clause. Skip catalog/schema filtering (PG has one catalog). |
| **No prepared-statement cache** | `query_raw` re-parses SQL on every call. For an IDE's repeated identical queries (autocomplete lookups, schema probes), this matters. | Wrap `Client::prepare()` + a small LRU keyed by SQL string. Or rely on PG's query plan cache (sufficient for most cases). |
| **`ObjectKind::Extension` declared but never populated** | PG extensions are first-class catalog objects (`pg_extension`). Schema tree ignores them. | Add to Shallow pass via `SELECT extname FROM pg_extension`. |
| **No `Value::Interval` variant** | Declared in `PrimitiveType::Interval` but no Value variant and no decode path. PG intervals surface as `Value::Engine`. | Add `Value::Interval(chrono::Duration)` (or `PgInterval` newtype for month-aware), wire `Type::INTERVAL` in `decode_value`. |
| **LISTEN/NOTIFY, COPY, advisory_lock stubbed in PgExt** | Trait methods exist but return `UnsupportedForEngine` for PG itself — semantically wrong ("PG doesn't support LISTEN" is false). | Implement: `listen_pool` on `PgDriverInner` (dedicated pool because LISTEN is conn-stateful); `LISTEN $1` SQL + dedicated connection task reading notifications into `mpsc::Sender<PgNotification>`. COPY via `client.copy_in`/`copy_out`. Advisory locks via `pg_advisory_lock($1, $2)` / `pg_advisory_unlock($1, $2)`. |

### P3 — polish

| Gap | Fix |
| --- | --- |
| Panic in spawned query task kills stream silently | Wrap `run_query` body in `catch_unwind`; on panic, emit `Page::Done { warnings: [panic_msg] }` before the channel closes. |
| No observability hooks | `tracing::instrument` on every public Driver method, span per cursor id. Already have `tracing` in deps; just not used. |
| No `MockDriver` in `driver-api` | Add behind a `mock` feature. Lets server-substrate tests run without a real DB. Critical path for steps 5–13 of PHASE0. |
| Cancel-token map leaks entries on conn close mid-query | `remove_conn` doesn't drain `cursors` for entries belonging to the conn. Add: `cursors.retain(|_, _| ...)` keyed by a side index `conn_id → Vec<cursor_id>`. |
| Channel buffer size hard-coded to 64 | Make configurable via `PgDriverInner.cfg`. Backpressure tuning for large-result streaming (Tier 2 #20). |

## Roadmap (next 5 — driver layer only)

In dependency order. Each item is one PR-sized unit.

1. **Integration test harness against real PG.**
   `crates/driver-postgres/tests/live_pg.rs` gated behind `feature = "live-pg"`.
   Spins up Postgres via testcontainers (or `nix develop` + pg_ctl for local
   runs). Covers every Driver verb end-to-end. This is the gate that lets
   every subsequent change claim to be "tested."

2. **`MockDriver` in `driver-api`.**
   `feature = "mock"`. Programmable: each method returns a queued result or
   error. Lets the server bootstrap (PHASE0 step 5) and session manager (step
   6) land and be unit-tested without a real DB. Required before any
   server-substrate work.

3. **`take_in_tx` race fix + unified conn-state map.**
   Single `Mutex<HashMap<conn_id, ConnState>>` where `ConnState` carries an
   optional `tx_id`. Drop the side `tx_index`. Eliminates the two-lock window.
   Small, contained refactor with the integration test as the safety net.

4. **Pool caching by spec + numeric decode + `affected_rows` via simple_query.**
   Three correctness fixes that together move the impl from "compiles" to
   "actually correct for the common case." Each independently shippable; lump
   them because they're all "the impl was a placeholder here" gaps. Integration
   test gates the lot.

5. **Deep schema completeness (PK / FK / indexes / constraints).**
   Extends `protocol::ObjectInfo` and queries `pg_constraint` + `pg_indexes`
   in the Deep pass. Required for FEATURES Tier 1 #11 (autocomplete) and Tier
   2 #30 (table designer).

After these five, the driver layer is in shape to support the server
substrate (PHASE0 steps 5–13) and the SQL Server fast-follow (steps 15–16).

## Out of scope (tracked in PHASE0.md, not here)

- Server bootstrap, axum app, Tower middleware, figment config (step 5).
- Session + connection manager at the server level (step 6).
- HTTP / WS surface (steps 9–10).
- Auth middleware, OpenAPI generation, client-sdk reference consumer
  (steps 11–13).
- Live-co-edit CRDT layer (Tier 3, much later).
- GPUI desktop client (Phase 1).
- Real SQL Server impl via tiberius (step 15).

## What "the driver layer is done" looks like

When the five roadmap items above are merged:

- ✅ Every Driver method on `PgDriver` is exercised against real PG by an
  automated test.
- ✅ `MockDriver` exists for server-substrate unit tests.
- ✅ No two-lock races in connection-state management.
- ✅ Pools are reused across opens of the same spec.
- ✅ Numeric types decode to `Value::Decimal`, not `Value::Engine`.
- ✅ DML results report `affected_rows`.
- ✅ Deep schema pass surfaces PK / FK / indexes / constraints.
- ✅ Cancel-token map cleans up on close.
- ✅ Decode errors are surfaced as warnings, not silently dropped.

At that point: PG impl is genuinely solid. SQL Server impl (step 15) and
server substrate (steps 5–13) can proceed in parallel without the driver
layer being a source of bugs.
