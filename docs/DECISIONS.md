# sift — Architectural Decisions

This file keeps only current, load-bearing decisions. Reference material
(feature checklist, Zed-lessons rationale) lives under `docs/legacy/`; the
code-grounded ordered backlog is `docs/PLANS/server-build-list-v2.md`.
Candidates ADR-011 through ADR-022 are listed there against their phase.

Format is ADR-lite: **Context · Decision · Consequences.**

---

## ADR-001 — The Server Is The Product

**Context.** Database IDE behavior spans connections, credentials, sessions,
schema, execution, history, audit, and collaboration. Putting that logic in a
window process would make hosted and multi-client modes bolt-ons.

**Decision.** `sift-server` owns product behavior. Clients are renderers and
automation consumers over the public HTTP/WebSocket protocol. The backend lab is
a development workbench, not the product UI.

**Consequences.** The server can be tested headlessly and reused by future
desktop, web, and automation clients. The protocol must stay stable,
versioned, and explicit.

---

## ADR-002 — Shared Crates Stay UI-Free

**Context.** Desktop and web product clients may use different UI stacks, while
server, protocol, drivers, metadata, document, and SDK crates need to remain
portable and testable.

**Decision.** UI dependencies do not enter shared crates. Product clients map
protocol/server data into their own UI models at their crate boundary.

**Consequences.** UI decisions remain reversible without changing backend
contracts. Some edge mapping code is expected in each product client.

---

## ADR-003 — Protocol Is Pure Serde Data

**Context.** The server, SDK, backend lab, and future clients all need the same
wire contract.

**Decision.** `sift-protocol` contains serde/schemars data types only: request
and response structs, operation enums, WebSocket messages, and stable error
codes. It has no I/O, Tokio, filesystem, or server dependencies.

**Consequences.** The protocol is easy to version and inspect, and can be used
from native and wasm consumers. Server-internal types must be adapted at the
boundary.

---

## ADR-004 — Tokio Async Server And Drivers

**Context.** Database work is I/O-bound, query streams need backpressure, and
the public API includes HTTP plus WebSocket streams.

**Decision.** The server, drivers, and SDK use Tokio. Synchronous metadata
SQLite work is isolated behind bounded blocking work.

**Consequences.** Driver and streaming code can remain async end-to-end.
Blocking components need explicit isolation and backpressure.

---

## ADR-005 — Pure-Rust Database Driver Stack Where Possible

**Context.** Native ODBC stacks add packaging friction, especially in Nix and
cross-platform environments.

**Decision.** PostgreSQL uses `tokio-postgres`; SQL Server uses `tiberius`.

**Consequences.** Builds stay reproducible and mostly Rust-native. SQL Server
features not exposed by `tiberius` are evaluated individually instead of
pulling in ODBC by default.

---

## ADR-006 — Local-First, Hosted-Capable

**Context.** Single-user local usage should be easy, but hosted collaboration
must use the same product model.

**Decision.** The same server binary supports local-first mode and hosted mode
through config. Local bootstrap creates a personal tenant/principal, while
remote/hosted modes use explicit auth.

**Consequences.** Local and hosted paths share code. Auth and metadata runtime
hardening can advance without changing the product model.

---

## ADR-007 — Rooms Are The Collaboration Unit

**Context.** Earlier workspace/tab planning does not map cleanly to shared
documents, presence, and room-scoped history.

**Decision.** A room is the durable collaboration boundary under a tenant:
members, documents, attachments/presence, and query history are scoped through
rooms.

**Consequences.** Single-user local mode is a one-member room. Multi-user mode
adds members and attachments without changing the core model.

---

## ADR-008 — Secrets Stay Out Of SQLite

**Context.** Connection profile metadata needs persistence, but credentials
should not be stored in the metadata database.

**Decision.** SQLite stores opaque secret handles only. Secret bytes live behind
`SecretStore`.

**Consequences.** Metadata remains portable and inspectable. Secret backend
quality can improve independently from schema and route design.

---

## ADR-009 — Operation Audit Is A First-Class Contract

**Context.** Collaboration, replay, diagnostics, and command surfaces all need
a durable vocabulary of user-visible actions.

**Decision.** Public user actions are represented as `Operation` variants or
metadata operation entries and are recorded in the operation audit.

Audit records are **sanitized before they are stored** on any surface (the
in-memory `/v1/operations` view, the JSONL log, and the durable
`operation_audit` table): SQL text is reduced to a normalized fingerprint
(`sqlfp:…`), execute bind values are cleared, connection passwords are
redacted, and bulk payloads are dropped. The audit trail is therefore a record
of *what happened*, not a verbatim source that can replay query bodies with
their original data. Raw SQL for a user's own history lives only in
`query_history` (no bind values), and can itself be reduced to a fingerprint
with `metadata.store_sql = false`.

**Consequences.** New product actions should add protocol-visible operation
shape instead of disappearing into ad hoc handler logic. Anything sensitive a
new `Operation` variant carries must be added to the audit sanitizer, so the
trail never becomes a secret/bind-value sink. Full-fidelity replay of query
bodies is intentionally out of scope for the audit trail.

---

## ADR-010 — Product UI Is Deferred Until The Headless Layer Is Stable

**Context.** The backend lab can test routes and workflows, but it is not a
production client. A product UI should not drive backend architecture before
the headless layer is stable.

**Decision.** Desktop/web product UI work starts after the headless server,
metadata, room runtime, and protocol contract are stable enough to consume.

**Consequences.** The next product-client decision can choose desktop, web, or
both from a stable backend foundation instead of freezing backend design early.

---

## ADR-013 — Driver Isolation and Wedged-Driver Containment

**Context.** Drivers run engine-specific, panic-prone code (tiberius,
tokio-postgres, decode paths) behind the object-safe `Driver` trait. A driver
that panics, hangs, or leaves a connection in an undefined state must not take
down the server or wedge unrelated requests. Two engines with different
cancellation capabilities — PostgreSQL cooperative backend cancel, SQL Server
task-abort plus connection discard (ADR-017) — must clear the same bar.

**Decision.** The containment boundary has three layers, and both engines meet
all three:

1. **No driver work runs inline on the request path.** Every synchronous driver
   call the server makes (ping, schema, execute, bulk insert, transactions,
   savepoints, reconnect-open) is dispatched on a spawned task bounded by the
   per-request timeout. A wedged or slow call surfaces `Code::QueryTimedOut`
   and frees the handler instead of blocking it. Streaming execute additionally
   runs the query producer on its own task with one-page backpressure.

2. **Panics are caught, not propagated.** A panic in driver work never unwinds
   across the trait boundary. tokio isolates a spawned task's panic from the
   process, and the server maps the resulting `JoinError` to
   `ApiError::Internal`. On the streaming path each driver wraps its query task
   in `catch_unwind` (PG `run_query`, SQL Server `execute`) and emits a terminal
   `Page::Error { DriverInternal }`, so the consumer sees a clean diagnostic
   rather than a silently dropped channel.

3. **A cancelled or broken connection leaves nothing reusable.** PG cancel uses
   the backend cancel token; SQL Server cancel aborts the query task and
   discards the connection, because tiberius exposes no safe out-of-band
   attention API (ADR-017). Neither hands a poisoned connection to a later
   request. Idempotent reads (ping/schema) may transparently re-establish a
   broken connection once; mutating work never auto-retries.

The policy is defined by the guarantee, not the mechanism. A new driver must
dispatch on a task, catch panics on its streaming path, and ensure no reusable
connection survives a cancel; how it cancels is its own choice.

**Consequences.** A wedged, panicking, or connection-dropping driver degrades
one request, not the process. Server-side timeout plus spawn discipline is the
primary containment; per-driver `catch_unwind` is a diagnostics refinement on
the streaming path. Engine parity is the guarantee, so PG's cooperative cancel
and SQL Server's abort-and-discard both satisfy it without a
lowest-common-denominator trait. A driver that blocks inline, lets panics
escape, or reuses a post-cancel connection is an ADR violation, not just a bug.

---

## ADR-016 — Protocol Versioning and Negotiation

**Context.** `PROTOCOL_VERSION` is a bare string (`"1"`) emitted in the
`x-sift-protocol-version` response header but never read from requests. A
client built against a future, incompatible wire contract would hit confusing
partial failures instead of a clear signal, and there was no way to reject an
unsupported client.

**Decision.** The protocol version is a single monotonically increasing integer
string, not semver. It bumps only on a *breaking* wire change; additive changes
do not.

- Breaking (bump): removing or renaming a field or endpoint, changing a field's
  type or meaning, changing an existing enum variant's shape, or tightening
  validation so previously valid requests fail.
- Additive (no bump): new endpoints, new optional request fields with defaults,
  new response fields, new enum variants existing clients can ignore.

Negotiation is pin-or-proceed. A request may pin the version via the
`x-sift-protocol-version` header. If present and it does not equal the server's
version, the request is rejected before routing with `400` and error kind
`unsupported_protocol_version`, naming the requested and supported versions. If
absent, the request proceeds — an unpinned client is assumed compatible — and
the server always advertises its version on the response. There is no range
negotiation while a single version exists; a future multi-version window
extends this by accepting a set and defining a deprecation window (N and N-1).

**Consequences.** A client pinned to a version the server no longer speaks
fails fast with an actionable error instead of subtle misbehavior. Additive
evolution stays cheap — most changes never touch the version. The check is a
cheap header comparison in middleware. Unpinned clients keep working, so the
gate is opt-in until a breaking change makes pinning meaningful.

---

## ADR-017 — Driver Trait Lock After Two Real Implementations

**Context.** The server now has real PostgreSQL and SQL Server drivers behind
the same `Driver` trait. Phase A's purpose was to prove the trait shape before
the public protocol is treated as stable enough for GUI and third-party
clients. The remaining Phase A ambiguity was not about more verbs; it was about
which engine-specific capabilities belong in extension traits, how portable
values are represented, and which backend limitations are explicit
unsupported states.

**Decision.** The Phase A driver contract is locked around the core eight
verbs: `open`, `ping`, `schema`, `begin`, `commit`, `rollback`, `execute`,
`cancel`, and `close`. The trait remains object-safe: `&self` receivers,
boxed async futures via `async_trait`, concrete protocol-crate request/response
types, and handle structs rather than associated connection types. Engine-only
features stay in extension traits selected through `as_pg()` and `as_mssql()`;
wrong-engine calls produce `UnsupportedForEngine` at the server boundary.

`ConnHandle` remains an opaque id plus engine tag and does not carry a
`Weak<dyn Driver>` back-reference. The server's connection registry is the
ownership boundary for routing cancel/close/transaction work. A future backref
would be a new design item, not part of the Phase A lock.

The portable value union is intentionally not a lowest-common-denominator
schema. Decimal values are represented as canonical strings in
`Value::Decimal(String)` to avoid binary floating-point rounding and preserve
arbitrary precision across PostgreSQL `numeric` and SQL Server
`decimal`/`numeric`/money-like values. Intervals use `Value::Interval` only
when they can be represented as `chrono::Duration`; month-aware PostgreSQL
intervals fall through to `Value::Engine` with display text because a month is
calendar-relative and cannot be represented as a fixed duration. SQL Server has
no matching interval primitive.

TLS has two separate boundaries. Driver-side TLS to user databases is owned by
the concrete driver and connection spec: PostgreSQL maps `SslMode` through
rustls/native roots for verify modes, while SQL Server uses tiberius TDS
encryption plus `TrustServerCertificate`. TLS termination for sift's own
HTTP/WebSocket listener is a server deployment concern and is not implied by
driver TLS.

SQL Server parity is locked to what tiberius and the current protocol can
support cleanly: core verbs, schema including shallow objects/triggers/index
kinds, CSV bulk import, `USE`, and savepoint/rollback-to-savepoint. Runtime
MARS toggling is not in `MssqlExt`; MARS is a connection-time setting and is
currently rejected because the driver/session model allows one active stream
per connection. SQL Server native bulk-load is not represented by the Phase A
`BulkOp`, which carries CSV bytes; native TDS bulk needs typed rows and column
metadata and must use a future request shape if it graduates.

PostgreSQL cancellation uses the backend cancel token. SQL Server cancellation
is implemented as task abort plus connection discard because tiberius does not
expose a public TDS attention API that can be safely sent from a different
task while the query owns the socket. The server removes the SQL Server
connection after cancel so the orphaned backend session cannot be reused.

Driver pooling is not part of the trait signature. PostgreSQL may satisfy
`open()` from a cached pool; SQL Server currently dials one backend session per
handle. Pool warmth and preconnect policy are Phase C performance work and do
not change the Phase A trait shape.

Any future change to a locked core driver signature, handle semantics, portable
value representation, or public operation/request shape requires an explicit
ADR update and a protocol-version bump. Adding a new extension method is
allowed only when the unsupported behavior is already explicit and existing
clients continue to receive the same response shape.

**Consequences.** Server code can depend on a stable two-engine driver
contract without pretending every backend exposes the same native features.
Known SQL Server limitations are explicit unsupported states rather than
stubs. Performance work can improve pooling and warm starts without reopening
the trait lock, while true protocol shape changes remain gated.

---

## ADR-018 — Graceful Shutdown Contract

**Context.** `shutdown_signal` only resolves the axum graceful-shutdown
future, which stops accepting new TCP connections and waits for in-flight HTTP
requests. It does not stop accepting new *logical* work (sessions,
connections, queries), does not bound how long draining may take, and does not
deterministically cancel or persist anything. A long-running or wedged query
could hold shutdown open indefinitely, or be dropped mid-flight with a driver
connection left in an undefined state. Hosted operation needs a defined,
bounded shutdown sequence.

**Decision.** On the first termination signal (SIGINT/SIGTERM), the server runs
a fixed sequence before the listener closes:

1. **Stop accepting new work.** A process-wide drain flag flips to draining.
   New sessions and new connections are rejected with `503 Service
   Unavailable` (error kind `service_draining`). In-flight requests and queries
   on existing sessions continue.
2. **Mark readiness false.** `/v1/ready` (readiness split, next step) reports
   not-ready while draining so external routers stop sending traffic.
   `/v1/health` stays liveness-only and keeps returning ok until the process
   exits.
3. **Drain in-flight queries until a deadline.** The server awaits the
   in-flight query count reaching zero, bounded by
   `config.timeouts.shutdown_drain_secs` (default 30). Each individual query is
   already bounded by the per-query request timeout (ADR-lite step 1), so the
   drain deadline is a ceiling, not the common case.
4. **Cancel remaining cursors.** Queries still running at the deadline are
   abandoned: the listener closes and axum drops their request tasks. Per-query
   timeouts and connection close reclaim driver-side work, and the SQL Server
   discard-on-cancel rule (ADR-017) still applies to any cursor cancelled on
   the way out. A global cursor registry that issues an explicit `cancel` to
   every straggler is a documented follow-up; today the per-query deadline is
   the backstop.
5. **Flush durable state.** Room document CRDT state is persisted to the
   metadata store on every applied operation, so there is no separate
   room-snapshot buffer to flush; presence is ephemeral and intentionally
   dropped. Metadata SQLite writes are durable at each call.
6. **Exit.** The shutdown future returns, axum stops the listener, and the
   process exits.

The drain state lives in a `Shutdown` handle carried in `AppState`, separate
from `SessionStore` and `RoomRuntime`, because both the HTTP layer (rejection,
readiness) and the shutdown driver (await, deadline) share it.

**Consequences.** Shutdown is bounded and observable: it never blocks forever,
new work is refused deterministically once draining starts, and readiness can
flip so external routers redirect traffic. In-flight queries get a real drain
window rather than being killed immediately. The remaining gap — explicitly
cancelling every straggler cursor at the deadline rather than relying on
per-query timeout plus connection close — is documented and deferred to a
cursor-registry pass. Adding `shutdown_drain_secs` is additive config; no
protocol shape changes.

## ADR-011 — Server-Side Cursor Registry

**Context.** Cursors live inside each driver today (PG `cursors: DashMap`, SQL
Server `cursors: DashMap` of `JoinHandle`). There is no server-side registry,
no per-session cap, no eviction, and no coordination point between the WS ack
loop and future work like predictive prefetch or large-result spill. The
Phase C follow-ups (bounded memory for a 1M-row result, page-N+1 prefetch,
spill to disk) all need a shared place to stand.

**Decision.** A `CursorRegistry` sits in `SessionStore` above the drivers,
proxying every `execute_stream`. The driver still produces a raw
`ResultSetStream`; the registry wraps it, buffers up to `N` pages ahead of
the last-acked seq, exposes `pause` / `resume` / `cancel`, and enforces a
per-session cap.

1. **Per-session cap only, no global cap.** Each `Session` carries
   `max_cursors` (default 32) and a `SessionId → { CursorId → CursorEntry }`
   view lives in the registry. When a session opens a new cursor and it is
   already at cap, the registry evicts one of its own cursors first — never
   another session's. A runaway session hurts only itself.
2. **Idle-first eviction with spill.** Eviction candidates are ranked
   by time-since-last-ack. On eviction the pump task writes any
   still-buffered pages to `{spill_dir}/sift-cursor-{id}.bin`
   (length-prefixed JSON) if `spill_dir` is set AND the footprint
   exceeds `spill_min_bytes` (default 1 MiB), then sends a synthetic
   `Page::Error { code: CursorEvicted, resume_url }` to the consumer.
   The driver-side stream is cancelled through the registry's
   `on_evict` callback, which routes through `driver.cancel(handle,
   cursor)` so the ADR-013 ownership check applies to evictions
   exactly like user-issued cancels. The client resumes by `GET`ing
   the `resume_url` (`/v1/cursors/{id}/pages?from_seq=N`), which
   returns a batch of pages and a `done` flag. Spill files are
   deleted on the final read, on explicit
   `DELETE /v1/cursors/{id}`, or after `spill_ttl` (default 10 min)
   whichever happens first; a background reaper enforces the TTL.
3. **Explicit pause/resume backpressure.** The registry pumps pages off
   the driver's mpsc into a per-cursor buffer bounded by `prefetch_pages`
   (default 2, matching the current 1-ahead behavior plus one for the
   prefetch step). When the buffer is full the pump `await`s a pause
   condvar; the WS ack loop calls `resume(cursor)` after each ack. Pause is
   the primary mechanism; the underlying mpsc `channel(1)` is the backstop.
4. **Cancel goes through the registry.** `SessionStore::cancel` now looks
   up the registry entry, calls `driver.cancel(handle, cursor_id)`, and
   removes the buffer + spill file. The driver-side ownership check
   (ADR-lite from P0 #4) remains authoritative for cross-user protection.
5. **Registry is a server layer, not a trait method.** Drivers stay
   unchanged; they keep producing `ResultSetStream`. The registry lives in
   `crates/server/src/cursors.rs` and is composed into `SessionStore`.
   Adding a trait method would put the eviction/spill policy in every
   driver — the exact spread we're trying to avoid.

**Consequences.** Bounded memory per session becomes a real invariant, not
a hope; a client that leaks cursors caps itself at 32. Spill gives an
evicted-but-still-live cursor a resume path so an idle browser tab does not
lose its results. Backpressure gets a first-class knob that the WS ack loop
already knows about; the mpsc bound remains as a defense against a
misbehaving pump. Drivers stay simple and the ADR-013 driver isolation
boundary is undisturbed. The remaining gap — a global cap for the hosted
tenant story — is documented and left for a hosted-topology ADR (Phase H).

**Scope note — adaptive prefetch depth.** The pump ships with fixed-depth
prefetch (`prefetch_pages`, default 2), which delivers the "page N+1
buffered when the client asks for it" behavior the Phase C plan
originally sketched. **Scaling that depth adaptively based on measured
ack velocity is explicitly out of scope** for this ADR. A future ADR
will introduce it if telemetry shows the fixed depth is a real
bottleneck; until then, operators tune `prefetch_pages` via server
config. This is a deliberate choice not to build a self-tuning knob
before there is measured evidence it moves the needle.

## ADR-012 — Schema Cache with TTL Ceiling and Engine-Specific Invalidators

**Context.** Every `RefreshSchema` / `get_schema` call hit the driver, which
hit the DB — a ~30ms round-trip on the happy path (up to a few hundred ms
for a `deep` scope). Data-tool UIs poll schema on every panel refresh, tree
expand, autocomplete cache warm; the DB round-trip became the dominant
latency in the schema panel. There was no cache and no invalidation
contract.

**Decision.** Introduce a per-spec schema cache above the driver layer.
Key = `(spec_hash, canonical_scope_json)`; the same spec+scope from
different connections shares the cached entry. TTL is 60 seconds by
default and is the ultimate ceiling — every entry is refetched at least
once per TTL regardless of invalidation.

Two engine-specific invalidator strategies run alongside the TTL:

1. **PG: LISTEN/NOTIFY on `sift_schema_change`.** The registry opens a
   dedicated connection per unique spec and calls `PgExt::listen` on the
   fixed channel. The user opts in by installing a DDL event trigger that
   `NOTIFY`s the channel on `ddl_command_end`. Without the trigger, the
   listener is quiet and the TTL alone bounds staleness.
2. **SQL Server: poll `MAX(modify_date)` on `sys.objects` every 30s.**
   Cheap in the steady state (single scalar). On change, invalidate every
   cached entry for that spec.

Invalidator tasks are lifetime-tied to the process — one per unique spec,
spawned lazily on first cache insert, aborted on server drop. If the
dedicated connection fails to open (auth error, DB unreachable) the task
exits quietly and the cache falls back to TTL-only.

Cache lookup returns immediately on hit; miss goes through the existing
`SessionStore::schema` path and inserts the result on Ok. Hit/miss/
invalidation counters are exposed as atomics for metrics.

**Consequences.** The steady-state schema-panel latency drops from a DB
round-trip to a `DashMap.get` — under 1ms. Users on either engine see
snappy schema navigation without any user-visible flag. DDL changes are
reflected as fast as the invalidator observes them (immediately for PG
with the trigger; within 30s for MSSQL); worst case, TTL closes the gap at
60s. Memory cost is bounded by (unique specs) × (unique scopes) × snapshot
size — small in practice. The trade-off: the PG fast path depends on the
user installing the trigger, and MSSQL polling adds a small periodic DB
load per unique spec. The 60s TTL means neither surface is load-bearing —
if either invalidator fails silently, correctness is preserved with at
most 60s of staleness. A future ADR may introduce a coarser
"schema-changed" hint from the client (e.g. after an executed DDL
statement) to invalidate immediately without the trigger dependency.

## ADR-lite — Server-side composition for Phase D headless features

**Context.** Phase D adds three headless features (DDL generation,
autocomplete, and — later — inline-edit DML) that could each be
expressed either as a new `Driver` trait method or composed on the
server over the existing eight verbs. ADR-017 locked the trait around
those eight; every trait addition breaks the lock and forces a protocol
bump.

**Decision.** Compose them server-side. DDL generation
(`crates/server/src/ddl.rs`) established the pattern: fetch what's
needed via `Driver::schema` + `Driver::execute`, format the result in
server code. Autocomplete follows the same rule with one wrinkle —
the SQL context parser is non-trivial and belongs in its own pure-Rust
workspace crate (`sift-completion`) so a future desktop client can
share it without pulling in the server. `sift-completion` depends only
on `sift-protocol` (for wire types + `SchemaSnapshot`) and
`sqlparser-rs` (for tokenization); no I/O, no tokio.

The engine-specific keyword and builtin-function tables originally
called out for `sift-protocol` in the Phase D plan instead live in
`sift-completion::keywords`. Protocol stays pure serde (ADR-004); the
tables aren't wire types, they're data the ranker consumes.

**Consequences.** ADR-017 stays intact — no signature change, no
protocol bump on either feature. `sift-completion` is reusable by the
eventual desktop client and the wasm client (its interface takes a
`SchemaSnapshot`, not a live driver). Inline-edit DML will follow the
same shape when it lands. The trade-off: server-side composition means
the server does work an engine could arguably do faster in-native
(catalog joins on the server side rather than pushed down). For DDL
and autocomplete this is a wash — the DB calls are the same shape
`RefreshSchema` already makes and the cache absorbs them. If a future
feature genuinely needs an engine-native pass (e.g. plan capture), it
graduates to a trait extension via an explicit ADR then, not by
grandfather.

---

## ADR-019 — Audit Durability: Async Best-Effort, Transactional For Security-Critical Mutations

**Context.** ADR-009 makes every user-visible action an audited
`Operation`. *How* the durable `operation_audit` row is persisted was a
separate, unstated tradeoff. The default path is asynchronous: a mutating
metadata method commits its own SQLite transaction, and the server
separately enqueues a `NewOperationAudit` onto a bounded channel that a
dedicated writer thread drains on its own pooled connection (P1-meta-1,
P1-meta-5). This keeps the durable write off the async request path — a
slow disk never stalls a tokio worker — but it opens a window: a crash
between the mutation commit and the audit write leaves an action that
*happened* with no durable audit row. For most operations that window is
acceptable; for security-critical mutations it is not.

**Decision.** Audit durability is **async best-effort by default**. For a
small set of **security-critical mutations** the audit row is instead
written **in the same SQLite transaction as the mutation**, so the two
commit atomically or not at all. Today that set is:

- deleting a connection profile (`delete_connection_profile`)
- setting/replacing a per-user credential (`set_per_user_credential`)
- revoking an API token (`revoke_api_token`)

These metadata methods take a `NewOperationAudit` and `INSERT` it inside
their transaction via the shared `insert_operation_audit_row` helper (the
same INSERT the async writer uses, so the persisted row is byte-identical
regardless of path). On success the HTTP handler records the in-memory
ring + JSONL replay entry through `SessionStore::push_operation_local`,
which deliberately **skips** the async durable enqueue — the row is
already durable, and enqueuing again would double-write it. Exactly-once
holds because the two paths are mutually exclusive per operation.

Failure of these mutations is unchanged: the transaction (audit row
included) rolls back, and — matching prior behavior — the handler's `?`
short-circuits before any audit is recorded. Secret-store cleanup for
profile/credential deletion still happens after commit (the secret store
is not part of the SQLite transaction); only the *audit trail* for the
mutation is made atomic, not the secret I/O.

**Consequences.** The crash window is closed for the mutations where a
missing audit row is a compliance/forensic problem, at the cost of one
extra INSERT inside those transactions (negligible; these are rare
control-plane operations, not the query hot path). All other operations
keep the async best-effort path and its throughput benefit. Adding a
mutation to the security-critical set is a deliberate, reviewable step:
give the metadata method a `NewOperationAudit` parameter, INSERT it in the
tx, and switch the handler to `push_operation_local`. Revisit the default
(e.g. an outbox pattern that makes *every* mutation transactional) if a
multi-tenant or formal-compliance requirement makes the best-effort
window unacceptable for ordinary operations.

## ADR-026 — Server-Owned Transaction Panel State

**Context.** Sift already owns transaction handles and exposes begin, commit,
rollback, and savepoint mutations, but clients cannot enumerate open
transactions or inspect savepoint state. A panel cannot reconstruct that state
reliably from requests because clients reconnect and multiple clients may act
on one session.

**Decision.** The session store is authoritative for transaction-panel state.
It exposes session-scoped listing and side-effect-free commit/rollback preview,
and records ordered savepoint lifecycle metadata next to each opaque driver
handle. State changes only after the corresponding bounded driver call
succeeds. Rollback-to invalidates later savepoints; Postgres release marks its
target released; SQL Server release remains unsupported. Preview describes
known server consequences and never guesses row counts, locks, or database
business effects.

**Consequences.** Any client can render current transaction state after a
reconnect without replaying local history. The state remains process-local,
matching the lifetime of driver handles; a server restart rolls database
connections back and therefore has no transaction state to restore. The
driver trait stays locked.
