# Alternative-Tech Counterfactual — Where Non-Rust Beats Rust On Product Quality

> **Status: counterfactual exploration, not a current decision.** This note
> ignores `AGENTS.md`, `docs/DECISIONS.md`, and every locked ADR. The single
> evaluation criterion is **end-user-visible product quality and performance**.
> Development cost, execution risk, contract stability, and "we already wrote N
> lines of Rust" are explicitly out of scope. Nothing here is a recommendation
> to act — it is a map of where the grass is actually greener if those costs
> did not exist.
>
> Companion to `docs/DECISIONS.md` (the constraints this ignores) and
> `docs/PLANS/server-build-list-v2.md` (the code these recommendations touch).

## Framing

Two technologies come up. They attack different product dimensions and they
compound:

- **Elixir / BEAM** wins on the *lifecycle* of multi-user sessions — process
  isolation, supervision, presence, streaming fanout, clustering, hot reload.
  These map almost 1:1 onto sift's room/presence/streaming problems.
- **Nix-style content-addressing** (the *philosophy*, not the build tool —
  applied to app data) wins on the *identity* of the data sessions produce —
  verifiable results, truthful history, tamper-evident audit, query lineage.

Both push against the default "mutable state in rows identified by
autoincrement" model that RDBMS-centric apps inherit. The default is easy, not
good — and for a collaboration-native, audit-first, local-first product it is
actively the wrong shape in several places.

## Where to switch (area by area, code-grounded)

### 1. Room runtime / presence / WS fanout → Elixir + Phoenix

**Current.** `crates/server/src/room_runtime.rs:1-102` — hand-rolled
`DashMap<i64, Arc<RoomRuntimeRoom>>` with `broadcast::channel(1024)` for room
events, manual `attach`/`detach`/`presence_for`. Presence is a `DashMap` of
struct clones sorted by attachment id; there is no reconnect story and a slow
consumer just gets dropped when the broadcast buffer overflows.

**Switch to.** A BEAM supervision tree: one `DynamicSupervisor` per room, a
`GenServer` per member attachment, `Phoenix.PubSub` for fanout,
`Phoenix.Presence` for presence tracking. The room's lifecycle (spawn members
on attach, tear down on room close, restart on crash) becomes declarative.

**Product gains.**
- Presence that survives flaky networks (mobile, bad wifi). Phoenix Presence
  does CRDT-ish merges across reconnects; the current `broadcast::channel`
  drops receivers on overflow and has no merge/rejoin path.
- A wedged query or panicking handler in room A **cannot** degrade room B *by
  construction* (process isolation + supervision), vs by discipline today.
- Streaming query results to N room viewers with independent failure
  semantics — query process, DB-socket process, each viewer's WS process are
  independent actors. A viewer dropping does not pause the stream for others.
- Horizontal clustering for hosted mode is built in (`node()`, EPMD,
  distributed processes). The current single-node Tokio model has no
  clustering story at all.

### 2. Driver-call lifecycle / timeout / cancel isolation → BEAM supervision

**Current.** `crates/server/src/session.rs:125-143` (`run_bounded`) and
`:391-428` (`cancel_after_timeout`) — hand-composed `tokio::spawn` +
`tokio::time::timeout` + an `Arc<Mutex<Option<CursorId>>>` cursor slot so a
timed-out HTTP execute can still cancel the in-flight cursor. Every driver
verb re-implements this envelope. The "wedged driver cannot freeze the server"
rule (AGENTS.md) is enforced per call site by hand.

**Switch to.** Each query/verb as a supervised `GenServer` with its own
`Process.send_after/3` timeout, `Process.exit/2` for cancel, supervisor
restart semantics for crash isolation.

**Product gains.**
- Crash isolation is structural. A panicking driver task cannot poison the
  scheduler or any sibling; today it is caught per call via
  `catch_unwind` in the driver, which is convention, not guarantee.
- **Per-process GC** — this is the one that maps directly onto goal #4
  ("Zed-class snappiness"). A connection that just streamed a 10M-row result
  does not pollute the GC of a connection doing `SELECT 1`. BEAM heaps are
  per-process; Tokio's shared allocator has no such isolation. Under mixed
  fast/slow query load this is the difference between flat latency and
  tail-unwinding.
- **Hot-code reload** — upgrade a hosted sift *without dropping active queries
  or evicting room members*. No Rust equivalent exists; the only option is
  blue-green with a cutover. For a B2B IDE where a query runs 20 minutes and a
  mid-query kickout is real harm, this is a sellable operational property.

### 3. Audit log → hash-chained, content-addressed

**Current.** `crates/server/src/session.rs:166-206` — in-memory `Vec` capped
at 10,000 rows, newline-delimited JSON append to an optional file.
`OperationAuditEntry { at, operation, status }`. No correlation id (flagged in
the build list), no tamper-evidence, no content spine.

**Switch to.** Each entry embeds `hash(prev_hash || canonical(content))`,
forming a chain. Entry content includes the query content-hash (see #4/#5) so
audit is anchored to the exact bytes that ran, not a prose description.

**Product gains.**
- **Tamper-evident audit** that holds up to compliance scrutiny (finance,
  healthcare, regulated industries). Any retroactive edit breaks the chain
  detectably. SQLite rows cannot make this claim.
- Cryptographic provenance for every operation: "this result came from this
  query at this time" becomes a verifiable claim rather than a hope.

### 4. Query-result storage → content-addressed blob store

**Current.** Results are ephemeral streams. `crates/server/src/session.rs:929-
982` (`drain_stream`) materializes up to `MAX_HTTP_EXECUTE_ROWS = 10_000` rows
into a `Vec<Row>` and returns them; nothing is persisted, nothing is
shareable after the fact.

**Switch to.** Every result set is a content-addressed blob keyed by
`hash(sql_text || param_shapes || schema_fingerprint || engine_version)`.
Stored once, referenced by hash.

**Product gains.**
- **"Share my results with a teammate" = share a hash.** The recipient fetches
  the blob and can *verify it matches the query* — no "did you tweak the
  numbers before sending." Verifiable result sharing is a feature Navicat and
  DataGrip do not have.
- Reproducibility for compliance: "this dashboard came from query X at time T
  against schema S" becomes a content-addressed, checkable claim.
- Cross-user dedup. Two analysts running the same `SELECT * FROM users` every
  morning store it once.

### 5. Schema snapshots → content-addressed, referenced by query history

**Current.** Schema is fetched on demand per connection
(`crates/server/src/session.rs:323-334`) and not snapshotted or versioned
alongside query history.

**Switch to.** Schema snapshots stored as CAS blobs; every query-history entry
references the schema-hash it ran against.

**Product gains.**
- **History that tells the truth.** `SELECT name FROM people` meant something
  different before and after the `ALTER TABLE`. Every existing history tool
  pretends the schema is constant and lies as a result. Pinning the
  schema-hash fixes this at the data-model layer.
- Blast-radius analysis: "what breaks if this column changes type?" answerable
  by walking which historical queries reference which schema hashes.

### 6. Saved queries → content-addressed with git-style provenance

**Current.** `saved_query` is dead schema — created by migration, never read
or written by any Rust code (`docs/PLANS/server-build-list-v2.md` flags this).

**Switch to.** Saved-query identity = content hash; a parent-pointer DAG
records forking ("I took your query, changed the WHERE clause").

**Product gains.**
- **Query forking** works like git. Organizational query knowledge becomes a
  first-class, forkable artifact.
- Auto-dedup org-wide: three people independently writing the same query
  surface as one canonical version.
- Query lineage: "who based work on this query?" answerable by walking the
  DAG — a collaboration feature, not a storage detail.

### 7. Document / CRDT store → content-addressed blob store (git object model)

**Current.** `crates/doc/src/lib.rs:1-181` — UTF-8 byte buffer + apply-op,
explicitly a placeholder for "future Loro/Automerge plumbing" (line 3). The
`CrdtKind { Loro, Automerge }` enum already exists but is decorative; neither
backend is wired.

**Switch to.** Document state as content-addressed op blobs. Sync becomes
"send me blobs with these hashes I lack" — deduped, resumable, network-
efficient, offline-capable.

**Product gains.**
- **Offline-capable collaboration that actually converges** — the honest
  meaning of "collaboration-native," not just "two cursors at once."
- Free history, free branching, no central authority over identity.
- This is orthogonal to the CRDT-library choice (Loro vs Automerge); it is
  about the storage/sync substrate underneath whichever is picked. It should
  be decided *before* the CRDT library, because it constrains the choice.

## Keep in Rust (where switching regresses product quality)

Honesty matters here — this is not "rewrite everything."

- **Driver wire protocol + row decode** (`crates/driver-postgres`,
  `crates/driver-sqlserver`). CPU-bound, binary parsing, zero-copy decoding of
  typed columns, NUMERIC/DECIMAL precision handling. BEAM would be a
  measurable regression: GC pauses on the hot decode path, no zero-copy, no
  type-level guarantees on protocol correctness. **Keep.**
- **Schema introspection computation** (deep schema SQL + decode in each
  driver). Pure compute, Rust wins.
- **Hashing / compression / large-result materialization.** 5–20× faster than
  BEAM. Matters precisely for the large result sets the CAS in #4 would store.
- **Protocol crate** (`crates/protocol`). Pure serde data — language-agnostic
  by construction (ADR-003 is right regardless). No quality gain from
  switching languages; Rust is fine.
- **Native client SDK bindings.** Rust→wasm / native FFI is excellent. Keep.

## Considered and rejected as marginal

- **Haskell / OCaml for SQL analysis** (query normalization, equivalence
  detection for #6's dedup). Functional languages are well-suited to symbolic
  manipulation, but `sqlparser-rs` is competent and the gain over a Rust
  implementation is small. Not worth a third runtime.
- **Go for the collaboration layer.** Goroutines are cheap, but Go's
  supervision/reconnect/hot-reload story is strictly weaker than BEAM's. If
  the collab layer is going to switch, switch to BEAM, not Go.
- **JS/TS for the server-side CRDT.** Yjs is JS and mature, but only matters
  *client-side* (the browser editor). On the server, a CAS substrate (#7) +
  whichever CRDT core compiles to Rust (Loro, Automerge) is the better fit.

## Idealized target architecture (if dev cost were zero)

Three layers, each technology where its strength lives:

1. **Rust driver / decode / compute layer.** The current `driver-api`,
   `driver-postgres`, `driver-sqlserver`, `protocol` crates, essentially
   unchanged. Where work is CPU-bound and type-correctness on binary protocols
   matters.
2. **BEAM / Elixir collaboration & supervision layer.** Rooms, presence,
   query-lifecycle orchestration, streaming fanout, HTTP/WS surface,
   clustering. Replaces `crates/server/src/room_runtime.rs` wholesale and the
   orchestration portions of `session.rs`.
3. **Content-addressed blob store** underneath both. Results, schema
   snapshots, saved queries, document state, audit entries. Replaces the
   in-memory `Vec` audit (`session.rs:166-206`), the ephemeral result streams
   (`session.rs:929-982`), and the dead `saved_query` table.

Wiring: the Rust layer exposes a thin native service surface (in-process via
FFI, or a localhost Unix-socket sidecar). The BEAM layer owns all client-
facing connections and orchestrates Rust work units as supervised processes.
The CAS is shared infrastructure both read and write. The protocol crate
stays pure serde and is consumed unchanged by both layers.

That product would be categorically different from the current row-id-SQLite +
Tokio-everywhere default: rooms that never freeze each other, collab that
works offline and converges, results you can cryptographically trust, history
that does not lie about schema changes, queries that fork like git, audit that
holds up in court, and a hosted tier that scales and upgrades without
infrastructure milestones.

The reason not to build it is execution cost and contract stability — which
is exactly what `docs/DECISIONS.md` is correctly optimized for. This note
exists to keep the *product-quality ceiling* visible while those constraints
are honored.
