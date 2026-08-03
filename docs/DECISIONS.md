# sift — Architectural Decisions

This file keeps only current, load-bearing decisions. Reference material
(feature checklist, Zed-lessons rationale) lives under `docs/legacy/`; the
code-grounded ordered backlog is `docs/PLANS/server-build-list-v2.md`.
Written and candidate ADRs are indexed there against their phase.

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

**Context.** `PROTOCOL_VERSION` began as a bare string (`"1"`) emitted in the
`x-sift-protocol-version` response header but never read from requests. A
client built against a future, incompatible wire contract would hit confusing
partial failures instead of a clear signal, and there was no way to reject an
unsupported client.

**Decision.** The public protocol starts at version `1`. Earlier protocol
numbers existed only during pre-user development and carried no compatibility
promise, so Phase I replaces those draft shapes instead of shipping parallel
codecs for consumers that do not exist. After protocol v1 is published, the
version is a monotonically increasing integer, not semver. It bumps only on a
*breaking* wire change; additive changes do not.

- Breaking (bump): removing or renaming a field or endpoint, changing a field's
  type or meaning, changing an existing enum variant's shape, or tightening
  validation so previously valid requests fail.
- Additive (no bump): new endpoints, new optional request fields with defaults,
  new response fields, new enum variants existing clients can ignore.

Before its first product request, a client sends its inclusive supported range
to `POST /v1/handshake`. The server selects the highest mutually supported
integer and returns its own range, the selected version, release version,
opaque instance/generation identity, and a bounded capability set. No overlap
returns HTTP 426 with `unsupported_protocol_version`. The initial published
range is `[1, 1]`; release semver and application protocol compatibility are
independent.

After selection, every HTTP request and WebSocket upgrade carries the exact
`x-sift-protocol-version`. Every response, including an upgrade and an error,
echoes it; the reference SDK rejects a missing or different value before
decoding the body. A connection never changes protocol in place. Once Phase H
ships, only handshake and health/readiness probes accept an absent version
header. Supporting N and N-1 means advertising that explicit range, not
silently treating an unpinned client as compatible.

**Consequences.** Incompatible clients fail before authentication or product
state is touched, while compatible releases need not have identical semver.
Additive evolution stays cheap and most changes do not bump the protocol.
Clients and servers must retain range-aware codecs for every published version
they advertise; unreleased development shapes do not create permanent
compatibility debt. The SDK gains a small shared handshake state before normal
requests. Detailed wire and reconnect behavior is in
`docs/PLANS/phase-h-remote-development.md`.

---

## ADR-022 — Provider Extensibility Uses A Versioned Out-Of-Process Driver RPC

**Context.** PostgreSQL and SQL Server currently implement the locked ADR-017
`Driver` trait, while public dispatch depends on a closed `Engine` enum,
engine-specific connection unions, and `as_pg`/`as_mssql` downcasts. Adding an
enum variant and downcast for every provider would make core releases the only
way to extend Sift. Treating arbitrary providers as PostgreSQL or SQL Server
would be worse: it would make clients infer unsupported capabilities and hide
provider-specific correctness gaps. ODBC and JDBC offer breadth but also add
driver-manager/JVM packaging, discovery, blocking, type, schema, cancellation,
and fidelity problems that are unrelated to proving Sift's extension boundary.

**Decision.** Phase I introduces immutable validated `ProviderId` and
`DialectId` values, versioned provider descriptors, JSON Schemas for provider
configuration and credential fields, and explicit capability families. Built-in
PostgreSQL and SQL Server map to `sift/postgres` + `sift/postgresql` and
`sift/sql-server` + `sift/tsql`. Public protocol v1 uses provider identity as
the external dispatch key. The pre-release `Engine` dispatch shape is replaced,
not retained as a compatibility codec. Missing capabilities fail explicitly
and are never inferred from a provider name.

First-party PostgreSQL and SQL Server remain native in-process drivers during
Phase I but register through the provider-neutral registry. Third-party drivers
run as supervised local child processes. Driver RPC v1 uses bounded
length-prefixed UTF-8 JSON over dedicated stdin/stdout, a mandatory compatible
range and manifest-identity handshake, generation-scoped opaque handles,
structured errors, absolute deadlines, and host-granted byte credit for result
pages. Stdio is the only v1 transport; plugins cannot open a control listener
or choose an alternate socket. JSON is the only v1 encoding.

Opaque 128-bit handles and request/stream ids are fixed-width lowercase hex
strings rather than JSON numbers. The host alone enforces deadlines. Result
data credit is charged by exact encoded frame bytes and is independent from a
small bounded control-frame allowance; initial credit always permits one
maximum legal result frame.

The logical RPC requires the ADR-017 core verbs: open, ping, schema, begin,
commit, rollback, execute, cancel, and close. Optional behavior is callable
only through negotiated, versioned capability families. Core continues to own
public cursors, paging/spill, sessions, transactions, result references,
timeouts, cancellation orchestration, quotas, and audit. A timed-out or
protocol-violating process is canceled then killed; every handle from that
process generation is invalidated. Discovery may retry, but queries, writes,
and transactions are never replayed automatically.

Connection profiles store provider ids, validated non-secret configuration,
and opaque credential handles. Secret values travel only in the admitted RPC
request, never in arguments, environment, metadata, logs, or audit. The first
external provider is a purpose-built conformance fixture. ODBC/JDBC bridges,
DSN/JAR/JVM discovery, and any automatic IDE-fidelity claim are deferred.

**Consequences.** Sift can add providers without weakening the stable public
contract or loading third-party libraries into the server. The provider RPC,
SDK/schema artifacts, fault corpus, and certification matrix become public
compatibility surfaces. JSON framing costs some throughput, accepted for v1 in
exchange for inspectability and broad implementation support; a future encoding
requires an explicitly negotiated RPC version. Built-ins and external
providers share logical semantics without forcing proven in-process drivers
through a new IPC path. Detailed message, credit, cancellation, and migration
rules are in `docs/PLANS/phase-i-extensibility.md`.

---

## ADR-031 — Extensions Are Declarative-First, Capability-Gated, And Core-Governed

**Context.** Drivers are only one extensibility need. Sift also needs connection
tunnels, credential brokers, hooks, formats, SQL tooling, commands, governed
agent tools, and future client contributions. An unrestricted plugin API could
bypass the invariants that make the server the product: authorization, audit,
secret isolation, durable shared state, quotas, and cancellation. Zed
demonstrates useful extension ergonomics through a small versioned manifest,
declarative contributions, a narrow procedural API, explicit capabilities,
development overrides, and reviewed publication. Sift must adapt those ideas
to an operator-owned, long-lived server.

**Decision.** Every package has a strict versioned `sift-extension.toml`, an
immutable `publisher/name` id, content hashes, target artifacts, compatibility
ranges, contributions, requested capabilities, and provenance. Unknown v1
manifest fields fail closed. Packages install content-addressed. Provenance is
reported as bundled, verified, local, or development; a registry index never
substitutes for verification of the exact package. Development overrides are
local/personal by default and require an explicit instance-admin setting on
hosted deployments.

Most contributions are declarative and start no code. Third-party procedural
server extensions are out of process by default and a v1 package has at most
one supervised executable per target. Manifest discovery never requires
starting that executable. Data-bearing executable contributions use separate
tenant-scoped process generations; an extension process never receives multiple
tenants' credentials or row data. The supervisor owns generation identity,
handshake, health, deadlines, bounded logs, restart backoff, quarantine, drain,
atomic activation, and rollback. It starts the exact self-contained artifact
without a shell or inherited environment. Process isolation contains failure
but is not called a sandbox; runtime records distinguish host-enforced,
platform-sandboxed, and process-only isolation.

Package activation uses immutable content-addressed directories plus a
transactional SQLite selection pointer; startup reconciliation handles
abandoned staging directories, unreferenced packages, and missing selected
bytes. Signed package locks cover the manifest and every payload entry, reject
undeclared archive files, and exclude only the lock and signature themselves.

Requested capabilities are inert until granted by an instance administrator.
Effective permission is the intersection of the manifest request, instance
policy, tenant allowlist, and per-operation authorization. Capabilities cover
scoped database/network access, one-call secret delivery, extension storage,
explicit filesystem/process/HTTP access, operation invocation, event
publication, and tool registration. Tenant users may select allowed
contributions but cannot install server code.

Plugins cannot access metadata SQLite, arbitrary secret handles, raw routes,
another plugin's storage, trusted-local identity, or untracked product
resources. User-visible plugin work enters core through a namespaced extension
operation with a manifest-locked read/execute-read/write/destructive/
administrative classification and an audit-safe schema projection. Phase F
authorization, connection policy, rate/quota admission, approval, deadline,
cancellation, and audit run before dispatch. Plugins cannot mint approvals or
weaken their classification.

Core provides quota-bound namespaced opaque storage with revisions and declared
migrations. Disable retains data. Uninstall removes code and grants but retains
orphaned data; destructive purge is a separate audited admin action. Upgrades
start a candidate generation beside the old for stateless checks, then block
new work and drain/cancel old handles before staging storage migration. Package
and versioned-storage pointers switch atomically only after health passes; no
live handle migration is promised.

Process generations, storage values, namespace totals, and migration working
sets have server hard ceilings. Tenant generations start lazily and idle
generations with no live handles may be evicted. Storage upgrade staging is
copy-on-write over immutable content-addressed blobs so a small migration does
not duplicate an entire namespace.

Connection brokers, tunnels, and hooks participate in a deterministic
core-owned pipeline and return validated leases or patches rather than mutating
profiles directly. `sift mcp` exposes explicitly described, currently
authorized tools through normal Sift sessions. Writes and destructive/admin
actions require narrowly bound approval by default. Declarative client panels
may reference registered operations and typed data but cannot ship arbitrary
JavaScript or bypass server dispatch.

Phase I reserves contribution identities for later SQL semantic and workspace
contracts without invoking them before Phase K/L. A marketplace service,
mandatory OS sandbox, Wasmtime tooling host, ODBC/JDBC bridges, and arbitrary
client UI code remain reversible future work.

**Consequences.** Extension ergonomics follow Zed's strongest patterns without
copying a desktop trust model into the server. An installation remains useful
and administrable with no optional plugin or registry connection. Native
plugins remain operator-trusted where the OS cannot enforce their declared
permissions, and Sift reports that honestly. Manifest, grant, operation,
lifecycle, storage, MCP approval, and conformance contracts become
load-bearing public surfaces. The complete scope and graduation matrix are in
`docs/PLANS/phase-i-extensibility.md`.

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

## ADR-027 — Process Control Through Bounded Catalog SQL

**Context.** Database process inspection and termination are engine-native
administrative operations. Adding them to the locked driver trait would make
every driver implement concepts that are already expressible through SQL.

**Decision.** The server owns a normalized process DTO and composes list/kill
through the existing bounded execute path. Postgres uses `pg_stat_activity`
and `pg_terminate_backend`; SQL Server uses the dynamic management views and a
validated numeric `KILL`. Listing is capped at 500 entries and excludes the
catalog query's own backend. Kill accepts only a positive integer and refuses
the current backend. Database permissions and engine errors pass through the
normal driver error mapping.

**Consequences.** The driver lock and isolation boundary remain unchanged,
while clients receive one cross-engine model. The normalized fields are the
intersection of useful engine metadata; engine-only detail stays optional.
Users without catalog or termination privileges see an explicit database
error rather than a partial success.

## ADR-028 — Advisory, Server-Derived Operation Capabilities

**Context.** A command palette needs the operation vocabulary before it has
concrete request payloads, and it needs disabled reasons for the current
session/connection/transaction context. The existing `/v1/operations` endpoint
is a replay log, so changing its response shape would break consumers.

**Decision.** Add a payload-free `OperationKind` mirror and expose all kinds at
`GET /v1/operations/available`. The query contains only resource ids; the
server derives engine and transaction facts from live session state and
returns availability, a reason, destructive classification, and selected
engine. This surface is advisory UI data. Dispatch routes continue to perform
all state and authorization checks themselves.

**Consequences.** Thin clients can render one complete, contextual command
inventory without duplicating engine rules. The audit-log endpoint remains
compatible. `OperationKind` adds a maintenance obligation, enforced by tests:
new user-visible operations must join both the enum and evaluator.

## ADR-029 — Server-Normalized CSV Import With Two Conflict Modes

**Context.** The existing SQL Server bulk route accepts raw CSV for an existing
table; Postgres has a dormant COPY import extension. Neither offers table
creation, inference, a cross-engine response, or a declared conflict policy.

**Decision.** The server parses and validates CSV, infers a conservative common
type lattice from at most 1,000 rows, and optionally creates the target table
with engine-quoted DDL. `abort` dispatches normalized CSV through Postgres COPY
or SQL Server bulk ingest and remains atomic. `skip` dispatches parameterized
row inserts, suppressing only unique-key conflicts; it reports inserted and
skipped rows and may retain earlier successful rows if a later non-conflict
error stops the request. Existing table types override inference. Payloads are
capped at 64 MiB and excluded from audit.

**Consequences.** Both real drivers share one predictable import contract and
the driver trait signatures stay locked (only extension-operation data grows).
Users choose between atomic failure and duplicate-tolerant progress rather
than receiving engine-dependent implicit behavior. High-volume skip-mode
imports trade throughput for portable conflict semantics; abort remains the
fast path.

---

## ADR-030 — Instance-Owned Closed Registration And Provider-Neutral Principals

**Context.** Sift must support a zero-friction personal loopback server, a
future server reached through SSH, and a network-hosted collaborative instance.
Treating these as a single `local | hosted` switch conflates transport with
trust. Hosted login must also identify more than a caller: the current runtime
has principal ownership on sessions but does not consistently enforce it on
every session-derived route. Adding OAuth alone would therefore leave a
network-hosted instance unsafe. Finally, self-hosted instances cannot depend on
a Sift-operated identity broker or a shared GitHub callback registration.

**Decision.** Deployment policy (`personal | team`) is independent of transport
(`loopback | network | ssh-proxy`). Loopback bypass exists only for the
personal-loopback combination; network transports require explicit
authentication. Team mode is closed-registration and fails closed if its
metadata, durable secret backend, external URL, or authentication configuration
is unavailable.

A `principal` is provider-neutral and may own multiple authentication
identities. Phase E implements instance-owned username/password and a
per-instance GitHub OAuth App. Admins create password identities and allowlist
GitHub logins, optionally linking either credential to an existing principal.
GitHub's immutable numeric user id becomes the durable provider subject after
first login. Email never links accounts implicitly. New principals receive a
personal tenant and join team tenants only through explicit invitations. OIDC
is deferred, but identities retain issuer + subject keys so it is additive.

Passwords are salted and hashed with Argon2id on bounded blocking workers; the
verifier is stored behind `SecretStore` and SQLite contains only its opaque
handle. GitHub client credentials are instance secret-bearing configuration,
never metadata. OAuth uses authorization code, state, and S256 PKCE, and the
temporary GitHub token is discarded after profile synchronization.

Interactive login issues short-lived opaque access tokens and rotating opaque
refresh tokens. Durable state retains only token lookup/digest material,
lineage, expiry, and revocation. Refresh replay revokes the family. Native
clients use bearer tokens; same-origin web clients use secure HttpOnly cookies
with CSRF protection. WebSockets authenticate into renewable leases that can
be invalidated when the principal, auth session, or room membership is
revoked. API tokens remain separate automation credentials; existing Ed25519
key/challenge schema is adopted for challenge login and future SSH bootstrap.

Phase E also establishes the minimum authorization floor: one middleware
produces the authoritative auth context, protected routes are fail-closed,
sessions are principal- or room-owned, and every session-derived resource
inherits that ownership. Phase F retains richer tenant and connection policy,
general rate limits, quotas, and accounting. Initial collaboration is direct:
all collaborators connect to the same network-hosted Sift instance. A central
relay or identity broker is not part of Phase E.

**Consequences.** Password and GitHub users are functionally identical after
authentication, and an admin can attach both methods to one stable account.
Self-hosted operators own their GitHub registration and secrets, avoiding a
Sift cloud dependency at the cost of per-instance OAuth setup. Opaque sessions
make revocation and membership changes immediate; a bounded cache prevents a
SQLite lookup from becoming request-path latency. The first hosted release is
single-process per metadata store, with persistence behind an auth-session
boundary for later replacement. Phase E grows to include ownership enforcement
and auth-specific throttling because exposing identity without those controls
would not create a safely hostable server. Detailed contracts and sequencing
are in `docs/PLANS/hosted-identity.md`.

---

## ADR-020 — Authorization Intersects Tenant, Room, And Connection Policy

**Context.** Phase E established authenticated principals and ownership for
session-derived resources, but ownership alone is not sufficient for a hosted
database IDE. A tenant member may be allowed into a room without being allowed
to use every connection, and observing a collaborative query is different from
executing it. The current raw-spec connection route would also bypass any
profile policy if it remained reachable on a shared instance. At the same time,
requiring a login or a saved profile for a personal server bound to loopback
would violate Sift's zero-friction local-first goal.

**Decision.** Authorization and transport remain separate concerns. The trust
boundary is:

| Deployment | Transport | Login | Connection entry |
| --- | --- | --- | --- |
| personal | loopback | optional | raw spec or profile |
| personal | network | required | profile only |
| team | loopback | required | profile only |
| team | network | required | profile only |

The personal-loopback bypass resolves to the bootstrapped local principal and
personal tenant internally; it is not an unaffiliated or unowned runtime path.
General abuse rate limits and tenant quotas are unlimited by default in this
trusted-local mode, while hard safety bounds such as per-result size, cursor
backpressure, driver timeouts, and cancellation remain active. Explicit policy
on a saved local profile is still honored. Future SSH-proxy transport must
establish an authenticated, instance-bound principal context and never widens
loopback trust.

For managed connections, permission is the intersection of the authenticated
principal's tenant role, optional room role, and connection-profile policy.
Every applicable layer must allow an operation and an explicit denial always
wins. Tenant owner/admin roles may administer connection policy but do not
bypass it for database operations. Tenant members may use profiles permitted to
their role; tenant viewers cannot execute. Room owners/editors may edit and
execute when tenant and profile policy also allow it; room viewers may observe
documents, status, and shared result references but cannot execute.

A connection profile carries a minimum tenant role plus `read_only`, optional
`allowed_ops`, `blocked_ops`, and optional `allowed_schemas`. Operation sets use
the public `OperationKind` vocabulary so capability discovery and enforcement
cannot drift. A missing allowlist is unrestricted by that field, an empty
allowlist permits nothing, and a blocklist always takes precedence. A missing
schema allowlist is unrestricted; an empty one permits no schema. Restricted
SQL is classified with the selected engine dialect before driver dispatch;
unknown or ambiguous statements fail closed when read-only or schema policy
requires classification. Database-side least-privilege credentials remain the
final security boundary for dynamic SQL and stored procedures.

One server-internal authorization evaluator is authoritative for HTTP,
WebSocket, session dispatch, capability discovery, shared-room execution, and
future automation surfaces. Managed runtime connections retain principal,
tenant, profile, and policy-revision provenance; transactions, queries, and
cursors inherit it. Removing access, deleting/disabling a profile, revoking
credentials, or explicitly disconnecting invalidates active descendants.
Ordinary policy edits take effect before the next operation while an already
authorized in-flight operation may finish.

General API rate limiting uses hierarchical token buckets. After authentication
and tenant resolution, each admitted action must obtain its configured cost
from both a principal bucket and a tenant bucket for its route class. Routes
without tenant context consume only the principal bucket; public login and
refresh routes retain Phase E's separate abuse limiter. The route classes are
control/metadata, interactive reads, query admission, heavy transfer, and
streamed bytes. HTTP admission failure returns 429 with `Code::RateLimited` and
a ceiling-rounded `Retry-After`; WebSocket operations return the same stable
code in their error envelope. Checking both scopes is one reservation: a denial
does not partially consume the other bucket.

Buckets refill from monotonic time, support a burst capacity, and are created
lazily with bounded idle eviction. Configuration defines refill rate, burst,
and operation cost per route class; a disabled class has no bucket rather than
using magic zero values. Byte limits on an already-started stream apply
backpressure for a bounded interval instead of attempting to change an HTTP
status after headers were sent. Cancellation and shutdown interrupt that wait.
Trusted personal-loopback traffic is exempt by default, with an explicit
configuration switch available for testing or unusually constrained local
hosts. Rate-limited attempts still produce sanitized failed-operation audit
entries through the existing bounded audit path.

Phase F resource enforcement is single-process. Durable policy and optional
per-tenant overrides live in SQLite, while token buckets and live-resource
counters live in memory. Rate admission intersects principal and tenant token
buckets by route class. Tenant accounting covers managed connections,
concurrent queries, cursors, and retained result bytes. A later distributed
coordination backend may replace these in-memory mechanisms without changing
the public policy model.

Tenant limits cover durable connection profiles and the live counts of open
sessions, managed connections, concurrent driver queries, open cursors, and
retained result bytes. Retained bytes mean memory buffers, response bodies
owned by the server, and cursor spill files; they are not cumulative query
output, history storage, or bytes already delivered to a client. Existing
per-session cursor and per-result size caps remain independent safety ceilings,
so admission must satisfy both the tenant limit and the narrower existing cap.

Instance configuration supplies defaults and operator ceilings. Optional
per-tenant overrides are durable metadata, may be changed only by an instance
administrator, and cannot exceed those ceilings; tenant owners/admins may read
their effective limits and usage. `None` means unlimited and zero denies new
admission. Trusted personal-loopback tenants default to unlimited tenant
quotas. Lowering a limit below current usage does not destroy active work: new
admission stops until usage drains, unless an administrator separately invokes
the explicit disconnect/revocation path.

Live accounting uses reservation guards acquired before expensive work and
released on every completion, cancellation, timeout, disconnect, or task
failure. A successful open transfers its reservation to the runtime resource;
queries retain theirs until the driver stream ends, and result bytes remain
charged until the owning response, cursor page, or spill file is dropped.
Durable profile counts are enforced transactionally in metadata. Process
restart intentionally resets only live counters and reconstructs durable
counts from SQLite.

Rate exhaustion uses `Code::RateLimited`. Live tenant-capacity exhaustion uses
`Code::TenantResourceExhausted` with HTTP 429 and `Retry-After` only when the
server can calculate one; durable object-count exhaustion uses the same stable
code with HTTP 409 and no misleading retry time. Phase F exposes an authorized
tenant-usage snapshot and internal metric hooks. Phase J owns the Prometheus
and OpenTelemetry exporters, so tenant/principal identifiers and high-cardinality
labels are not accidentally frozen into the Phase F wire contract.

**Consequences.** Local use remains login-free and supports direct connection
specs without creating a remote policy bypass. Hosted and collaborative paths
gain one explainable deny-wins model, and `ListAvailableOperations` can report
the same decision the dispatcher will enforce. Sessions and connection entries
need richer provenance, and restricted SQL incurs parser/classifier work;
unrestricted profiles avoid that cost. Phase G shared connections must use this
evaluator, Phase H proxy bootstrap must establish its principal context, Phase
I MCP governance must consume rather than duplicate this policy, and Phase J
metrics export must read the Phase F resource counters.

---

## ADR-014 — Loro Is The Single CRDT Backend For Room Documents

**Context.** Room documents held an opaque byte buffer edited through positional
`insert`/`delete`/`replace` operations applied server-side. That model cannot
converge concurrent edits, survive offline divergence, or preserve intent, and
it carried a speculative `CrdtKind::{Loro, Automerge}` selector with no real CRDT
behind either label. Collaboration depth (Phase G) needs genuine convergence,
reconnect/offline merge, exact-version execution, and stable presence anchors.

**Decision.** Every client and the server hold a [Loro](https://loro.dev) replica
of each document. The only root container is a single `LoroText` named `"text"`;
rich-text marks and extra containers are rejected. Clients author native Loro
updates and the server never generates positional edits on their behalf — it
validates, durably sequences, and rebroadcasts. Loro is the *only* CRDT backend:
the `CrdtKind`/`Automerge` selector and the positional
`TextDocumentOperation`/`DocumentOperationEnvelope` contract are removed rather
than kept as a legacy mode. CRDT bytes (snapshots, updates, version vectors,
frontiers, cursors) cross the wire as standard padded RFC 4648 base64 inside
JSON, each behind its own typed newtype in `sift-protocol`. All Loro CPU work
runs off the Tokio request workers through a per-document blocking actor.

**Consequences.** `sift-doc` depends on `loro`, which lifts the crate's effective
Rust floor above the nominal MSRV 1.80; this is accepted because Loro is the
product's collaboration substrate. Audit attribution is always the authenticated
submitter, never client-controlled CRDT metadata. Full Loro history is retained
inside each snapshot (bounded by a hard per-document history cap) so arbitrarily
old replicas still synchronize. Phase G initially retained protocol version
`"1"`; the typed-null and invalidated-connection contract introduced during
the Phase I readiness pass advances it to `"2"`. Automerge, shared rich-text
marks, and CRDT state outside the SQL text are explicitly out of scope.

---

## ADR-015 — Signed Background Updates Activate On Restart

**Context.** Local, daemon, and SSH-remote installations need low-friction
updates, but replacing a running database server can interrupt queries and
HTTPS alone does not establish that an artifact is an authorized Sift release.
Containers have a different ownership boundary: their orchestrator, not a
process inside the image, owns replacement and rollback.

**Decision.** Sift checks a configured release channel in the background,
downloads without interrupting work, and activates a verified candidate only
on a later launch or explicit daemon restart. An Ed25519 signature over the
exact raw manifest bytes is verified against public release keys embedded in
the trusted binary before JSON parsing. The signed manifest binds channel,
monotonic sequence, expiry, release/protocol versions, target, artifact URL,
byte length, and SHA-256 digest. The updater rejects stale, expired,
downgraded, wrong-target, oversized, malformed, unsafe-archive, or
digest-mismatched input.

Artifacts install into immutable versioned directories. An atomic pointer
selects the next launch; the running executable is never overwritten and the
previous known-good version is retained. Candidate activation requires process
readiness and a compatible ADR-016 handshake, otherwise the pointer rolls back.
`in-process` lifecycle is parent-owned, daemon mode may check and stage but
does not restart active work automatically, and container mode disables
self-update entirely. Remote bootstrap consumes the same signed manifest and
content-addressed artifact cache rather than creating a second trust path.

**Consequences.** Update checks are unobtrusive and a compromised artifact host
cannot authorize a binary without a release signature. Sequence/expiry state,
safe extraction, signing-key rotation, activation health, and rollback become
release-critical code. Binary rollback cannot undo an irreversible metadata
migration, so such a migration requires its own future release gate. The full
manifest and mode contract is in
`docs/PLANS/phase-h-remote-development.md`.

---

## ADR-038 — Metadata Migrations Have One Explicit Lifecycle Owner

**Context.** Every process that opened metadata previously ran Refinery and a
Loro data rewrite. Server startup, administration helpers, and remote-agent
commands could therefore race as schema writers. Candidate activation also
migrated before readiness, although rolling the binary back cannot reverse an
incompatible schema change.

**Decision.** Opening metadata never migrates it. Normal consumers verify that
the embedded migration history is current. `sift-server migrate status|apply`
owns inspection and mutation; apply takes an online SQLite backup before a
non-empty database changes, applies SQL, then runs application-data upgrades.
Every migration is classified, and personal in-process launch may automatically
apply only changes compatible with the previous binary. Daemon, team, remote,
and container deployments require an explicit stopped-server migration step.

**Consequences.** Helpers cannot incidentally migrate, startup failures give a
single recovery command, and launcher rollback remains meaningful across
automatic updates. Operators own maintenance timing outside personal mode.
Contract migrations need an explicit release gate and cannot use automatic
activation. The complete policy, classifications, and restore boundary are in
`docs/PLANS/metadata-migration-lifecycle.md`.

---

## ADR-039 — Backups Restore Sift State Into Destination-Owned Identity

**Context.** Metadata and credential bytes live in separate durability domains,
runtime identity is installation-local, and copied authentication sessions
must not remain valid on a cloned host. SQLite-only copies are therefore not a
complete or safe product backup.

**Decision.** V1 creates an authenticated encrypted archive of a stopped
installation's metadata and exportable secret state under an exclusive
maintenance lock. File secrets are portable inside the encrypted archive and
are re-encrypted with the destination key; keychain secrets remain external
dependencies. Restore is a validated dry run by default, takes an encrypted
rescue backup before apply, preserves destination identity, revokes bearer and
one-use authentication state, rotates system authentication keys, and uses a
durable replacement journal. Connected-database backup is a separate product
surface.

**Consequences.** Backup and restore have a coherent cross-file recovery point
and cannot race a serving process. Archives require a separately protected key
file, restores may require destination keychain preparation, and operators own
retention/storage in v1. Details and the failure matrix are in
`docs/PLANS/state-backup-restore.md`.

---

## ADR-021 — Direct SSH Bootstrap, Persistent Remote Daemon

**Context.** Sift's server-first shape permits a thin client to render locally
while product state and database access remain on a remote machine. Making a
personal daemon publicly reachable would weaken the local trust model, while a
hosted relay or Sift identity broker would add an unrelated service and trust
boundary. Treating remote loopback traffic as implicitly trusted would also
violate ADR-020 and ADR-030.

**Decision.** Initial remote development is a direct OpenSSH topology. A local
helper uses the user's system SSH configuration and host-key policy to probe or
upload a verified server binary, start or reuse a detached remote daemon, and
relay an ephemeral local loopback listener to the daemon's ephemeral remote
loopback port. OpenSSH control-master multiplexing is an optimization with a
dedicated-connection fallback. The daemon runs independently as
`mode=daemon`, `transport=ssh-proxy`; that transport always requires explicit
authentication and rejects loopback bypass.

SSH protects bootstrap but does not by itself name an arbitrary Sift
principal. A personal instance may map its privately owned OS state to the
bootstrapped local principal; team instances require proof with a registered
Sift Ed25519 principal key. Bootstrap returns a short-lived, one-use
`SshProxyCapabilityClaims` envelope bound to the exact instance audience and
principal; its one-use server record additionally binds the daemon generation.
Exchange through the tunnel atomically consumes it for a short-lived access
grant without a portable refresh token. Renewal repeats authenticated
bootstrap. No capability, password, or private key enters arguments,
environment variables, metadata secret bytes, or logs.

An SSH drop closes transport channels but does not stop the daemon. Durable
rooms, documents, profiles, history, and audit survive; process-local sessions,
transactions, cursors, presence, and database connections may need reopening
after a daemon generation change. Loro documents resynchronize normally.
Interrupted requests with an unproven outcome are never automatically replayed.
A hosted collaboration relay and central identity broker remain separate
future designs.

**Consequences.** Remote use gains local rendering and remote database locality
without opening an inbound Sift port or weakening authentication. The client
must own bootstrap, forwarding, capability renewal, reconnect classification,
and verified binary selection. Remote correctness depends on the ADR-016
handshake rather than equal executable versions. Detailed state machines,
failure gates, and implementation order are in
`docs/PLANS/phase-h-remote-development.md`.
