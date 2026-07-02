# sift — Driver Layer: Status & Forward Plan

> Status snapshot after the P0/P1 changelist (commit pending). Companion
> to `DRIVER_TRAIT.md` (the spec) and `PHASE0.md` (the build order). Scope
> of this doc is **only the driver layer** (`protocol` + `driver-api` +
> `driver-postgres` + `driver-sqlserver`); server substrate, HTTP/WS surface,
> sessions, OpenAPI, and client-sdk are out of scope here and tracked in
> `PHASE0.md`.

## What changed in this changelist

| Area | Change | Verification |
| --- | --- | --- |
| `MockDriver` (driver-api) | Programmable canned-result driver behind `mock` feature for server-substrate tests. | Compiles + clippy clean with `--features mock`. |
| Conn-state race | Dropped the separate `tx_index` map; single `Mutex<HashMap<conn_id, ConnState>>` with `InTx` carrying `tx_id` inline. Iteration for tx lookup is O(conn_count), fine at Phase 0. | Live PG tests cover commit/rollback/savepoint flows. |
| Pool caching by spec | `pools: DashMap<SpecHash, Arc<Pool>>`; two opens of equivalent specs hit the same pool. Hash is on the canonical serde-JSON form of `ConnectionSpec`. | `transaction_*` tests reuse the cached pool across opens. |
| Cancel-token cleanup on close | Cursors map now stores `(conn_id, CancelToken)`; `close()` drains cursors belonging to the conn. | `close_mid_query_does_not_panic` test. |
| Decode error surfacing | Replaced `unwrap_or(None)` with `tracing::warn!` + `DriverWarning` push + `Value::Engine { display_text: "<decode error>" }`. | Decode path unit-tested indirectly via live tests. |
| Panic catching in query task | `run_query` wrapped in `AssertUnwindSafe::catch_unwind`. Panic produces `Page::Done` with diagnostic instead of silent channel close. | Manual review; integration test would need a forced panic. |
| `affected_rows` on DML | `execute()` now dispatches on leading keyword: SELECT/WITH/TABLE/VALUES/SHOW/EXPLAIN → streaming extended-protocol path (no affected count, matches semantics); everything else → `simple_query` whose `CommandComplete(u64)` carries the count directly (no tag parsing needed in 0.7.18+). | `execute_dml_reports_affected_rows` test asserts `Some(2)` for an UPDATE. |
| Deep schema completeness | Deep pass now queries `pg_attribute` (with type OID), `pg_index` (PK detection, indexes with `amname` and partial predicates), `pg_constraint` (PK/FK/UNIQUE/CHECK), `pg_trigger` (with `tgtype` bitmask decode for timing + events). | `schema_deep_lists_columns_pk_indexes_constraints` test covers all four. |
| Primary-key detection | `pk_column_set` joins `pg_index` + `pg_attribute` on `indisprimary`; ColumnMetadata `primary_key` flag populated. | Deep schema test asserts `id.primary_key == true`. |
| `ObjectKind` extensions | Added `ForeignTable` (relkind `f`), `PartitionedTable` (relkind `p`). Shallow pass now maps both. | Manual review. |
| Filter pushdown | `SchemaFilter.name_pattern` is pushed to PG as a LIKE clause (glob `*`/`?` → SQL `%`/`_`). | `schema_shallow_pushes_down_name_filter` test. |
| `Value::Interval` | Protocol slot exists. Decode path still falls through to `Value::Engine` for `Type::INTERVAL` (tokio-postgres has no `chrono::Duration` FromSql without a wrapper). | Variant declared, not yet decoded. |
| `ObjectInfo` extensions | Added `indexes: Vec<IndexInfo>`, `constraints: Vec<ConstraintInfo>`, `triggers: Vec<TriggerInfo>` plus supporting enums (`IndexKind`, `ConstraintKind`, `TriggerTiming`, `TriggerEvent`). | Deep schema test populates all three. |
| Live PG test harness | `crates/driver-postgres/tests/live_pg.rs` gated behind `live-pg` feature. 10 tests covering every Driver verb + cancel + close mid-query. Per-test unique schema so tests parallelise cleanly. | 10/10 pass against PG 17.10 via nix-shell socket. |

**Final tally:** 3778 LOC across the workspace (was 2326). 18 tests pass
(8 unit + 10 integration). clippy `-D warnings` clean in dev + release.
cargo fmt clean.

## What's solid (proven or structurally sound)

| Property | Evidence |
| --- | --- |
| Trait is object-safe | `Box<dyn Driver>` / `Arc<dyn Driver>` used in the spec's registry pattern; no method generics, no associated types in signatures. |
| Anti-JDBC-LCD shape | Core trait holds only the 8 verbs every engine supports; engine-specific ops (LISTEN/NOTIFY, MARS, advisory locks, savepoints) live on `PgExt` / `MssqlExt` declared in `driver-api`. No engine-specific type appears in the core trait signature. |
| Type escape hatch | `TypeRef::Primitive` (IDE render vocab, 17 variants) + `TypeRef::Engine { engine, name, category }` (carries native name verbatim). Categorization uses `tokio_postgres::types::Kind`, not name-suffix heuristic. |
| Streaming + backpressure | `execute()` returns `ResultSetStream` wrapping `tokio::sync::mpsc::Receiver<Page>` (buffer 64). Producer task blocks on send when consumer is slow. |
| Cross-task cancel | `cursors: DashMap<CursorId, (conn_id, CancelToken)>` populated at execute start, drained at execute end and on close. `cancel()` reads by cursor id and fires — does not need to coordinate with the execute task. ADR-013 satisfied. |
| Error model | `pg_err()` maps SQLSTATE class prefixes to stable `Code`. Raw text preserved in `message`; native code preserved in `engine_sqlstate`. |
| Progressive schema | `SchemaScope { depth: Shallow|Deep }`. Shallow = one pg_catalog round-trip (names+kinds, system schemas excluded, name_pattern pushed to LIKE). Deep = `pg_attribute` + `pg_index` + `pg_constraint` + `pg_trigger` joined into a complete object description. |
| Identifier safety | `validate_ident()` gates every engine-specific op (savepoint names) against SQL injection; pure-function unit-tested. |
| Workspace hygiene | 7-crate workspace, shared deps via `[workspace.dependencies]`, `protocol` has zero I/O deps (ADR-004 honored). clippy `-D warnings` clean in dev and release. |
| Test coverage | 8 unit tests (pure functions) + 10 integration tests against real PG 17.10 covering every Driver verb, cancel, close mid-query, parallel-safe via per-test schemas. |
| Driver isolation | Every Driver method safe inside `tokio::spawn`; query tasks wrapped in `catch_unwind`; cancel callable cross-task. ADR-013 satisfied. |

## Gaps remaining

### P1 — correctness / feature parity

| Gap | Why it matters | Fix |
| --- | --- | --- |
| **Numeric/decimal surfaces as `Value::Engine`** | PG `numeric` is the canonical arbitrary-precision type. tokio-postgres 0.7.18 has no `bigdecimal` feature; binary decode needs a third-party crate or hand-rolled decode of PG's numeric wire format. | Either add `postgres-bigdecimal` (or similar) dep and wire `Type::NUMERIC` → `Value::Decimal(string)` in `decode_value`, or fall back to a `SELECT col::text` rewrite at the driver layer when binary decode fails for numeric OID. Tier 1 #15 work. |
| **SSL `VerifyCa` / `VerifyFull` downgraded to `Require`** | Security regression. Without cert verification, MITM attacks succeed. | Wire a real TLS connector (`tokio-postgres-rustls` + `rustls`) into `pool_for`'s `create_pool` call. Map our `VerifyCa`/`VerifyFull` to the matching variant. Backend Tier 0 #12. |
| **`Value::Interval` not decoded** | Protocol variant exists but PG `Type::INTERVAL` falls through to `Value::Engine`. | Wrap `chrono::Duration` (or define a `PgInterval { months, days, micros }` newtype for month-aware fidelity), implement FromSql, extend `decode_value`. |
| **`LISTEN/NOTIFY`, `COPY`, advisory locks stubbed in `PgExt`** | Trait methods exist but return `UnsupportedForEngine` for PG itself — semantically wrong. | `listen_pool` on `PgDriverInner` (dedicated pool because LISTEN is conn-stateful); `LISTEN $1` SQL + dedicated connection task reading notifications into `mpsc::Sender<PgNotification>`. COPY via `client.copy_in`/`copy_out`. Advisory locks via `pg_advisory_lock`/`unlock`. |

### P2 — design debt / future-proofing

| Gap | Why it matters | Fix |
| --- | --- | --- |
| **ConnHandle dropped `Weak<dyn Driver>` backref** | Documented deviation from spec. Server-side registry is in scope everywhere a ConnHandle is used today, so this is latent. | Add `OnceLock<Weak<dyn Driver>>` to `ConnHandleInner`; server sets it after wrapping in `Arc<dyn Driver>`. Add a round-trip test. |
| **Sequential-per-conn access** | `take_for_op` returns "conn busy" if another op is in flight. PG enforces this anyway; SQL Server with MARS allows concurrency. Server-side session manager must queue per-conn ops. | Document the contract in `Driver::execute`. For MARS later, `MssqlExt::set_mars` toggles concurrent-execution mode that relaxes the `Taken` slot constraint. |
| **No prepared-statement cache** | `query_raw` re-parses SQL on every call. For IDE usage (autocomplete, schema probes) the savings are real but not large. | Wrap `Client::prepare()` + small LRU keyed by SQL hash. Or rely on PG's plan cache. |
| **`ObjectKind::Extension` declared but never populated** | PG extensions are first-class catalog objects (`pg_extension`). Schema tree ignores them. | Add to Shallow pass via `SELECT extname FROM pg_extension`. |
| **`ObjectKind::PartitionedTable` declared but Shallow filter still excludes child partitions** | Shallow filters `relkind IN ('r','v','m','S','f','p')` — partitions themselves are relkind 'r' but inherit from a 'p' parent. | Optional: surface partition children under their parent, or list as standalone. |
| **Channel buffer size hard-coded to 64** | Backpressure tuning for large-result streaming (Tier 2 #20). | Make configurable via `PgDriverInner.cfg`. |
| **No tracing instrumentation on Driver methods** | `tracing` is a dep but unused. Observability gap. | `#[tracing::instrument(skip(self))]` on every Driver method, span per cursor id. |

### P3 — polish

| Gap | Fix |
| --- | --- |
| FK target columns not surfaced | `ConstraintInfo.references` carries the table name but not the referenced columns. Extend `query_constraints` to pull `confkey` and resolve to column names. |
| Trigger column scope missing | `TriggerInfo.columns` is always empty. `pg_trigger.tgattr` carries UPDATE OF column list. |
| Index column expressions vs names | Computed indexes (e.g. `LOWER(email)`) surface as `pg_get_indexdef` text rather than a clean column name. Currently dropped; surface as the expression string. |
| Typefacet richness for PG | `PgColumnFacets` declared but never populated in the Deep pass. Carry OID, enum values, array dims. |
| Test for forced panic in run_query | The `catch_unwind` path is reviewed but not exercised by a test. Hard to trigger naturally. |

## Roadmap (next — driver layer only)

The five original P0/P1 items are closed. New work in dependency order:

1. **Numeric decode + `Interval` decode.**
   Pick a `bigdecimal` or `rust_decimal` dep (or a hand-rolled wire-format
   decoder). Wire both `Type::NUMERIC` and `Type::INTERVAL`. Add integration
   test that inserts and reads back both types.

2. **TLS connector for `VerifyCa`/`VerifyFull`.**
   `tokio-postgres-rustls` + `rustls`. Build a `MakeTlsConnector` wrapper.
   Integration test against a PG instance with self-signed cert.

3. **`LISTEN/NOTIFY` real impl in `PgExt`.**
   Dedicated `listen_pool`; long-running task per LISTEN that emits
   `PgNotification` through an `mpsc::Sender`. Test via a NOTIFY from a
   second conn.

4. **`COPY` real impl in `PgExt`.**
   `client.copy_in` for `COPY ... FROM STDIN`, `copy_out` for `COPY ... TO
   STDOUT`. Streams bytes through `CopyResult::bytes`/rows. Test with a
   small CSV round-trip.

5. **Advisory locks real impl in `PgExt`.**
   `pg_advisory_lock`/`unlock` (and the try-variants). Test concurrent
   lockers via two opens of the same spec.

After these five, the `PgExt` trait stops returning `UnsupportedForEngine`
for any PG-native operation. The driver layer is then feature-complete for
Phase 0; SQL Server (PHASE0 step 15) and server substrate (steps 5–13) can
proceed without driver-layer churn.

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

When the five roadmap items above are merged, on top of the closed
changelist:

- ✅ Every Driver method on `PgDriver` is exercised against real PG by an
  automated test.
- ✅ `MockDriver` exists for server-substrate unit tests.
- ✅ No two-lock races in connection-state management.
- ✅ Pools are reused across opens of the same spec.
- ✅ Numeric + interval types decode natively.
- ✅ DML results report `affected_rows`.
- ✅ Deep schema pass surfaces PK / FK / indexes / constraints / triggers.
- ✅ Cancel-token map cleans up on close.
- ✅ Decode errors are surfaced as warnings, not silently dropped.
- ✅ TLS `VerifyCa` / `VerifyFull` work end-to-end.
- ✅ `PgExt` methods are real impls, not `UnsupportedForEngine` stubs.
- ✅ `tracing` spans cover every public method.

At that point: PG impl is genuinely solid. SQL Server impl (step 15) and
server substrate (steps 5–13) can proceed in parallel without the driver
layer being a source of bugs.

## How to run the integration tests locally

```text
# Inside the nix dev shell:
initdb -D /tmp/sift-pg -U sift --auth=trust --no-locale --encoding=UTF8
printf 'port = 5433\nunix_socket_directories = '\''/tmp/opencode/sift-pg-socket'\''\n' >> /tmp/sift-pg/postgresql.conf
mkdir -p /tmp/opencode/sift-pg-socket
pg_ctl -D /tmp/sift-pg -l /tmp/sift-pg.log -w start
psql -h /tmp/opencode/sift-pg-socket -p 5433 -U sift -d postgres -c 'CREATE DATABASE sifttest;'

# From the repo root:
cargo test -p sift-driver-postgres --features live-pg --test live_pg
```

Tests parallelise cleanly via per-test unique schema names; no
`--test-threads=1` required.
