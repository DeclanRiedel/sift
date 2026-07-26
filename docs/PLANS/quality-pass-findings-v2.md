# Repo-wide quality pass v2 — open findings

Read-only re-review across `crates/server`, both driver crates,
`crates/metadata`, `crates/protocol`, `crates/doc`, `crates/core`,
`crates/completion`, `crates/server/src/autocomplete.rs`, and
`crates/protocol/src/completion.rs`.

**All P1 findings and the whole P2 correctness / reliability / security set are
resolved** (see git history: metadata WAL connection pool, in-tx audit for
security-critical mutations, bounded audit writer on its own connection, RingLog
snapshot instead of clone-under-lock, cursor LRA atomic, MSSQL decode-errors
surfaced + transactional bulk insert, PG prewarm spawned off `open`, export
routed through the cursor registry, `MetadataError::Io` split out, …). What
remains below is open P2 hygiene, deferred scaling notes, and test/benchmark
gaps.

Two systemic themes worth graduating into ADRs so the patterns don't recur:

1. **async-boundary discipline** — codify where `spawn_blocking` is required.
2. **hot-path allocation budget** — the row-streaming path must not allocate
   per cell.

---

## Open — correctness / behavior

- **PG type coverage gaps** (`driver-postgres/src/decode.rs:34-62`). Arrays,
  `JSONPATH`, network types (CIDR/INET/MACADDR/MACADDR8), range types, XML,
  MONEY, HSTORE, TIMETZ fall through to a `Value::Engine` placeholder. Additive.

Resolved in the Phase I readiness pass: untyped PostgreSQL null parameters no
longer masquerade as text, SQL Server requires `Value::TypedNull`, and native
SQL Server cancellation tombstones the connection with stable
`Code::ConnectionInvalidated` semantics. These wire additions advanced the
application protocol to version 2.

## Open — hygiene / transactions (metadata)

- **V006 is a destructive migration with no backout** (`V006__rooms.sql:1-3`).
  `DROP TABLE IF EXISTS …`. Fine pre-release; document before any beta user has
  a DB they care about.

Resolved in the Phase I readiness pass: saved-query filters use one placeholder
style, principal and tenant creation are transactional, duplicate room detach
is explicit, shared `MetadataStore` clone semantics are documented, unusable
broker profiles are rejected at admission, and principal keys/challenges are
live authentication paths.

## Open — scaling notes (fine today, flagged)

- **`FileSecretStore` O(N) write amplification per mutation** (`secrets/file.rs:55-122`).
  Every put/delete clones the whole map, serializes, encrypts, writes, fsyncs.
  Fine at single-tenant IDE scope.
- **No prepared-statement caching in metadata** (`lib.rs` uses `prepare`
  everywhere; `prepare_cached` is available — ~100% hit after warmup with the
  pooled connections). PG/MSSQL prepared-statement caches are also unmanaged for
  ad-hoc IDE workloads; bounding the PG one means hooking connection recycle
  (deadpool-postgres 0.14 has no capacity setter) — **deferred**.
- **`room_runtime.rs:93-101` full clone + sort per presence event**;
  **`close_session` fans out one spawn per connection** (`session.rs:400-408`,
  use a bounded `JoinSet`); **`reject_if_connection_has_tx` O(N) scan per
  execute** (`session.rs:1075-1102`). All fine at current N.
- **`handle_ws` rejects concurrent execute on one socket** (`http.rs:2840-3061`);
  clients must open multiple sockets. Worth a note in the protocol doc.

## Open — completion (the "Zed-class snappiness" goal)

- **O(N²) schema dedup** (`dictionary.rs:55-58`) — dedupe into a `HashSet`.
- **`format!` per matching column / object candidate** (`rank.rs:182-186, 234-236`)
  — same `Cow<'static, str>` fix that resolved P1-comp-9.
- **Unchecked `as u32` truncating casts** (`lib.rs:42-43`) — clamp or 400 on
  overflow.
- **`tokenize().unwrap_or_default()` swallows lex errors** (`context.rs:40-43`) —
  an empty token Vec misclassifies as `Statement`. At least `tracing::debug!(?err)`.
- **`ExpectingColumn { qualifier: Some(_) }` returns zero candidates** when the
  qualifier is a CTE / alias / temp table (`rank.rs:43-53`) — fall back to the
  unqualified-column path.
- **Over-eager `[` quote-absorption** (`context.rs:165-170`) — corrupts
  `replaced_range` for MSSQL `arr[0]` subscripts. Restrict to MSSQL / verify no
  close-quote ahead.
- **Magic scoring constants** (`rank.rs:243-245`) — promote to named `const`s.
- **Engine-agnostic ident grammar** (`context.rs:175-177`) — `is_ident_byte`
  allows `c >= 0x80` regardless of engine.
- **No keystroke-path benchmarks** and **many test gaps**: direct `detect_context`
  tests, substring / case-insensitive fallbacks, MSSQL keyword+function tables,
  `resolve_qualified` / `quote_ident_if_needed` edge cases, SQL inside string
  literals/comments. **Worst:** `complete_dotted_returns_columns` does not verify
  the deep fetch ran — `MockDriver::schema` ignores its `_scope`, so the test
  passes even if the deep-fetch+merge path breaks. Add criterion benches with a
  CI regression budget.

## Open — driver / test infrastructure

- **Mock driver can't assert on `sql` / `params`** (`driver-api/src/mock.rs:295-418`).
  Records only method names; accepts everything real drivers reject;
  `MockDriver::savepoint` returns `TxId(0)` rather than `t.tx_id`.

## Open — large-file refactors ("do last")

- **`crates/server/src/http.rs` (~7,000 LoC)** → `router.rs` / `middleware.rs` /
  `auth.rs` / `metadata_handlers.rs` / `session_handlers.rs` / `ws.rs` /
  `openapi.rs`, and generate the OpenAPI blob from `schemars`.
- **`crates/driver-sqlserver/src/lib.rs` (~1,800 LoC)** → mirror PG's
  conn / stream / decode / schema / bulk / quoting split.
- **`crates/metadata/src/lib.rs` (~7,300 LoC)** → identity / connections / rooms /
  documents / history / audit / saved_queries; compress the near-identical
  `*_from_row` / `*_by_id_locked` helpers.
- **`client-sdk` still missing methods for some routes** — audit reach.
