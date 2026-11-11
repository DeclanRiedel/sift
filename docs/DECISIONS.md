# sift — Architectural Decisions

This file keeps only current, load-bearing decisions. Product gaps live in
`docs/PLANS/ide-parity-and-provider-extensibility.md`; focused implementation
and validation plans remain under `docs/PLANS/` only while they carry active
work or normative contract detail.

> **Status convention:** this is architecture documentation, not a release
> roadmap. An ADR marked `graduated` has satisfied its scoped engineering
> evidence; it does not imply beta, release-candidate, distribution, platform,
> or product readiness. `v1` identifies a protocol or contract version unless
> a passage explicitly says otherwise. No product release is declared here.

Format is ADR-lite: **Context · Decision · Consequences.**

---

## ADR-001 — The Server Is The Product

**Context.** Database IDE behavior spans connections, credentials, sessions,
schema, execution, history, audit, and collaboration. Putting that logic in a
window process would make hosted and multi-client modes bolt-ons.

**Decision.** `sift-server` owns product behavior. Clients are renderers and
automation consumers over the public HTTP/WebSocket protocol. Development
clients remain separate from the product UI.

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

**Context.** The server, SDK, and clients all need the same
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

**Context.** Development clients can test routes and workflows, but they are
not production clients. A product UI should not drive backend architecture before
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
promise, so the extension contract replaces those draft shapes instead of
shipping parallel codecs for consumers that do not exist. After protocol v1 is
published, the version is a monotonically increasing integer, not semver. It
bumps only on a *breaking* wire change; additive changes do not.

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
decoding the body. A connection never changes protocol in place. Once remote
hosting ships, only handshake and health/readiness probes accept an absent
version header. Supporting N and N-1 means advertising that explicit range, not
silently treating an unpinned client as compatible.

**Consequences.** Incompatible clients fail before authentication or product
state is touched, while compatible releases need not have identical semver.
Additive evolution stays cheap and most changes do not bump the protocol.
Clients and servers must retain range-aware codecs for every published version
they advertise; unreleased development shapes do not create permanent
compatibility debt. The SDK gains a small shared handshake state before normal
requests. Detailed wire and reconnect behavior is in
ADR-021.

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

**Decision.** The extension model introduces immutable validated `ProviderId`
and `DialectId` values, versioned provider descriptors, JSON Schemas for
provider configuration and credential fields, and explicit capability families.
Built-in PostgreSQL and SQL Server map to `sift/postgres` + `sift/postgresql`
and `sift/sql-server` + `sift/tsql`. Public protocol v1 uses provider identity
as the external dispatch key. The pre-release `Engine` dispatch shape is
replaced, not retained as a compatibility codec. Missing capabilities fail
explicitly and are never inferred from a provider name.

First-party PostgreSQL and SQL Server remain native in-process drivers and
register through the provider-neutral registry. Third-party drivers run as
supervised local child processes. Driver RPC v1 uses bounded length-prefixed
UTF-8 JSON over dedicated stdin/stdout, a mandatory compatible range and
manifest-identity handshake, generation-scoped opaque handles, structured
errors, absolute deadlines, and host-granted byte credit for result pages. Stdio
is the only v1 transport; plugins cannot open a control listener or choose an
alternate socket. JSON is the only v1 encoding.

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
rules are recorded by ADR-022.

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
administrative classification and an audit-safe schema projection. Central
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

The first-party desktop renders extension commands, flat scalar forms, and
read-only panels bound to no-argument, instance-scoped read operations in a
fixed host-owned Extensions dialog. Action results use bounded read-only table
or detail descriptors derived from declared output schemas. The renderer has
no dynamic GPUI entities, scripts, HTML, layout slots, or persisted extension
result data. All action calls use the audited extension operation endpoint;
approval-required calls remain bound to the original request. Unsupported
schemas stay unavailable in the desktop instead of falling back to arbitrary
rendering.

The extension model reserves contribution identities for later SQL semantic and
workspace contracts until those services are implemented. A marketplace service,
mandatory OS sandbox, Wasmtime tooling host, ODBC/JDBC bridges, and arbitrary
client UI code remain reversible future work.

**Consequences.** Extension ergonomics follow Zed's strongest patterns without
copying a desktop trust model into the server. An installation remains useful
and administrable with no optional plugin or registry connection. Native
plugins remain operator-trusted where the OS cannot enforce their declared
permissions, and Sift reports that honestly. Manifest, grant, operation,
lifecycle, storage, MCP approval, and conformance contracts become
load-bearing public surfaces. The complete scope and graduation matrix are in
ADR-022 and ADR-031.

---

## ADR-017 — Driver Trait Lock After Two Real Implementations

**Context.** The server now has real PostgreSQL and SQL Server drivers behind
the same `Driver` trait. The driver contract work aimed to prove the trait shape
before the public protocol is treated as stable enough for GUI and third-party
clients. The remaining driver contract ambiguity was not about more verbs; it
was about which engine-specific capabilities belong in extension traits, how
portable values are represented, and which backend limitations are explicit
unsupported states.

**Decision.** The driver contract is locked around the core eight verbs: `open`,
`ping`, `schema`, `begin`, `commit`, `rollback`, `execute`, `cancel`, and
`close`. The trait remains object-safe: `&self` receivers, boxed async futures
via `async_trait`, concrete protocol-crate request/response types, and handle
structs rather than associated connection types. Engine-only features stay in
extension traits selected through `as_pg()` and `as_mssql()`; wrong-engine calls
produce `UnsupportedForEngine` at the server boundary.

`ConnHandle` remains an opaque id plus engine tag and does not carry a `Weak<dyn
Driver>` back-reference. The server's connection registry is the ownership
boundary for routing cancel/close/transaction work. A future backref would be a
new design item, not part of the driver contract lock.

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
kinds, CSV bulk import, `USE`, and savepoint/rollback-to-savepoint. Runtime MARS
toggling is not in `MssqlExt`; MARS is a connection-time setting and is
currently rejected because the driver/session model allows one active stream per
connection. SQL Server native bulk-load is not represented by the driver
contract `BulkOp`, which carries CSV bytes; native TDS bulk needs typed rows and
column metadata and must use a future request shape if it graduates.

PostgreSQL cancellation uses the backend cancel token. SQL Server cancellation
is implemented as task abort plus connection discard because tiberius does not
expose a public TDS attention API that can be safely sent from a different
task while the query owns the socket. The server removes the SQL Server
connection after cancel so the orphaned backend session cannot be reused.

Driver pooling is not part of the trait signature. PostgreSQL may satisfy
`open()` from a cached pool; SQL Server currently dials one backend session per
handle. Pool warmth and preconnect policy are performance work and do not change
the locked trait shape.

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
Server `cursors: DashMap` of `JoinHandle`). There is no server-side registry, no
per-session cap, no eviction, and no coordination point between the WS ack loop
and future work like predictive prefetch or large-result spill. The performance
follow-ups (bounded memory for a 1M-row result, page-N+1 prefetch, spill to
disk) all need a shared place to stand.

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

**Consequences.** Bounded memory per session becomes a real invariant, not a
hope; a client that leaks cursors caps itself at 32. Spill gives an
evicted-but-still-live cursor a resume path so an idle browser tab does not lose
its results. Backpressure gets a first-class knob that the WS ack loop already
knows about; the mpsc bound remains as a defense against a misbehaving pump.
Drivers stay simple and the ADR-013 driver isolation boundary is undisturbed.
The remaining gap — a global cap for the hosted tenant story — is documented and
left for a hosted-topology ADR.

**Scope note — adaptive prefetch depth.** The pump ships with fixed-depth
prefetch (`prefetch_pages`, default 2), which delivers the "page N+1 buffered
when the client asks for it" behavior the performance plan originally sketched.
**Scaling that depth adaptively based on measured ack velocity is explicitly out
of scope** for this ADR. A future ADR will introduce it if telemetry shows the
fixed depth is a real bottleneck; until then, operators tune `prefetch_pages`
via server config. This is a deliberate choice not to build a self-tuning knob
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

## ADR-lite — Server-side composition for headless features

**Context.** Headless tooling adds three features (DDL generation, autocomplete,
and — later — inline-edit DML) that could each be expressed either as a new
`Driver` trait method or composed on the server over the existing eight verbs.
ADR-017 locked the trait around those eight; every trait addition breaks the
lock and forces a protocol bump.

**Decision.** Compose them server-side. DDL generation
(`crates/server/src/ddl.rs`) established the pattern: fetch what's
needed via `Driver::schema` + `Driver::execute`, format the result in
server code. Autocomplete follows the same rule with one wrinkle —
the SQL context parser is non-trivial and belongs in its own pure-Rust
workspace crate (`sift-completion`) so a future desktop client can
share it without pulling in the server. `sift-completion` depends only
on `sift-protocol` (for wire types + `SchemaSnapshot`) and
`sqlparser-rs` (for tokenization); no I/O, no tokio.

The engine-specific keyword and builtin-function tables originally called out
for `sift-protocol` in the headless features plan instead live in
`sift-completion::keywords`. Protocol stays pure serde (ADR-004); the tables
aren't wire types, they're data the ranker consumes.

The desktop's Full DDL view waits for this audited server operation; it must
not present a synthesized subset of the catalog as a complete definition.
Explicitly partial column/index previews remain labelled as such. Static,
reviewable editor templates live in `sift-snippets`, not the protocol crate;
preview/designer templates recognize supported providers exactly and return
no draft for unknown providers. They are editable text, not a second execution
or catalog-authority path. Execution and migration validation remain server-owned.

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
identities. Authentication supports instance-owned username/password and a
per-instance GitHub OAuth App. Admins create password identities and allowlist
GitHub logins, optionally linking either credential to an existing principal.
GitHub's immutable numeric user id becomes the durable provider subject after
first login. Email never links accounts implicitly. New principals receive a
personal tenant and join team tenants only through explicit invitations. OIDC is
deferred, but identities retain issuer + subject keys so it is additive.

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
be invalidated when the principal, auth session, or room membership is revoked.
Room sockets also revalidate membership in their containing tenant.
Periodic checks use current metadata, run off async
workers, and remain active while result streams wait on driver output or ACKs.
Stream errors and graceful drain cancel and release their active cursor.
API tokens remain separate automation credentials; existing Ed25519
key/challenge schema is adopted for challenge login and future SSH bootstrap.

Authentication also establishes the minimum authorization floor: one middleware
produces the authoritative auth context, protected routes are fail-closed,
sessions are principal- or room-owned, and every session-derived resource
inherits that ownership. Resource governance covers richer tenant and connection
policy, general rate limits, quotas, and accounting. Initial collaboration is
direct: all collaborators connect to the same network-hosted Sift instance. A
central relay or identity broker is not part of identity and authentication.

**Consequences.** Password and GitHub users are functionally identical after
authentication, and an admin can attach both methods to one stable account.
Self-hosted operators own their GitHub registration and secrets, avoiding a Sift
cloud dependency at the cost of per-instance OAuth setup. Opaque sessions make
revocation and membership changes immediate; a bounded cache prevents a SQLite
lookup from becoming request-path latency. The first hosted release is
single-process per metadata store, with persistence behind an auth-session
boundary for later replacement. The authentication scope includes ownership
enforcement and auth-specific throttling because exposing identity without those
controls would not create a safely hostable server. Detailed contracts and
sequencing are recorded by ADR-030.

---

## ADR-020 — Authorization Intersects Tenant, Room, And Connection Policy

**Context.** Authentication established authenticated principals and ownership
for session-derived resources, but ownership alone is not sufficient for a
hosted database IDE. A tenant member may be allowed into a room without being
allowed to use every connection, and observing a collaborative query is
different from executing it. The current raw-spec connection route would also
bypass any profile policy if it remained reachable on a shared instance. At the
same time, requiring a login or a saved profile for a personal server bound to
loopback would violate Sift's zero-friction local-first goal.

**Decision.** Authorization and transport remain separate concerns. The trust
boundary is:

| Deployment | Transport | Login | Connection entry |
| --- | --- | --- | --- |
| personal | loopback | optional | raw spec or profile |
| personal | network | required | profile only |
| team | loopback | required | profile only |
| team | network | required | profile only |
| personal | ssh-proxy | required | profile only |
| team | ssh-proxy | required | profile only |

The personal-loopback bypass resolves to the bootstrapped local principal and
personal tenant internally; it is not an unaffiliated or unowned runtime path.
General abuse rate limits and tenant quotas are unlimited by default in this
trusted-local mode, while hard safety bounds such as per-result size, cursor
backpressure, driver timeouts, and cancellation remain active. Explicit policy
on a saved local profile is still honored. SSH-proxy transport establishes an
authenticated, instance-bound principal context and never widens loopback trust.

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
and tenant resolution, each admitted action must obtain its configured cost from
both a principal bucket and a tenant bucket for its route class. Routes without
tenant context consume only the principal bucket; public login and refresh
routes retain the authentication-specific abuse limiter. The route classes are
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

Resource enforcement is single-process. Durable policy and optional per-tenant
overrides live in SQLite, while token buckets and live-resource counters live in
memory. Rate admission intersects principal and tenant token buckets by route
class. Tenant accounting covers managed connections, concurrent queries,
cursors, and retained result bytes. A later distributed coordination backend may
replace these in-memory mechanisms without changing the public policy model.

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
code with HTTP 409 and no misleading retry time. Resource governance exposes an
authorized tenant-usage snapshot and internal metric hooks. Observability owns
the Prometheus and OpenTelemetry exporters, so tenant/principal identifiers and
high-cardinality labels are not accidentally frozen into the resource-governance
wire contract.

**Consequences.** Local use remains login-free and supports direct connection
specs without creating a remote policy bypass. Hosted and collaborative paths
gain one explainable deny-wins model, and `ListAvailableOperations` can report
the same decision the dispatcher will enforce. Sessions and connection entries
need richer provenance, and restricted SQL incurs parser/classifier work;
unrestricted profiles avoid that cost. Shared connections must use this
evaluator, remote proxy bootstrap must establish its principal context,
extension MCP governance must consume rather than duplicate this policy, and
observability metrics export must read the resource counters.

---

## ADR-014 — Loro Is The Single CRDT Backend For Room Documents

**Context.** Room documents held an opaque byte buffer edited through positional
`insert`/`delete`/`replace` operations applied server-side. That model cannot
converge concurrent edits, survive offline divergence, or preserve intent, and
it carried a speculative `CrdtKind::{Loro, Automerge}` selector with no real
CRDT behind either label. Collaboration needs genuine convergence,
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

**Consequences.** `sift-doc` depends on `loro`, which lifts the crate's
effective Rust floor above the nominal MSRV 1.80; this is accepted because Loro
is the product's collaboration substrate. Audit attribution is always the
authenticated submitter, never client-controlled CRDT metadata. Full Loro
history is retained inside each snapshot (bounded by a hard per-document history
cap) so arbitrarily old replicas still synchronize. Collaboration initially
retained protocol version `"1"`; the typed-null and invalidated-connection
contract introduced during the extension readiness pass advances it to `"2"`.
Automerge, shared rich-text marks, and CRDT state outside the SQL text are
explicitly out of scope.

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
ADR-021.

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
`docs/PLANS/state-backup-restore.md`. ADR graduation is enforced in CI by
committed SQLite databases at the V18 pre-contract, V19 contract-floor, and V28
current boundaries, plus stable JSON fixtures for backup manifests, restore
reports, and remote lifecycle output. The fixture suite also holds the
forward-additive/forward-contract compatibility distinction and secret
redaction behavior.

---

## ADR-032 — One Server-Owned SQL Semantic Document, Dialect Packs Behind It

**Context.** Completion currently tokenizes SQL and caches prefixes inside its
own crate. The SQL semantic service adds parsing, diagnostics, formatting,
statement selection, usages, refactoring, and quick fixes; implementing each
with private syntax and revision state would produce inconsistent byte ranges
and duplicate CPU work. The extension model reserves dialect-pack identity but
intentionally does not let an extension own routes, policy, caches, or product
lifecycle.

**Decision.** Add a UI-free, server-orchestrated semantic service with opaque
process-local document ids, optimistic server-issued revisions, immutable UTF-8
source per revision, and half-open byte ranges. One error-recovering, lossless
parse artifact per `(document, revision, dialect, pack version)` feeds all
semantic features. The connection's declared `DialectId` selects exactly one
capability-negotiated pack. Core owns document/cache/resource lifecycle,
portable contracts, catalog filtering, validation, cancellation, routes,
OpenAPI/SDK, and redacted Operations; packs own dialect grammar and rules.
Bundled PostgreSQL and T-SQL packs may run on a bounded blocking pool, while
external packs use extension supervision. Semantic features return diagnostics,
selections, candidates, or preconditioned text edits; they never mutate CRDT or
future workspace text directly.

**Consequences.** Completion must migrate from its private scanner to the
shared parsed model, and raw completion SQL must be removed from audit records.
Semantic documents are disposable accelerators recreated after eviction or
restart; durable room/workspace text retains its existing owner. Packs and
results require explicit bounds, deadlines, stale-result rejection, and
tenant/policy/catalog-separated caches. ADR graduation requires the two-engine
recovery, range, feature, isolation, cancellation, redaction, fuzz, and latency
corpus specified in `docs/PLANS/sql-semantic-service.md`. Plan capture may key
to the resulting statement identity but retains a separate execution/retention
lifecycle.

**Graduation.** Implemented and graduated on 2026-08-10. The two-engine
feature, isolation, redaction, public-surface, and latency evidence is recorded
in repository tests.

---

## ADR-033 — One Revisioned Catalog Graph Drives Diff And Safe Migrations

**Context.** The progressive shallow/deep schema tree is sufficient for a
browser and completion, but it has no stable object identity, dependency graph,
coverage contract, durable snapshot, or safe path from a structural difference
to executable DDL. Building diagrams, binding, diff, and migrations as separate
client features would duplicate engine inference and make partial
introspection look authoritative.

**Decision.** Add a server-owned, provider-neutral catalog graph with opaque
object ids, optional non-authoritative engine-native ids, monotonic content
revisions, deterministic digests, typed dependency edges, and explicit
complete/partial/stale coverage. Fetch it through an additive
`SchemaDepth::Graph`/`driver.schema.graph@1` capability so the locked Rust
`Driver` trait signature remains unchanged. Cache canonical graph truth per
database identity and derive policy-filtered projections for consumers;
invalidation advances an epoch shared by schema, search, semantic, and diagram
views. Durable tenant-scoped snapshots are immutable and secret-free.

Diff is a normalized, dependency-ordered comparison of two live or durable
catalog sources. It suppresses definitive destructive conclusions when
coverage is incomplete, treats heuristic renames only as suggestions, and
classifies every change by risk and reversibility. Migration preview generates
engine-aware transactional groups bound to exact catalog/policy revisions and
an opaque plan digest. Apply rechecks every precondition, requires explicit
destructive acknowledgements, executes through bounded isolated driver work,
records partial outcomes honestly, and invalidates catalog state after every
attempt.

**Consequences.** Graph-capable providers need bulk introspection and hostile
result validation, while providers without the capability retain existing
shallow/deep behavior. Native ids can aid live rename correlation but never
authorize access or promise portability. Clients own diagram layout, not graph
truth or DDL generation. The full contract and graduation matrices are in
`docs/PLANS/catalog-graph-schema-migrations.md`; comparison, diagram projection,
and semantic plan retention are locked separately in
ADR-032 and ADR-033.

**Graduation.** Implemented and graduated on 2026-08-10. The live-engine,
migration lifecycle, policy isolation, hostile-provider, public-surface, and
resource-budget evidence is recorded in
repository tests.

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
ADR-021.

---

## ADR-034 — Canonical Virtual Workspaces With Optional Filesystem And VCS Projections

**Context.** Workspace and automation support adds durable SQL files, local
history, offline DDL sources, Git, run configurations, schedules, and transfer
recipes. Making the client filesystem authoritative would break thin clients and
make those features disappear over SSH or on a hosted instance. Making a
checkout authoritative would also race collaborative Loro edits with filesystem
and Git operations. The old V003 principal-owned workspace/session/tab schema
predates ADR-007 and has no valid shared-room semantics.

Zed's Git architecture provides useful implementation patterns: repository
operations sit behind a trait, porcelain output becomes typed state, remote
clients invoke Git where the worktree lives, in-flight path operations are
explicit, and a fixed system-Git wrapper disables unsafe hooks/helpers and
interactive behavior for untrusted repositories. Sift adopts those boundaries
but cannot adopt a desktop editor's filesystem authority or ambient user
credentials.

**Decision.** A workspace is a server-owned, revisioned virtual tree attached
to exactly one room; a room may contain multiple workspaces. Stable node ids,
not mutable paths, are identity. An SQL node points one-to-one at the room's
existing Loro document, so SQL has one durable content owner. Paths, folders,
checkpoints, Git state, runs, logs, results, catalogs, and artifacts are never
CRDTs. L1 supports folders and SQL documents; later recipe output may add
immutable artifact nodes.

The user-facing history is a bounded set of immutable checkpoints over a tree
revision and document content frontiers. Automatic checkpoints occur before
meaningful reconcile, run, VCS, and restore operations; named checkpoints are
explicit. Restore creates a new head and an audited native Loro replacement,
never rewrites prior history.

A filesystem binding is an optional projection beneath an operator-configured
root. Public requests contain only normalized workspace-relative paths and
opaque binding ids. Reconciliation compares the last materialized revision and
per-file digests, returns a deterministic typed plan, and requires an explicit
preconditioned resolution when both sides changed. Symlink/hard-link escapes,
special files, traversal, aliases, and case-fold collisions fail closed. The
V003 principal-owned tables were already retired by the ADR-007 contract
migration in V006, which dropped `tab`, `session_snapshot`, and `workspace`.
L1 therefore creates a new room-owned schema rather than reinterpreting legacy
rows.

Git is a VCS adapter over one projection checkpoint. The bundled adapter uses a
fixed system Git executable, structured arguments, bounded output and process
lifetime, and no shell. Untrusted repositories disable hooks, credential
helpers, external diff, fsmonitor, extension protocols, pagers, and optional
interactive locks. Network credentials are resolved from `SecretStore` into a
one-operation helper channel and never enter URLs, arguments, inherited
environment, SQLite, logs, or audit. Remote clients invoke the adapter beside
the remote server; they do not mount or mirror its checkout. Local
status/diff/stage/commit precede authenticated fetch/push. Force-push, arbitrary
shell commands, and automatic merge/rebase are excluded from v1.

Run configurations capture immutable manifests before execution. Values use
typed database parameters; identifier substitutions are separately validated;
untyped textual substitution is forbidden. Schedules are owned by a normal
principal and re-evaluate current authorization at every occurrence. Revoked
authority disables work. Interrupted writes with uncertain outcomes are marked
`outcome_unknown` and never replayed automatically. Pre-tasks are bounded core
operations or extension tools declared schedulable, never shell strings.

Core owns transfer admission, canonical row streaming, artifacts, limits, and
stage-and-commit. Untrusted formatters run through extension supervision. The
workspace and automation public additions are additive under ADR-016 and retain
protocol v1.

**Consequences.** Virtual workspaces work identically in personal, SSH-remote,
network-hosted, and container deployments even when filesystem/Git capability
is disabled. Checkouts become disposable/rebuildable projections, commits bind
to explicit checkpoints, and collaborative edits arriving after
materialization cannot leak into an in-flight commit. The server must add
room-aware metadata instead of reviving V003, rooted filesystem traversal,
typed VCS/run/recipe contracts, durable recovery state, capability discovery,
and deployment/security graduation matrices. The normative implementation
order is retained in git history.

**Graduation.** Implemented and graduated on 2026-08-28. The deployment,
checkout ownership, backup/restore, degradation, compatibility, trust,
malformed-output, subprocess isolation, secret-flow, concurrency, and crash
recovery matrix is recorded in `docs/GIT-OPERATIONS.md` and enforced by the
Git adapter, workspace confinement, workspace HTTP, metadata, state-backup,
and instance-runtime tests. Operators can inspect the fixed executable,
observed version, helper state, health, and effective policy through the
admin-only typed VCS diagnostics route. Time, output, file, status, history,
commit-file, and diff ceilings are strict portable instance policy.

The 2026-08-28 Linux/Git 2.51 baseline measured steady status at 3.324 ms for
1,000 paths, 8.934 ms for 10,000, and 57.139 ms for 100,000. Identity-aware
render reuse reduced unchanged maximal-status refresh p95 from 173.933 ms to
61.669 ms (64.5%). These are bounded operational baselines, not a claim that a
20,000-row first render meets 120 Hz; commands and reproduction details remain
in `docs/PERFORMANCE.md`.

---

## ADR-041 — Two-File Reproducible Server Instances

**Context.** Server settings, identities, allowlists, tenants, and connection
profiles previously depended on ambient process configuration and mutable
SQLite state. Copying a local Sift installation therefore could not reproduce
the same server safely. Putting passwords in an editable portable manifest
would make that workflow easy to leak, while mixing host bindings into the
portable source would make it platform-specific.

**Decision.** A server root is exactly one operator-edited `sift.toml` and one
generated `sift.lock`. The manifest is strict desired state and contains
credential-free connection strings plus logical credential-slot references.
The lock binds its canonical digest to exact Sift, protocol, provider schema,
extension, and artifact identities. Secret bytes live only in the
destination's `SecretStore`; SQLite stores opaque handles. Apply records
manifest ownership, reconciles in one transaction, requires explicit destroy
approval, honors `prevent_destroy`, and invalidates credentials when their
consumer identity changes. Destination-private immutable generations hold
resolved bindings, and startup accepts only the applied generation matching
both portable files.

The initial personal/loopback bootstrap trusts the local OS account, private
state permissions, and verified loopback peer. Team/network deployments never
receive that bypass. Hosted OAuth resolves its client secret from a typed slot
at runtime. Client presentation preferences and database contents are outside
the manifest.

**Consequences.** The same root is portable across Linux, macOS, and Windows
when its selected providers/artifacts support the target. Reproduction covers
declared Sift behavior, not host provisioning, DNS/TLS, database data, or
secret values. Failed reconciliation never advances the current-generation
pointer, unmanaged rows cannot be adopted or deleted, and stale/edited roots
cannot start. The operator guide is `docs/INSTANCE-CONFIG.md`; the full design
and deferred package/remote automation work are in
`docs/PLANS/reproducible-instances.md`.

---

## ADR-042 — Manifest-Only Server Control Plane

**Context.** Sift exposed both a reproducible desired-state manifest and
interactive administration screens for some of the same resources. That made
ownership ambiguous: an operator could change realized metadata without
changing the file that was meant to reproduce the server. The manifest also
omitted operational settings that the runtime accepted through its legacy
process configuration, so copying an instance did not reproduce all declared
server behavior.

**Decision.** `sift.toml` is the only editable source of truth for server
policy and operator-managed resources. The desktop may provide schema-aware
editing, navigation, validation, planning, credential binding, and apply, but
it does not provide a second mutable representation of principals, tenant
memberships, connection profiles, extension admission, or other manifest-owned
state. Contextual actions on those resources route to the corresponding
manifest section. Runtime APIs remain authoritative for user content and
ephemeral workflow state such as documents, rooms, approvals, query sessions,
and results. Client presentation and keymaps remain local preferences.

Secret bytes, generated state paths, metadata database paths, resolved ports,
and immutable generation records remain destination-private bindings. They are
not portable desired state. The generated `sift.lock` is inspectable but never
edited. Environment variables may bootstrap legacy/non-instance server runs,
but cannot override a locked instance.

**Consequences.** Every durable operator change has one reviewable diff, one
validation model, and one apply path. Administration views distinguish
manifest-owned resources from user/runtime state and offer “Edit in
`sift.toml`” instead of mutating managed rows. The instance editor and Wiki
must be generated from or tested against the same manifest schema so docs,
completion, validation, and runtime realization cannot silently diverge.

---

## ADR-040 — GPUI Desktop Client With A Server-Only Product Boundary

**Context.** ADR-010 deferred the product UI until the headless server,
protocol, room, metadata, semantic, workspace, and automation contracts were
stable enough to consume. Phases A through L now provide that foundation. Sift
needs a first-party desktop client with Zed-class responsiveness without
turning local mode into a separate product architecture or leaking UI types
into shared crates. Zed is the interaction and GPUI architecture reference,
but its application crates encode a local worktree/editor product and are not
an appropriate dependency for a server-authoritative database IDE.

**Decision.** Sift builds a native desktop client directly on an exactly pinned
GPUI revision. Linux is the primary development platform; Linux, macOS, and
Windows are architectural and graduation targets. Sift owns its component
library, themes, icons, workspace entities, panes, items, actions, focus
contexts, state restoration, and virtualized database views. It adopts Zed's
entity ownership, emitted-event, action, pane, restore-before-I/O, and
background-task patterns without importing Zed's `ui`, `editor`, `workspace`, or
`project` crates.

Desktop composition is host-owned and closed to extensions. Stable typed
registries centralize built-in command, dock, and pane-item identity and
metadata, but they are Rust registries rather than runtime plugin registries.
Adding or replacing application chrome, panels, pane renderers, styling, or
layout requires a reviewed first-party client change. Extensions may add server
providers and governed operations through extension contracts; they do not
register GPUI renderers or mutate the desktop entity tree. Existing public
client-contribution descriptors remain wire-compatible for independent thin
clients, but the first-party desktop does not use them to alter its UI.

The desktop remains a thin client. A separately supervised local server uses
the same public HTTP/WebSocket path as SSH and hosted servers; feature views
cannot call server, driver, or metadata internals. One window presents one
virtual workspace with multiple connections and split panes. Query items own
their Data, Messages, Explain, and History results, while pinned results may be
promoted to independent items. Client persistence contains presentation state
only. Results, schema, connections, operations, workspaces, and policy remain
server-authoritative; only query text uses the existing Loro replica.

Presentation persistence is local to the desktop installation's OS account.
It is never uploaded to a Sift server, stored in a room, or synchronized
between collaborators. Users attached to the same room may therefore keep
independent windows, tabs, panes, dock widths, active items, and themes. Server
identity and workspace references may appear in local restoration data, but
they remain references and are re-authorized when restored.

Footer panel selection follows the same rule: the active left panel and bottom
tool are client-local presentation state. The host owns the fixed SQL-oriented
set; extensions cannot insert controls or mutate the desktop entity tree.

**Consequences.** ADR-010's deferral condition is satisfied and product UI work
may begin. GPUI remains isolated to desktop/UI crates and may be upgraded only
deliberately because it is pre-1.0. M0 must first remove the public SDK's
server-internal metadata dependency and prove GPUI text input, accessibility,
focus, async cancellation, virtualization, and testability. Platform-specific
window, input, dialog, credential, and packaging behavior lives behind one
narrow native boundary. The complete milestone order, visual language, ownership
model, performance rules, recovery requirements, and graduation gates are in
`docs/PLANS/gpui-desktop.md`.

Host ownership trades third-party UI customization for predictable upgrades,
accessibility, focus routing, theme coverage, restoration, and failure
isolation. Built-in commands share one definition for menus and the command
palette. Docks and pane items use typed identities rather than display strings;
display labels are never dispatch keys.

---

## ADR-042 — Recursive Client-Local Pane Layout

**Context.** The desktop initially represented panes as one flat horizontal
vector plus one parallel vector of flex values. That model can only append
columns. It cannot express an editor split above or below another pane, mixed
horizontal and vertical groups, local resize ownership, or deterministic
collapse after a pane is removed. Tab dragging then has to mutate pane order and
sizing as unrelated arrays, which makes previews and focus transfer fragile.

Zed's pane group is the interaction reference: a member is either a pane or an
axis containing members; splitting along the current axis inserts a sibling,
splitting across it nests a new axis, and removing the penultimate child
collapses the redundant axis. Sift needs those presentation semantics without
importing Zed application crates or moving any product state into the client.

**Decision.** Desktop presentation stores pane contents separately from one
recursive layout tree. Leaf nodes contain stable pane ids. Split nodes contain
an axis (`horizontal` or `vertical`), at least two child nodes, and one positive
finite flex value per child. Every pane id occurs exactly once. Invalid,
duplicate, missing, non-finite, or degenerate layout data is repaired to one
horizontal group containing the known panes in presentation order.

A directional split targets one leaf. Left/right use a horizontal axis;
up/down use a vertical axis. When the parent axis matches, the new leaf is
inserted adjacent to the target and sibling flexes are normalized. Otherwise
the target leaf is replaced by a two-child nested axis. Removing a leaf
collapses every one-child ancestor. The final leaf is never removed. Resize
handles belong to axis boundaries and change only their adjacent child flexes,
subject to the existing minimum pane extent.

Drag hover uses four directional edge zones plus a center zone. Edge depth is
the smaller pane dimension multiplied by a bounded presentation setting;
`0.20` is the initial value. Corners choose the nearest edge. The preview is a
typed, ephemeral drag overlay covering the exact half that a split would
occupy. Tab-bar and pane-body targets remain separate. Escape cancels the GPUI
drag without mutating layout. Drops defer workspace-tree mutation until event
dispatch completes, then move the complete item runtime bundle atomically and
focus the destination.

`WorkspacePresentation` gains an optional recursive layout. Existing version-1
files without it migrate from `panes` plus `pane_flexes`; no server or protocol
migration is involved. New snapshots write the tree and omit legacy flex data.
This remains OS-account-local presentation state under ADR-040.

**Consequences.** Panes can form arbitrary mixed split layouts and restore
deterministically. Preview geometry, resize ownership, focus navigation, and
pane removal derive from one structure instead of parallel vectors. The client
must add tree validation, migration, recursive rendering, directional tests,
and collapse/resize coverage. Protocol, room, result, schema, session, and CRDT
contracts remain unchanged.

---

## ADR-043 — Lazy Typed Git Hunks And Checkpointed Destructive Actions

**Context.** File statistics are enough for a compact change list but not for
review, hunk staging, or safe reversion. Returning raw unbounded patch text
would move Git parsing into clients, make remote usage expensive, and allow a
large or malformed repository to monopolize memory and rendering. Sift also
cannot copy an IDE's direct `git restore` behavior: the virtual workspace and
room document remain canonical, while the checkout is only a projection.

**Decision.** Project diff requests return bounded typed file statistics.
Textual hunks are loaded lazily with a normalized workspace-relative path and a
typed diff side (`HEAD` to index, index to worktree, or `HEAD` to worktree).
The server parses Git output into typed hunks and lines with old/new line
coordinates, stable content-derived hunk ids, binary markers, and truncation
at output, file, hunk, line, and byte ceilings. Clients never parse or render
ambient patch output as trusted structure. Hunk and line mutations must carry
the repository binding revision, path, diff side, and patch precondition; a
hunk id alone is not authority.

Destructive file or hunk actions are not direct checkout mutations. The server
serializes them with other repository/workspace mutations, creates a workspace
checkpoint, verifies the repository and workspace revisions, applies the
bounded Git operation, then reconciles the affected projection path through
the existing virtual-workspace contract. Untracked filesystem-only content
cannot be advertised as recoverable unless its bytes were first captured by a
bounded checkpoint artifact. Until that capture contract exists, discard of
untracked files stays disabled. Every accepted action is audited and publishes
workspace and repository change events.

**Consequences.** Remote and local clients share one safe diff model, project
views can stay virtualized and lazy, and stale hunk operations fail instead of
editing new content. Binary or oversized files remain reviewable at the file
metadata level. Discard/restore implementation depends on projection
reconciliation and checkpoint capture rather than bypassing the canonical room
document, so those actions may land after the first read-only diff UI.

---

## ADR-044 — Guarded Shared-HEAD Amend And Uncommit

**Context.** Amend and soft reset rewrite the shared repository HEAD. A local
IDE can treat those as private convenience commands, but Sift collaborators
share one server-owned index, branch, and projection. An unguarded rewrite can
silently replace a commit another client has already reviewed or built upon.

**Decision.** Amend and uncommit are typed server operations, never arbitrary
Git commands. Both require the repository binding revision and exact expected
HEAD OID, execute under the workspace/repository mutation lock, capture an
immutable `BeforeVcs` workspace checkpoint first, audit the actor, advance the
binding revision, and broadcast the resulting HEAD. A stale expected HEAD is a
conflict with no Git mutation.

Amend commits only the existing index, uses the same explicit author identity,
sign-off policy, hook prohibition, projection-reconciliation guard, and
checkpoint record as an ordinary commit. Uncommit is limited to a one-parent
soft reset of the current HEAD; it keeps the removed commit's changes staged,
does not cross a root commit, and does not alter collaborative document text.
The UI requires confirmation and shows the recovery checkpoint. Neither
operation performs merge, rebase, hard reset, or force push.

**Consequences.** History convenience remains recoverable and observable, and
two clients cannot intentionally rewrite different notions of HEAD. The
repository commit ledger remains append-only: an amended replacement receives
its own checkpoint mapping while the superseded commit record is retained for
audit. Uncommit recovery uses the captured workspace checkpoint plus ordinary
Git history; later branch/history milestones may expose that relationship more
fully.

---

## ADR-045 — Desktop Workspace Files Follow The Canonical Room Frontier

**Context.** The desktop needs an IDE-like file tree while Sift SQL text is a
collaborative room document and the filesystem is only an optional projection.
Treating editor buffers, watcher events, or Git files as another authority
would permit a save, checkout, or refresh to overwrite newer collaborative
text.

**Decision.** The desktop maintains a typed workspace-files projection separate
from repository status. Nodes retain server `WorkspaceNodeId` identity across
moves, and SQL tabs open the node's canonical room document. Editor updates are
submitted immediately; Save and Save All insert an ordered flush marker and
mark a tab clean only when the room supervisor confirms that every preceding
CRDT update was submitted.

Filesystem state is read through the existing bounded reconciliation plan.
The UI distinguishes editor, virtual-tree, filesystem-projection, and Git dirty
states. Both-changed entries have no default resolution. Applying a complete
revision-guarded plan uses the existing server checkpoint and reconciliation
transaction; watcher/fallback refreshes only plan changes and never mutate room
text. Git refresh caused by document editing occurs after the projection
revision commits, while a bounded fallback still discovers external checkout
or Git changes.

**Consequences.** Workspace CRUD, checkpoint history, restore-as-new-head, and
reconciliation are available to local and remote desktops without direct
filesystem access. A save acknowledgement is meaningful rather than cosmetic,
stale plans fail safely, and external checkout changes become visible choices
instead of implicit document replacements.

---

## ADR-046 — Revision-Guarded Shared Branches And Bounded History

**Context.** A repository binding has one shared worktree, index, and HEAD for
every collaborator in its room. Branch changes therefore are not client-local
navigation. History can also be arbitrarily large, contain unusual paths, and
produce output well beyond an interactive frame budget.

**Decision.** Branch and HEAD mutations execute only on the server through
typed, audited operations carrying the observed repository-binding revision.
Switching requires a clean Git worktree; reconciliation remains explicit
because the canonical room workspace must never be overwritten by checkout as
a watcher side effect. Local branch deletion uses Git's merged guard by
default, and an unmerged deletion requires a separate explicit confirmation.
Every successful mutation updates the observed binding and broadcasts its new
revision to the room.

History is read through bounded typed pages. Commit identifiers accepted for
historical content, comparison, restore, detached checkout, and revert are
full object ids rather than arbitrary revision expressions. Commit files and
historical text have server byte/count ceilings. Desktop history rows are
virtualized, historical files are read-only, and restoring/reverting first
captures a workspace checkpoint before reconciling changed projection paths
back into canonical room documents.

**Consequences.** Collaborators see one authoritative branch/HEAD transition,
stale clients fail rather than racing it, and history rendering cost is tied to
the visible page. Sift deliberately does not automate merge or rebase here;
dirty switching must first go through the visible workspace reconciliation
workflow.

---

## ADR-047 — Index-Stage Conflict Resolution And Recoverable Git Operations

**Context.** Merge, rebase, cherry-pick, and revert state lives in the shared
repository, not in one desktop process. Conflict markers are ordinary file
text and can be missing, edited, or deliberately marker-shaped, so parsing the
worktree cannot establish the base/ours/theirs versions. A disconnected client
must not own an operation that Git itself still considers active.

**Decision.** Repository status includes a typed active-operation state derived
from Git's operation metadata. Rebase state also reports its current commit and
step when Git records them. Conflicted files are identified from porcelain
status and their base, ours, and theirs content is read from index stages 1, 2,
and 3. Conflict-region ids are content-derived preconditions; the server
reloads the stages and rejects a stale id before mutation. Marker-shaped file
text is never parsed as conflict authority.

Use-ours, use-theirs, and use-both are typed, revision-guarded operations that
capture a workspace checkpoint, write or remove the resolved worktree path,
stage it, reconcile the canonical workspace projection, audit the actor, and
broadcast the new binding revision. Binary conflicts permit only one side.
Manual resolution first captures the same checkpoint; the desktop permits
mark-resolved only after the workspace editor has been opened and is saved.
Continue is disabled while index conflicts remain. Merge, cherry-pick, and
revert continuation and every supported abort are explicit operations. Rebase
continuation remains disabled until Sift has a non-interactive sequence-editor
contract; the UI explains that limitation and still offers abort.

Operation recovery has no client lease: every status request reconstructs the
state from Git, so restart or disconnect does not strand it. Unsupported and
corrupt states return stable actionable errors, and the desktop suspends its
fallback status requests while such an error is displayed. Repair re-observes
the configured projection and atomically refreshes repository identity and Git
adapter metadata under the existing binding id and revision. If an operator
moves a configured projection, they update the root mapping first and then run
repair; changing the logical root handle remains the explicit rebind workflow.
If a bound projection has lost its Git metadata, an explicit desktop action
initializes Git at the same configured root and repairs the existing binding
under its revision guard. Ordinary status reads never initialize a repository.
This creates fresh history; operators restore `.git` from backup when the old
history matters.

**Consequences.** Conflict UI does not confuse user SQL with Git structure,
stale clients cannot resolve a newer conflict, and repository operations remain
recoverable without an initiating desktop. Resolution is deliberately
whole-file while the adapter exposes one stage-derived region; finer semantic
regions may be added later without changing the trust boundary. Shared
operation serialization and actor/in-flight collaboration presentation remain
Milestone G8 work.

---

## ADR-048 — Explicit HTTPS Remotes And Principal-Scoped Credentials

**Context.** Git executes beside Sift's server-owned projection, so a desktop
credential helper, SSH agent, filesystem key, or URL-embedded password would
lend ambient server authority to an attached client. A credential shared by a
room would also make a remote action appear under the wrong hosting identity.
Network operations can be slow and mutate shared remote-tracking refs, so they
must not hide behind status refresh.

**Decision.** Repository setup accepts only an operator-configured logical root
handle. A workspace owner may bind an existing repository, initialize that
projection, or clone an HTTPS URL into an empty configured projection. Remotes
are listed and mutated through typed, validated APIs. Embedded URL credentials,
non-HTTPS transport, force push, arbitrary refspecs, hooks, interactive prompts,
global configuration, credential helpers, and inherited environment remain
disabled.

PAT/basic credentials are stored by `SecretStore`; SQLite contains a distinct
opaque handle for each `(repository binding, principal)`. A secret is exposed
only to one explicit fetch, push, clone, or credential-test process through the
bounded askpass channel and is never placed in a URL, argument, audit record, or
log. Public HTTPS fetch/push may run without a stored credential. Managed SSH
support is deferred until Sift owns a typed key record, host-key policy, and
one-process `GIT_SSH_COMMAND` wrapper that never consults `SSH_AUTH_SOCK`, a
home-directory key, or user SSH configuration.

Fetch and push are manual, visible, revision-guarded operations. Their result
reports typed before/after ref changes; common authentication,
non-fast-forward, protected-branch, missing-remote, and network failures are
classified without returning transport stderr. First push of a branch without
an upstream uses `--set-upstream`; later pushes preserve it. Pull remains
unavailable: users fetch first, inspect branch state, and use the existing
explicit branch/conflict workflow. A future pull must preview merge versus
rebase effects and require a selected strategy before it can mutate the shared
worktree. Sift permalinks identify repository, commit, and historical file
without assuming a hosting provider.

**Consequences.** Collaborators cannot silently borrow one another's remote
identity, remote traffic never occurs as a background refresh, and UI output is
actionable without leaking provider text or secrets. SSH and pull have explicit
security and review prerequisites instead of incomplete fallbacks. A remote
rename followed by URL replacement is two individually revisioned and audited
mutations, so partial completion remains visible and recoverable by refresh.

---

## ADR-049 — Server-Owned Shared Repository Mutation Lease

**Context.** A room workspace projects one canonical document tree into one Git
worktree. Its index, checked-out branch, HEAD, remotes, and active Git operation
are therefore shared even though credentials remain principal-scoped. Revision
guards reject stale clients, but guards alone do not prevent two requests from
passing observation concurrently and then racing filesystem or index changes.
A desktop-owned lease would also disappear on disconnect and could strand the
other collaborators behind state they cannot inspect.

**Decision.** The server serializes every repository mutation with the existing
per-workspace lock and reloads the binding after acquiring it before checking
the expected binding revision. The handler owns the lock through Git mutation,
metadata observation, audit, and room publication. A typed room event announces
the authenticated actor, action, and started/succeeded/failed phase. Attached
desktops show that operation, disable competing mutation controls, and refresh
workspace files and repository status from the authoritative server result at
the terminal phase. Repository-change events cover binding revision, branch,
HEAD, remote, and principal-visible credential-presence changes; explicit Git
actions remain typed `Operation::Vcs` audit records.

Commit preparation creates a canonical workspace checkpoint while holding this
same lock, then stages the checkpoint projection. Document edits accepted after
that frontier remain visible in the room and reconcile as later uncommitted
work; they cannot leak into the in-flight commit. The server guard is released
when a handler finishes or is cancelled, so client disconnect needs no
ownership transfer. After process failure, no lease survives; status reconstructs
Git-native merge/rebase/cherry-pick/revert state as defined by ADR-047, while
ordinary interrupted commands are recovered through a fresh authoritative
status observation.

Viewers may inspect status, diffs, branches, history, conflicts, and remotes.
Editors and owners may mutate the shared repository; binding remains an
owner-only workspace-projection action. Per-user branches or worktrees are
deferred: adding them now would split the canonical projection and require an
explicit publication/reconciliation model that the product does not yet need.

**Consequences.** Repository mutations have one server-defined order, stale
desktop intentions fail safely, and collaborators can see who currently owns
the operation without granting that client durable authority. Optimistic local
state is presentation only and always yields to terminal status. Separate
per-user credentials do not imply separate branches or indexes.

---

## ADR-050 — Database Change Ledger And Versioned Execution Provenance

**Context.** Git can prove which reviewed SQL/DDL artifact a person authored,
but it cannot prove who executed a change against a database, whether that
change committed, or what happened through grid editing, CSV import, or direct
DML. Putting live rows or credentials into Git would turn source control into a
data-exfiltration path. Conversely, treating a Git author as the database actor
would collapse authored, approved, and executed identities into a false claim.

**Decision.** Sift maintains an append-only, hash-chained Database Change
Ledger in metadata. It records authenticated executor, optional author and
approver, tenant/room/profile/database scope, affected object, bounded row
count, SQL and row-identity fingerprints, transaction/correlation identifiers,
terminal outcome, and validated workspace revision/checkpoint/path/commit
provenance. It never records raw SQL, parameters, cell values, before/after
rows, results, or credentials. Database-native audit/CDC imports retain their
database actor separately and must declare a non-Sift identity source and
confidence; they never impersonate an authenticated Sift principal.

Git remains authoritative only for immutable reviewed artifacts. A Sift commit
links to its workspace checkpoint and tree revision. Query, explain-plan,
migration, and automation execution may link to that artifact after the server
validates workspace membership, revision, path, checkpoint, and commit. The
ledger is authoritative for “who executed what and with what outcome.” SQLite
triggers reject update/delete and every entry hashes the complete redacted
record plus the preceding hash. Retention is tenant-admin controlled. The
optional `pull:csv` external sink is deliberately pull-based: a tenant admin's
collector retrieves the permission- and retention-scoped export endpoint, so
Sift stores no webhook URL, bearer token, or SIEM credential.

Database effects and SQLite metadata cannot share one atomic transaction.
After the database reaches a terminal result, Sift makes a bounded best-effort
ledger append and emits an operator-visible error if metadata is unavailable;
native database auditing is the required independent control where losing that
final append is unacceptable. The hash chain detects metadata tampering but is
not a substitute for an external immutable archive.

Optional before/after capture is a separate future compliance mode, disabled by
default. It requires per-tenant envelope encryption, a distinct access role,
field allowlists, explicit purpose, short retention, deletion/legal-hold rules,
key rotation and revocation, access auditing, export controls, and a database
engine capability review. It must never reuse Git, ordinary operation audit,
query history, logs, or the default ledger columns.

Generated migrations create paired `.migration.sql` and `.rollback.sql`
workspace documents atomically. Pre-commit validation parses staged SQL,
reports formatter drift and affected objects, requires generated pairs to be
staged together, and rejects secret-shaped material. Transactional migrations
can be executed and rolled back only after the user explicitly confirms the
selected connection is a test database; non-transactional plans fail closed.

**Consequences.** Review history, execution history, and native database
identity remain distinct but navigable. Auditors can filter/export a redacted
tamper-evident trail, developers can move from commit or table to executions,
and normal Git workflows cannot accidentally capture result data or secrets.
Installations needing stronger delivery guarantees configure native auditing
and an external collector rather than assuming cross-system atomicity.

---

## ADR-051 — Provider-Neutral Repository Hosting Boundary

**Context.** Browser links, pull requests, and CI checks enrich Git review, but
hosting APIs are not Git transport. Reusing fetch/push credentials would grant
unnecessary API scope and blur user identity. Accepting arbitrary hosts or
redirects while attaching a token would also create a credential-exfiltration
path.

**Decision.** A typed `HostingProvider` boundary owns repository discovery,
pull-request association, checks, repository listing, and pull-request
creation. Sift recognizes only credential-free HTTPS remotes on the explicit
`github.com`, `gitlab.com`, and `bitbucket.org` host allowlist. Repository,
branch, commit, and file links are constructed from validated path/ref segments;
arbitrary remote URLs are never opened as provider API endpoints.

GitHub implements API-backed repository picking, branch pull requests, commit
checks, and pull-request creation. GitLab and Bitbucket initially provide safe
browser links behind the same identity contract; unsupported API mutations
fail explicitly. Provider HTTP calls use fixed API origins, bounded responses,
timeouts, and disabled redirects. Hosting tokens are per-principal SecretStore
values referenced by opaque SQLite handles in a namespace distinct from Git
transport credentials. They never enter repository URLs, arguments, logs, Git
environment, operation audit, or workspace files.

Pull-request creation requires the current shared branch, current repository
revision, enabled network policy, editor authority, and an explicit user
action. Review-comment mutation remains deferred until local review has usage
evidence and a separate typed comment contract; it is not simulated through a
generic HTTP or shell escape hatch.

**Consequences.** Hosting is optional and degrades to local Git plus safe links.
Users can inspect and create review artifacts without widening Git credential
scope. New providers can implement the typed boundary without changing the
repository adapter or exposing arbitrary network requests.

---

## ADR-052 — Tenant-Scoped Server Vaults Separate Secret Use From Reveal

**Context.** Teams need one place to store database connections, passwords,
tokens, and sensitive notes. Existing connection profiles already keep secret
bytes behind `SecretStore`, but they do not provide personal/team grouping,
explicit sharing, immutable history, or a safe way to model secrets a human
must read. Treating every vault permission as an ascending role would also let
someone who may rotate a credential learn its previous value, while treating a
database connection like a shared password would unnecessarily deliver driver
credentials to clients.

**Decision.** A vault belongs to one tenant and is either the unique personal
vault of one principal or an explicitly granted team vault. Vault grants store
independent `inspect`, `use`, `reveal`, `edit`, and `manage` capabilities;
`edit` does not imply `reveal`. Tenant administration may recover `manage` but
does not implicitly gain `use` or `reveal`. Authorization intersects current
tenant membership, the vault grant, item kind, connection policy, and room role
where applicable. Revocation and cross-tenant identifiers fail closed.

Vault items are typed as connection, login, token, or secure note. SQLite
contains redacted metadata, immutable version records, and random opaque secret
handles only. Secret bytes remain in `SecretStore` under a distinct namespace.
Every write creates a new handle before committing metadata; retired and
orphaned handles are processed by a durable retryable cleanup queue. Restore is
append-only and copies retained secret bytes to a new server-generated handle.
No client can submit or receive a handle.

Connection credentials have only a **use** path: the server resolves them into
the existing connection-profile/session/driver flow and never returns them.
Login, token, and secure-note items may have a separate **reveal** path. Reveal
is an item-specific, rate-limited, non-cacheable `POST` requiring explicit
capability plus recent interactive digest-bound step-up; API tokens,
refresh-token-only sessions, background jobs, WebSockets, and connection items
cannot reveal. Normal list/detail/history/diff responses are always redacted.
The desktop holds a revealed value only in a focused temporary surface, masks
it on focus loss or after 30 seconds, and never persists it.

Every vault, grant, item, secret, test, restore, use, and reveal action has a
typed `Operation` variant and a transactionally durable sanitized audit record.
Audit contains ids, kind, revision, actor, and outcome, never metadata capable
of carrying secrets, secret bytes, or handles. Secret-bearing types have
redacted `Debug`, strict body limits, TLS outside trusted loopback,
`Cache-Control: no-store`, and sentinel coverage across logs, errors, protocol
examples, backups, crash recovery, Git, CRDT state, and desktop presentation.

The canonical management UI is a keyboard-navigable Vault view in the
Collaboration dock with My Vault and Team Vaults roots. Connections may show
shortcuts to vault-backed connection items but do not duplicate access or
history management. The full delivery order and graduation tests live in
`docs/PLANS/collaborative-connection-vaults.md`.

The default admission limits are 160-byte labels, 32 KiB metadata, 64 KiB
secret values, 100 vaults per tenant, and 1,000 items per vault. The server
retains at most 50 immutable versions per item. Cleanup processes 100 handles
every 30 seconds and retries from 30 seconds up to one hour. Operators may
tighten or raise these values through the typed `vault` instance configuration;
zero limits and inverted retry bounds fail validation.

**Consequences.** A team member can use a shared database credential without
learning it, an editor can replace a secret without reading the prior value,
and explicitly authorized people can share human-readable secrets through a
narrow audited reveal surface. The server and its secret backend remain inside
the confidentiality boundary; end-to-end encryption and external secret
brokers require separate designs.

---

## ADR-053 — Dialect-Owned SQL Intelligence And Explicit Execution Results

**Context.** Sift has one revisioned semantic document service and both drivers
already stream `Page::NextResult` boundaries, but the desktop retains only the
first result set and the HTTP adapter rejects a second. The current semantic
catalog projection carries object and column names, which is enough for basic
completion but not trustworthy hover types, expression inference, or star
expansion. Treating PostgreSQL and T-SQL as small variations of one generic SQL
grammar would also make completion and edits least reliable where users depend
on them most: aliases, temporary objects, batches, functions, variables,
quoting, and type coercion.

**Decision.** Core continues to own semantic document identity, immutable
revisions, resource limits, catalog authorization, cache lifecycle, public
routes, and edit validation. Each dialect pack owns lossless parsing,
error-recovery rules, identifier normalization and quoting, scope construction,
type inference, variable compilation, star expansion, and completion
enrichments. Packs return bounded portable facts and edits; they never receive
a driver, secret store, metadata store, or unfiltered catalog. PostgreSQL and
T-SQL remain separate first-party conformance packs behind the ADR-032
capability boundary rather than branches inside UI code.

Completion, hover, and prepared edits bind against the active connection's
policy-filtered catalog revision and database identity. The server builds an
immutable indexed binding view containing stable object ids, kinds, qualified
names, ordered typed columns, nullability, comments, routine signatures, and
explicit completeness. Warm editor requests perform no database I/O. Answers
echo semantic and catalog revisions; the desktop discards stale answers.
`Tab` accepts the active completion in Vim insert mode, while aliases,
qualifiers, CTEs, temporary objects, and current-scope columns outrank global
catalog candidates. An unqualified field is never offered as unambiguous when
multiple visible relations provide it.

The desktop owns activation policy, not completion correctness. In Vim insert
mode it applies a bounded lexical guard, coalesces automatic requests for 180
ms, and retains immediate Ctrl+Space completion. Diagnostics use an independent
1200 ms idle window and disappear when their document revision becomes stale.
Every live query tab records a credential-free instance, tenant, profile,
provider, and database target; the executor rejects semantic work when that
target does not match the opened connection. The shallow schema loaded during
connection setup warms the spec-keyed, single-flight cache shared by metadata
and semantic physical connections, so warm completion performs no driver I/O.

An exact, unquoted identifier extension may immediately narrow the currently
visible completion list while a revision-bound server refresh is pending. This
is a bounded preview, not an exhaustive catalog cache; snippets, deletions,
punctuation, cursor movement, external edits and connection changes invalidate
reuse. The accepted replacement range is mapped to the current text. Server
context travels with the completion reply so local snippet enrichment does not
reparse SQL on the UI thread.

Plain unquoted SQL tokens may also receive immediate, bounded shared-keyword
previews without a round trip. These are lexical suggestions, not local semantic
analysis; dialect-specific and catalog candidates remain server-owned. Tab
pressed while a completion is pending requests an immediate reply and records
acceptance only for that exact revision and caret. Text/cursor changes or
cancellation discard the intent. Completion acceptance edits the existing Vim
buffer instead of rebuilding its key machine. Queued completion takes priority
over diagnostics; read-only analysis may yield to new controls, but semantic
document updates remain serial and are never interrupted midway.

Execution v2 uses an explicit ordered event lifecycle: execution start,
statement start, result-set start, rows addressed to a result-set id,
result-set completion, command completion, notices, progress, terminal error,
and execution completion. Result-set completion is distinct from batch
completion. The server owns execution/result-set ids, backpressure, quotas,
spill, retention, and paging. The desktop renders only the active result grid;
inactive result tabs keep summaries and server page references. Legacy v1
single-result adapters may project the first result set, but must report
truncation rather than silently treating a batch as one result.

Portable progress always reports honest lifecycle facts such as queueing,
connection acquisition, current statement, elapsed time, rows and bytes
received, spilling, and cancellation. Engine-native percentages are optional
extension-trait telemetry sampled on a separate bounded monitor connection.
Missing privileges, monitor saturation, or an engine without determinate
progress degrades to an indeterminate phase and never delays or fails the
query. Sift does not estimate a fake percentage for ordinary queries.

SQL variables use a Sift-owned token form distinct from native PostgreSQL and
T-SQL variables. Value and value-list variables compile only to driver bind
parameters; identifier variables compile only through dialect quoting. Raw SQL
variables are excluded from the initial contract. Query history and audit keep
the template digest, names, kinds, and redacted provenance rather than resolved
secret values. Snippet tabstops remain editor-local and syntactically distinct
from execution variables. Built-in, personal, workspace, and tenant snippets
are dialect-scoped; catalog-generated templates are prepared by the server
against an exact catalog revision.

The locked `Driver` method signatures do not change. The existing
`ExecuteRequest` and streamed protocol shapes may receive a versioned breaking
revision under ADR-017. Engine-only native telemetry is added through `PgExt`
and `MssqlExt`. `sift-protocol` remains pure serde; parsing, dictionaries,
catalog indexes, and UI state stay outside it. Full delivery order and evidence
gates live in `docs/PLANS/sql-editor-productivity-and-execution.md`.

**Consequences.** Editor intelligence follows the selected connection and
database without embedding a second parser in GPUI or querying a database on
every keystroke. PostgreSQL and SQL Server can achieve native depth without
leaking engine details through the core driver trait. Multiple result sets and
commands gain stable independent status, paging, export, and cancellation
semantics. This requires a protocol-versioned execution migration, a richer
catalog binding projection, and two maintained dialect corpora instead of one
permissive generic parser corpus.


## ADR-054 — Bounded Metadata Connection Admission

**Status:** Accepted, 2026-09-06.

The file-backed SQLite pool admits at most 16 simultaneous checkouts. Admission
reserves capacity before opening a connection, never waits for a free slot, and
returns capacity on both initialization failure and guard drop. Idle connections
are reused. SQLite retains its existing five-second busy timeout for admitted
operations. In-memory stores retain their single-connection ownership.

Saturation returns a typed metadata error mapped to HTTP 503 (`metadata_busy`).
Clients must not automatically replay mutations: a service-level failure is not
proof that every preceding side effect was rolled back. This bounds active
SQLite resources independently of Tokio's blocking-thread ceiling. The initial
limit matches the former idle retention cap; throughput tuning requires measured
workloads. Pool exhaustion does not change operation auditing or secret storage.

## ADR-055 — Scoped PostgreSQL And SQL Server Provider Graduation

Status: Accepted, 2026-09-06.

The native PostgreSQL and SQL Server adapters graduate for the bounded support
matrix in [database-provider-acceptance.md](PLANS/database-provider-acceptance.md).
This is a tested daily-driver decision, not whole-engine DBA parity or a blanket
cross-platform/TLS/migration guarantee. PostgreSQL 17.10 and SQL Server 2022
16.0.4250.1 were exercised on Linux with the documented local trust settings.

Native object DDL is generated from engine catalogs, not reconstructed from the
lossy explorer projection. Table exports preserve supported column, identity,
generated/computed, collation, constraint, index and trigger properties. Explicit
unsupported states fence advanced shapes. Rich native column shapes that the
structural migration model cannot preserve are marked and rejected there.
Standalone index addressing, rich index migrations, extension/synonym DDL and
whole-database grants/storage/backup semantics remain outside this decision.

SQL Server estimated-plan capture exclusively owns its physical connection
through SHOWPLAN enable/capture/restore. Restoration failure discards that
connection. Parameterized estimates declare types without interpolating values
and disclose the lack of runtime parameter sniffing; actual SQL Server plans
remain unsupported. PostgreSQL ANALYZE rolls back all statements, including
SELECT INTO, and closes on rollback failure. Nontransactional/external effects
are not made reversible by this wrapper.

PostgreSQL cancellation serializes backend cancellation against connection
reuse, releases stalled output and acknowledges completion after restoring the
slot. SQL Server retains abort-and-discard cancellation. PostgreSQL interval
binds preserve exact microseconds and reject unrepresentable values. Native
batch/value/financial precision limits are documented in the matrix.

Evidence: 19 PostgreSQL live driver tests, 8 SQL Server live driver tests,
2 native DDL round trips with permissions/migration fences, 5 existing PostgreSQL
DDL fixtures, and 2 server execution acceptance tests. Required workspace
formatting, Clippy and tests pass. Local 100,000-row/page/timeout and small native
schema resource measurements are recorded with their limits; enterprise catalog
scale and production certificate verification remain unverified.

The locked core Driver signatures and public protocol shapes do not change in
this milestone. The default-unsupported MssqlExt estimated-plan operation is
server-internal. SQLite can now proceed with its separately designed public
protocol/configuration changes. Broader PostgreSQL/SQL Server DBA features stay
on the canonical product backlog rather than blocking the next provider.

## ADR-056 — Scoped SQLite Provider And Read-Only Metadata Inspection

Status: Accepted, 2026-09-07.

The native `sift/sqlite` provider uses bundled rusqlite on dedicated, admitted
workers. Its scoped daily-use contract is documented in [SQLITE.md](SQLITE.md)
and [database-provider-acceptance.md](PLANS/database-provider-acceptance.md).
Supported Linux file workflows graduate independently from whole-engine DBA
parity and the broader signed/cross-platform provider certification matrix;
the runtime quality label is `ide_capable`.

File authority belongs to the server: operator-configured roots, explicit tenant
IDs, relative existing-file profiles, strict read-only precedence, no credentials,
and denial of symlinks/hard-link aliases and Sift's live state. Root ownership
is an operational requirement, not a sandbox against hostile local replacement.
Unaccepted Windows file handling fails closed. SQLite retains its journal/sync
settings and can create ordinary sidecars for writable files.

Each admitted connection owns one worker, bounded mailbox/output and one active
operation. Progress/interrupt/busy handling observes cancellation; cancellation
discards the connection and managed transaction, with permits retained until
worker exit. Dynamic values retain storage classes. Managed Serializable
transactions and savepoints preserve SQLite's retryable COMMIT BUSY behavior;
raw transaction SQL cannot diverge from server state.

Native stored definitions are the DDL authority. Catalog metadata preserves
nullable-key, rowid-alias, generated, STRICT and WITHOUT ROWID distinctions.
The IDE uses a separately advertised **partial navigation catalog**, bypassing
shared schema caches and preserving physical-handle TEMP visibility. This does
not authorize full dependency graphs, diagrams, structural migrations or designer
mutations. Inline writes require proven non-null row identity and reject
generated/virtual/view targets. Estimated plans retain native detail with absent
costs; CSV import uses atomic bounded parameter batches. Actual plans, native
bulk/transfer targets and quarantine import remain unsupported.

Public protocol **2** introduces SQLite engine/configuration/facet shapes and the
local `InspectMetadata` operation. A default-unsupported `Driver::as_sqlite`
downcast provides the engine extension while existing core execution/transaction
signatures remain unchanged. Metadata V046 widens discriminators without dropping
profile parents or cascading credential references. Extension/driver RPC remain
version 1; host compatibility checks use the shared public protocol constant.
Clients and instance/extension locks must be updated together.

The seeded desktop demos add an independent SQLite file and saved connection;
repeated seeding never replaces existing data. Ordinary personal-instance
creation does not inherit the demo fixture.

`sift metadata inspect` is a local-file-owner operation that builds a fresh
allowlisted snapshot and a separate applied instance. It excludes credentials,
authentication state, configuration, SQL/history and document/vault contents.
Names and workspace paths remain visible to the source bootstrap identity.
A consistent read transaction includes committed WAL data; row/value/time limits
bound extraction. The output records its timestamp, truncation and Operation,
and the new instance records a durable audit event. The snapshot is opened through
the ordinary read-only provider path; generic profiles still cannot access or
modify Sift's live metadata. The Nix launcher builds matching desktop/server
binaries and opens the snapshot with one command.

Acceptance: seven real-file driver tests, the real SQLite HTTP/SDK integration,
semantic and GPUI form coverage, metadata upgrade/snapshot coverage, CLI snapshot
acceptance and Nix wrapper/seed validation passed. Workspace formatting, Clippy
with warnings denied and tests passed after the final execution-count/row-bound
fixes. PostgreSQL/SQL Server live driver suites (19/8) and server DDL/plan suites
(2/5/2) passed with protocol 2. Remaining platform/parser/DBA exclusions are
explicit in the evidence matrix and do not block work on the IDE.

## ADR-057 — Catalog JOIN Suggestions And Native Vim Cursor Groups

**Status:** Accepted (2026-09-08).

JOIN completion uses catalog-proven foreign-key column pairs, preserving their
order for composite predicates. It never guesses keys from names or parses
constraint definition strings. Traversal is bounded to three edges and 2,048
expansions; outer joins use direct paths only. Suggestions are literal SQL
completion candidates, so database identifiers cannot become snippet tabstops.
Unsupported contexts or unavailable graphs retain ordinary completion. Graph
fetches use existing schema supervision and cache coalescing, with a 250 ms
completion wait budget. The core Driver contract and public protocol stay intact.

ModalKit owns local Vim cursor groups and edit semantics. Sift renders followers
and mirrors each resulting edit into the existing query-text CRDT, with one
local undo record/update per edit. Authoritative remote text resets local cursor
groups. Result bytes and image caches remain local result presentation state.

General object design reuses structured table migration preview/apply and
canonical PostgreSQL/SQL Server replacement SQL for views, routines and triggers.
Replacement scripts remain bound to their source connection/database and require
normal explicit audited execution. Unsupported replacement semantics remain
unavailable; the designer does not manufacture a drop/recreate migration.
Shared-query browsing projects authenticated room/document entries and opens
them through the existing room service, preserving permission and audit checks.

## ADR-058 — Backend Transfers, Offline Backup Policies And Operator Recovery

Status: accepted. Date: 2026-09-08.

Parquet uses the existing export and transfer authorization/audit boundaries,
with schema-driven primitive columns, explicit text fallback, bounded artifacts
and transactional flat-type imports. No core Driver signatures change. Unsupported
types, conflicting import options and multiple result sets fail explicitly.

State-backup policies preserve ADR-039's offline consistency boundary. An external
timer invokes a due-checking command; Sift never stops a serving process implicitly.
A private ledger owns exact archive UUIDs, checkpoints pending uploads and gates
local retention on successful creation/delivery. Only encrypted archive bytes go
to explicitly configured HTTPS PUT destinations. Signing, remote lifecycle and
service maintenance windows remain operator-owned.

PostgreSQL logical recovery is a separate operator CLI using explicitly selected
client tools, not a new Driver trait method or a Sift-state archive payload.
Connection fields are explicit, password files stay external, inherited libpq
configuration is cleared and raw diagnostics are suppressed. Output publication
never overwrites. Restore validates a private input snapshot and requires explicit
apply to an existing target; apply uses a single transaction without database
drop/create or ownership/ACL replay. Only trusted archives are eligible operationally.

HTTP metrics are administrator-only and audited. OTLP/HTTP export is opt-in and
bounded, with allowlisted method/status/timing fields, no raw paths, SQL, credentials
or result data. This is best-effort request-span observability, not full distributed
driver tracing. Usage, safety boundaries and automated acceptance are recorded in
[backend operations](BACKEND-OPERATIONS.md).

## ADR-059 — Same-Installation Tenant Recovery

Status: accepted. Date: 2026-09-09.

Tenant-selective recovery extends ADR-039's stopped-server maintenance lock,
private staging, encrypted rescue archive and durable replacement journal. It
preserves original IDs because CRDT snapshots, checkpoint history and stored run
definitions contain references that cannot safely be rewritten generically.
Installation identities and schemas must match; referenced principals must
already match destination IDs and external identities. Explicit table ownership
and foreign-key boundary checks refuse ambiguous merges and unknown tables.

Only the selected tenant subtree is replaced. Destination principal/auth state,
unrelated tenants, instance configuration and append-only ledgers remain intact.
Selected invitations and scoped API tokens are revoked; pending approvals block
recovery because their context cannot be isolated by tenant. Imported schedules
and projections are disabled, repository credentials removed and active work
made terminal. Opaque extension storage requires a future migration contract.

The operator selects `backup restore-tenant --tenant-id ID`; preview is the
default and requires the stopped-server maintenance lock. An existing tenant
must match the source name and kind. The recovered subtree includes memberships,
connection profiles and credentials, rooms and documents, saved queries and
scoped history, workspaces and checkpoints, DDL, run definitions and history,
transfer recipes, plans, catalog snapshots, and vaults. Filesystem projections
stay disabled. Artifacts, projection reconciliation state, repository credentials,
unscoped principal history, and external database state are excluded. No
scheduler resumes imported work automatically.

Portable connection/vault secrets are copied into a staged destination store
under fresh handles; source authentication keys are never imported. Preview
validates the full staged result with audit-only destination writes. Apply
installs secrets before metadata using the existing recoverable journal.
File-secret archives and destinations are supported; memory mode requires no
selected secret references. Old unreferenced destination secrets remain for
separate garbage collection. Apply first creates an encrypted rescue backup.
Tests cover two tenants sharing a principal, scoped recovery, unchanged unrelated
authentication and secrets, identity/ID/FK/schema refusal, secret remapping,
and rescue-journal recovery. Cross-instance identity migration and external
database DR orchestration remain separate work.

## ADR-060 — Private Immutable Benchmark Snapshots

Status: accepted. Date: 2026-09-09.

Saved benchmark runs are immutable user-submitted snapshots, distinct from
rerunnable benchmark definitions and server-attested execution records. The
server bounds and validates samples, recomputes summaries and completion, and
never executes SQL while saving, browsing or opening a snapshot. Bind values and
result rows are absent. Ordinary execution timings remain separate from future
instrumented profiling populations.

Names, SQL, warnings and samples may be sensitive. ADR-008's SecretStore holds
the complete payload; SQLite holds owner/tenant/run IDs, timestamp, byte count
and an opaque handle. Every API operation is audited and membership plus owner
identity restricts access; tenant membership alone does not expose peer runs.
Private storage is capped without automatic expiry or silent eviction.

Secret publication precedes index insertion, with compensation on rejected
inserts. Secret deletion precedes index deletion so failed deletion can be
retried. These two stores are not one transaction: process failure can leave an
unindexed secret or an unavailable index entry. Unavailable entries remain
browsable and deletable. Full recovery needs both stores; tenant-selective
recovery copies benchmark payloads under fresh opaque handles. Definition
storage, parameter-aware reruns, sharing and orphan collection remain separate
work. See the [workbench checklist](PLANS/query-performance.md).

## ADR-061: Backend-owned tailnet transports and explicit remote exposure

Database network access belongs to the Sift backend. Optional `sift_network`
profile configuration selects direct tailnet access, SSH loopback forwarding,
or direct-TCP-first fallback. Device discovery uses the installed Tailscale
daemon. Neither desktop credentials nor a desktop-only tailnet membership are
implicitly transferred to a hosted backend.

Tailnet inventory, host-key scans, transport use, and Serve management require
instance-admin access because they exercise the backend's network/SSH identity.
OpenSSH runs non-interactively with strict host verification and no user SSH
configuration execution. Explicitly verified Ed25519 public pins may be stored
in profile configuration; private keys remain in the backend's existing agent
or default identity files. Database credentials remain in SecretStore.

Connection-owned loopback listeners avoid port reservation races. Dropping a
tunnel lease cancels forwarding tasks and kills owned SSH child processes.
Idempotent reconnect recreates the transport; no SQL replay or authentication
fallback is introduced. TLS certificate verification is never weakened.

Tailscale Serve is separate from connection establishment and profile deletion.
Its fixed remote helper requires preview revision plus explicit exposure
acknowledgement, rejects existing rules and Funnel-enabled devices, and keeps a
private instance-bound remote ownership journal. It refuses removal after an
external configuration change or uncertain apply. No distributed transaction
with external tailscale administrators is claimed: the helper serializes Sift
operations, while interrupted or conflicting outcomes require inspection.

Candidate validation does not publish temporary profiles or sessions. Existing
credentials can be reused server-side, with tenant-admin authorization required
when changing their destination/configuration. Validation and upsert remain
separate operations, so a later database outage can leave a validated saved
profile. This is safer than deleting a possibly concurrently updated profile.

See [Tailnet setup and recovery](TAILNET.md).

## ADR-062 — CSV type choices remain server validated

Status: accepted. Date: 2026-09-23.

The desktop CSV preview owns one SQL type input per source column, initially
showing the inferred type for the selected engine. Blank input restores that
inferred type. The desktop validates each
explicit value as one bounded SQL data type before generating reviewable DDL or
preparing a transfer recipe. The CSV API repeats that validation at execution;
recipe options and desktop validation never become an authorization boundary.

For a new table, the preview opens reviewable CREATE TABLE SQL using the chosen
types. For an existing table, import sends the selected mapping to the CSV API.
The preview can populate a transfer recipe with type mappings, table, conflict
policy, and create-table choice. Reopening a recipe restores those choices
before sending a normal audited execution request. Existing target tables use
their actual column types. For a new table, skip and quarantine ingestion casts
against the same explicit types used in CREATE TABLE. Durable resume remains
incompatible with explicit type mappings and table creation.

## ADR-063 — PostgreSQL object administration uses typed preview and apply

Status: accepted. Date: 2026-09-29.

Database extensions are PostgreSQL objects, separate from Sift extension
packages. The first workbench slice reads bounded catalogs through existing
supervised driver execution. A typed action generates exact quoted SQL on the
server; the preview includes a digest of the observed catalog state. Apply
requires confirmation and repeats authorization and catalog inspection before
running that SQL through the supervised query path. The digest detects ordinary
stale previews but is not a database lock; PostgreSQL remains the final owner,
permission, and dependency authority. Managed schema-restricted profiles are
denied this cross-schema workbench, and read-only profiles cannot apply. Each
read, preview, and apply has its own audited Operation. Partition attach,
extension update, dependency graph previews, and atomic cross-object plans
require separate designs.

## ADR-064 — A Connection Dashboard Is A Bounded Read-Only Snapshot

Status: accepted. Date: 2026-09-29.

The database server dashboard is a per-connection inspection surface. Its
server-owned response contains a sample time, engine, a bounded process-count
summary, and the effective capability states for its drill-down views. It
never returns SQL text, connection secrets, or a raw process list. The process
summary reuses the existing 500-row PostgreSQL and SQL Server monitor query;
at the cap it reports that the counts may be incomplete. SQLite reports the
process section as unsupported while still returning its connection and
capability state.

`ReadServerDashboard` is a distinct audited read operation. The handler checks
connection ownership and effective authorization before sampling. Database
monitoring permission failures produce a section-level permission state, so
the rest of the dashboard remains usable. Driver calls retain the existing
execution timeout and cancellation boundary. No `Driver` trait method is
added. The desktop opens the Overview tab through a Vim-accessible command
and refreshes on demand; it does not poll in the background.

## ADR-065 — PostgreSQL diagnostics are bounded permission-aware snapshots

Status: accepted. Date: 2026-09-29.

Replication and statistics inspection are separate audited read Operations,
available only on PostgreSQL connections. Server-owned fixed SQL runs through
the existing supervised execution path. Replication inspection first checks
effective PostgreSQL monitoring access and returns an explicit denial without
querying replication catalogs when the role lacks it. Its result includes only
sender, WAL receiver, and slot fields needed for diagnosis, omitting connection
strings, addresses, and SQL text. Sender and slot lists have hard row caps and
truncation flags.

Statistics inspection returns current-database cumulative counters and a
bounded page of user-table counters for relations with SELECT privilege.
Unavailable values remain nullable, and a single snapshot does not imply a
rate or predict replication catch-up time. Managed schema-restricted profiles
cannot access these cross-schema catalog views. The desktop Monitor has
explicit refresh and Vim navigation, with no automatic polling or mutation.

## ADR-066 — PostgreSQL administration starts with bounded catalogs and typed changes

Status: accepted. Date: 2026-09-29.

The first database administration slice exposes bounded PostgreSQL role,
schema, database, and explicit schema-grant catalogs. Role passwords and
connection credentials never enter these responses. Catalog reads use the
existing supervised PostgreSQL connection and audited object-read operation.

Role creation, schema privilege grants and revocations, and database or schema
ownership changes use typed actions. The server quotes every identifier,
generates the SQL for preview, hashes the observed catalog state with that SQL,
and repeats the read and policy checks at apply. Apply requires both preview
matching and explicit production confirmation, independent of any desktop
prompt. PostgreSQL remains the final authority for ownership, role membership,
and privilege delegation. The server checks visible ownership and role-creation
authority early and surfaces PostgreSQL's rejection if permissions change.
The existing supervised execution path supplies timeouts and cancellation;
administrative writes cannot join an active editor transaction.

This is a foundation for the database administration editor. Password rotation,
role attribute changes, membership delegation, object-level grants, default
privileges, and a complete privilege matrix require separate designs.

## ADR-067 — SQL Server constraint state stays native and migration-fenced

Status: accepted. Date: 2026-09-29.

For ordinary rowstore tables, native SQL Server table export preserves disabled
and enabled-but-untrusted CHECK and foreign-key constraints. The exporter first
creates each constraint, then emits ordered `NOCHECK CONSTRAINT` statements;
an untrusted enabled constraint is re-enabled with `CHECK CONSTRAINT` without
validating old rows. Constraint names are quoted. SQL Server's catalog remains
the authority for each state. `NOT FOR REPLICATION` remains an explicit native
DDL refusal until its behavior is verified.

The generic catalog diff has no constraint-state model. SQL Server graph
snapshots therefore fingerprint advanced constraint state and fence structural
migrations for affected tables, including unsupported replication flags. A
live fixture replays generated DDL into a second schema and compares the
regenerated native definition before this support is claimed.

## ADR-068 — SQL Server security editing begins with scoped catalog snapshots

Status: accepted. Date: 2026-09-29.

The first SQL Server security workbench reads bounded, metadata-visible server
logins, database principals, role membership, schema ownership, and explicit
schema permissions. It never reads password hashes, credential material, or
login secrets. Fixed-role and inherited permissions are omitted explicitly;
the catalog is not an effective-privilege matrix. Each section reports when
SQL Server denies its read rather than implying an empty result.

Typed database-role creation, membership changes, and schema SELECT grants or
revocations use server-generated SQL with escaped identifiers. Preview checks
the current database state and relevant authority, then hashes that state with
the SQL. Apply repeats authorization and preview before supervised execution;
it requires the matching preview token and an explicit production confirmation.
SQL Server is the final authority for permissions at execution time. The API
does not manage logins or passwords, and it does not accept SQL fragments.

This is a foundation for the editor. Login creation/password rotation, user
mapping, DENY handling, ownership changes, fixed-role permission explanation,
and a complete effective-privilege matrix remain separate work.

---
## ADR-069 — PostgreSQL policy renames use a guarded workbench action

**Status:** Accepted (2026-09-29)

The first row-level security editor slice lists policies through a bounded
PostgreSQL catalog query and permits a typed policy rename. A rename preserves
the policy command, roles, expressions, and permissive mode while giving users
a useful, low-risk correction path. Policy expressions are display-only; the
server never accepts expression text as a SQL fragment. Long displayed role
lists and expressions carry explicit truncation flags.

Only the table owner (including effective role membership) or a superuser may
preview a rename. Preview captures the policy and table catalog state and the
absence of the target name in a digest. Apply requires that digest, explicit
confirmation, production confirmation, and a fresh authority/state lookup.
The database remains the final authority if state changes between lookup and
execution. Both reads and writes use the existing supervised PostgreSQL object
operations and audit path. The desktop exposes this through the Vim Monitor
workbench. Broader policy definition editing requires a separate design for
expressions, role semantics, and reversible security changes.

---
## ADR-070 — SQLite Snapshot Migrations Admit Only Proven Simple Table Creation

Status: Accepted, 2026-09-29.

SQLite's native stored DDL is more expressive than the structural catalog graph.
This decision supersedes ADR-056's blanket structural-migration exclusion for
the one additive shape below.
The graph deliberately has partial dependency coverage, so an absent target
object cannot authorize a drop, and a table reconstructed from columns could
silently lose STRICT, WITHOUT ROWID, generated columns, checks, keys, indexes,
triggers, or virtual-table semantics. A general table rebuild is therefore not
an acceptable first migration path.

The existing audited, managed-profile snapshot, diff, preview, validation and
apply lifecycle is reused. A SQLite table node carries an executable marker
only when the driver verifies its stored native DDL against a narrow grammar:
an ordinary `main` table with one or more named columns, each declared as one
of INTEGER, REAL, TEXT, BLOB or NUMERIC, optionally NOT NULL, and no other
clauses or child objects. The marker is a canonical generated statement, never
an arbitrary stored SQL fragment. Preview independently checks that marker
against the graph's column details, rejects every other change kind and target
shape, and always makes one transactional group. `temp`, virtual tables,
defaults, keys, constraints, indexes, views, triggers, alterations, renames,
drops and unknown changes remain unsupported. A filtered or truncated graph
cannot make an unsafe shape executable.

Apply retains the existing scope, policy-revision, live-catalog-revision,
one-use digest, acknowledgement, per-connection lock, bounded execution,
rollback, cancellation and audit checks. It uses SQLite's managed Serializable
transaction mode. The SQLite file policy remains the write authority; a
read-only root or profile cannot be bypassed by preview or apply. No Driver
trait or public protocol signature changes.

This admits a useful snapshot-to-live additive migration without pretending
that partial dependency coverage proves a destructive change safe. Broader
native DDL round trips and designer changes need a separate supported-shape
contract and real-file acceptance before this restriction can be widened.

## ADR-071 — SQL Server process termination uses a fresh Monitor target

Status: accepted. Date: 2026-09-29.

The desktop uses the existing audited `KillProcess` operation for SQL Server
session termination. Vim navigation chooses a row from the bounded process
snapshot, then opens a confirmation that names the session, login, database,
and rollback consequence. A confirmation is accepted only while that snapshot,
connection, and selected session are still current. Refreshing, reconnecting,
or losing the session invalidates it. One request is allowed at a time; results
from an earlier connection or request are ignored. The server remains the final
authority and reports missing `ALTER ANY CONNECTION` permission from SQL Server.

The UI also shows the negotiated `KillProcess` capability reason before a
request. It does not claim a SQL Server permission preflight: the server can
revoke rights after the list was fetched. Canceling an editor query and killing
another server session are separate operations. Server session IDs can be
reused between observation and execution, so the confirmation is a careful UI
guard, not an atomic database guarantee.

## ADR-072 — PostgreSQL sequence ownership stays native and migration-fenced

Status: accepted. Date: 2026-09-29.

Standalone sequence DDL includes an `OWNED BY schema.table.column` statement
for an ordinary auto dependency. It preserves configuration, not the live
counter. An identity sequence is an internal table component; an extension
member belongs to its extension. Export refuses both instead of creating a
second independent object. It also refuses ambiguous dependency states.

The catalog graph fingerprints sequence configuration and ownership on the
sequence node. Owned sequences and their owner tables are marked unsupported
for generic structural migration rendering, because the portable schema model
cannot safely reconstruct the ownership relation or its dependency order.
Native DDL remains available for a sequence owner to review and replay after
the owning table exists. A detached sequence remains independently exportable.

Indexes already exported as part of PostgreSQL table DDL are not a public
standalone object. Adding a standalone index `ObjectKind` requires the
ADR-017 protocol bump and a separate design for constraint-backed, partitioned,
extension-owned, and invalid index states. Graph metadata fingerprints these
states and fences affected table migrations until that design is complete.

## ADR-073 — SQL Server CSV import uses reviewed column names and a retained recipe for resume

Status: accepted. Date: 2026-09-29.

The desktop CSV import starts from a SQL Server table or an explicitly chosen
connection. It parses at most 64 MiB locally, displays a bounded row sample,
and lets the operator map each source header to one distinct target column.
For an existing table, the server remains the authority for target columns and
types; a dry run validates CSV shape and authorization but cannot promise that
every database conversion or constraint will pass. The reviewed target names
are written to a temporary in-memory CSV payload; source row values are never
changed. Type overrides apply only to a proposed new table and remain subject
to the server's type validation. Identity, computed, rowversion, and generated
columns are not given special insertion semantics; the operator must omit
those source columns or use a separately reviewed SQL path.

The direct import retains one request generation and a visible pending/result
state. Local cancellation drops the HTTP waiter and invalidates late replies;
it does not promise that SQL Server rolled back work already committed. A
separate transfer recipe provides server-side durable resume for an existing
table with Abort policy, no manual offset or type override, a stable run UUID,
and a target checkpoint table. The desktop exposes this as the retry path,
preserves the same recipe identity and file on retry, and states that progress
within an in-flight request is not observable. Every server import, preview,
recipe mutation, and execution uses the existing audited operations, policy
checks, bounded driver calls, and capability negotiation. No Driver trait or
wire contract change is needed.

## ADR-074 — SQLite native bulk targets bind typed rows under a preview lease

Status: Accepted, 2026-09-29.

SQLite has no separate wire-level bulk protocol. Its native bulk target is a
bounded sequence of prepared, parameterized multi-row inserts on the admitted
SQLite worker. Existing CSV import and transfer recipes already use that
mechanism, but their dry run does not prove destination affinity or exact
decimal storage. The generic SQL Server CSV bulk route must not silently grow
SQLite semantics, and the locked `Driver` signatures stay unchanged.

Extend the existing audited `BulkInsert` operation additively for SQLite
`Native` format with column names and typed `Value` rows. Preview and apply
share this operation kind and route. The server validates the complete bounded
batch, target table metadata, and every value before issuing a short lived,
scope-bound, one-use preview token. Apply requires that token and explicit write
confirmation, repeats authorization and validation, and refuses a changed
request or table shape. The audit record drops both CSV bytes and native rows.
Only an authorized existing ordinary `main` table can be targeted; reserved
`sqlite_` internal tables are refused. The file policy remains the final
read/write boundary.

The safe affinity matrix is intentionally narrow: integer and boolean values
to INTEGER or NUMERIC, finite float values to REAL, text/date/time/UUID/JSON
to TEXT, bytes to BLOB, and null only to nullable columns. Decimal values use
canonical base-10 text with at most 38 digits and 18 fractional digits, and
only a TEXT-affinity destination; NUMERIC and REAL may silently round them.
Generated, hidden and virtual columns, triggers, ambiguous target metadata,
and unsupported value kinds are refused. No automatic SQLite coercion is
presented as lossless conversion.

Admission bounds rows, columns, serialized bytes and parameters. Execution
uses the existing supervised query path in finite batches within one managed
Serializable transaction with a 120-second deadline checked between batches;
cancellation, timeout or any batch failure rolls
back, and rollback failure discards the connection. The current HTTP request
is bounded in memory; this decision does not claim unbounded socket streaming
or a resumable native batch. Existing CSV/transfer recipes remain the path for
uploaded files and retain their own checkpoint semantics.

## ADR-075 — Standalone PostgreSQL indexes use a public index object kind

Status: accepted. Date: 2026-09-29.

`GenerateDdl` accepts `ObjectKind::Index` and the wire protocol advances to
version 3. A graph index node already has a PostgreSQL catalog OID, but its
parent table path is not an unambiguous standalone DDL target. The new kind
uses the index's schema and name to resolve its catalog identity on the server;
it does not add a method to the locked driver trait. The existing audited,
supervised `GenerateDdl` operation remains the authority and execution path.

Native export uses `pg_get_indexdef` so expressions, operator classes,
collations, included columns, predicates, and storage options retain their
catalog syntax. It requires effective table ownership and refuses invalid or
not-ready indexes, constraint-backed indexes, extension members, attached
partition indexes, partitioned index parents, clustered or replica-identity
state, and ambiguous or missing targets. Replaying a standalone index requires
its table and referenced dependencies to exist first. These refused states
remain explicit instead of silently producing an ordinary `CREATE INDEX`.

The graph fingerprints every index's native definition and state on its index
node and owning table. Generic migration rendering is fenced on affected
tables because the portable table/index projection cannot represent the full
native shape or dependency order. This is a safe diff boundary, not automatic
rich-index migration support. PostgreSQL table export still includes supported
indexes with the table; callers should select one export path for a replay.

## ADR-076 — Native index migration requires a stable existing table

Status: accepted. Date: 2026-09-29.

PostgreSQL graph snapshots with `include_definitions=true` retain catalog-owned
`pg_get_indexdef` text only for indexes that pass the ADR-075 standalone export
guards and whose table owner is effective for the current role. A native index
hash remains available in every graph. The migration renderer consumes native
index SQL only from a complete PostgreSQL live or server-captured snapshot,
never from a DDL source model. It verifies the source/target graph relationship,
the index's schema and parent table, and the stable table shape before emitting
the statement. This remains the audited `PreviewMigration` and guarded apply
flow; no new driver method or SQL fragment from a client is accepted.

The first renderable shape is create or drop of an independent index on a table
that exists unchanged in both graphs. A derived table diff caused only by its
index-set fingerprint is suppressed; the index node remains the actual change.
Create uses the captured native definition and drop uses a quoted, qualified
name with `RESTRICT`. The diff's proven parent/dependency edges order any
selected prerequisites; the initial supported path has no parent creation.
An index alter, unproven dependency, partial
catalog coverage, stale or missing native definition, constraint/extension/
partition member, owned sequence, partition hierarchy, or policy-bearing table
remains unsupported for automatic rendering. This boundary does not turn table
creation, table options, sequence ownership, partitions, or policies into
automatic migrations. Their native DDL stays available for review and manual
ordering until the graph can prove a complete multi-object replay.

## ADR-077 — PostgreSQL desktop maintenance uses audited explicit-target operations

Status: accepted. Date: 2026-09-29.

The Vim Monitor maintenance view uses the existing `postgresMaintenance` and
`checkIntegrity` HTTP operations through the SDK. It requires an explicit
schema and table or index name; it never defaults to a whole database. VACUUM,
ANALYZE and REINDEX first return the exact quoted SQL without executing it.
The desktop retains that preview only for the current connection generation,
unchanged target and action, and a typed confirmation. The server continues to
enforce connection policy, PostgreSQL authority, transaction state, bounded
driver execution and audit. A stale preview or response cannot authorize an
apply after switching or reconnecting. The read-only integrity action uses
`amcheck.verify_heapam` only when amcheck is installed; it reports the bounded
findings and incomplete state without claiming a whole-database check.

PostgreSQL custom-archive dump and restore remain operator-only CLI actions.
Their current `postgres_backup::run` contract reads a local specification and
archive, validates tool paths and target identity, and audits against the
server's local metadata store. The desktop can point at a remote server, so
launching that CLI from the desktop would cross the file, credential and audit
boundary. A future desktop path needs a server-owned, policy-scoped endpoint
with a bounded archive transfer and explicit target confirmation. This change
does not add a wire operation or Driver trait method for dump and restore.

---
## ADR-078 — SQLite file maintenance stays inside the configured root

Status: accepted. Date: 2026-09-29.

SQLite file creation and backup are server-side file operations, not SQL sent
through the ordinary query editor. The workflow starts from an existing
managed SQLite connection. Its profile must be read/write, and the requested
destination must use the same configured root and tenant as that connection.
The native SQLite extension, which owns `FilePolicy`, validates the root and
each path component; the locked `Driver` trait does not change. On Unix, the
parent directory must already exist within the root, be free of symlinks,
and be outside protected paths. A destination must be absent and is created
exclusively; overwrite, restore, delete, arbitrary absolute paths, ATTACH,
and Windows file mutation are outside this decision. Root directories are
operator-owned, so this is not a defense against a local process replacing
them during a call.

An audited SQLite maintenance operation has explicit preview and apply phases.
The public `ManageSqliteDatabase` operation and typed request advance the wire
protocol to version 4; the locked core `Driver` trait remains unchanged.
Preview reports source/destination identity, current file size, expected
backup artifact and warnings without changing files. The server issues a
short-lived, one-use token bound to session, connection, action, paths, root,
and observed file identity. Apply requires that token and a separate explicit
confirmation, repeats authorization and preflight, and refuses changed files
or destinations. A client disconnect cancels the work where the native API
permits; a late result cannot be treated as a new preview. Failure removes
any incomplete newly created destination and never replaces an existing file.

The supported actions are creation of a new empty database and online backup
of the connected `main` database to a new file. Creation writes a valid empty
SQLite database and leaves connection profile registration to a separate user
action. Backup uses SQLite's online backup API, in bounded page steps with a
time and size limit, so WAL state is captured consistently. The preview states
that creation has no prior data to back up and that existing databases should
be backed up before any later mutating maintenance. Integrity checks remain
the existing read-only `CheckIntegrity` operation. VACUUM (later admitted by
ADR-085), implicit repair,
checkpoint, PRAGMA write, extension load, and general database-admin commands
remain gated pending a bounded cancellation design.

The request and result contain relative logical paths only. Operation audit
records action, target, preview/apply outcome and artifact path, never file
contents or secrets. File-root, tenant, connection-policy and protected-path
checks remain the authority even when the user has an administrative tenant
role. The desktop may expose only these typed actions in Vim mode.
## ADR-079 — PostgreSQL heap storage and index table-state export

Status: accepted. Date: 2026-09-29.

PostgreSQL table DDL may emit validated catalog `reloptions` for the built-in
heap access method, and replay per-column storage and compression overrides
through `ALTER TABLE ... ALTER COLUMN` after creating the table. The exporter
retains option order and values from `pg_class`/`pg_attribute`; it does not
accept option fragments from a client. A non-default table tablespace, non-heap
table access method, TOAST relation options, column FDW/options state, or extension
member remains an explicit unsupported state until its external dependencies
can be proven.

`pg_get_indexdef` recreates an index definition but omits the table's chosen
clustered index and replica identity. Table export restores these with
`ALTER TABLE ... CLUSTER ON` and `ALTER TABLE ... REPLICA IDENTITY USING INDEX`
after all indexes/constraints exist. Standalone index DDL includes the same
post-create table-state statements for a supported index. Invalid or not-ready
indexes remain unsupported: normal `CREATE INDEX` cannot faithfully recreate
their transition state. Partition-attached, constraint-backed standalone, and
extension-member indexes retain the ADR-075 refusal boundary. This supersedes
ADR-075's clustered/replica-identity refusal for otherwise supported indexes.

The catalog graph fingerprints heap and TOAST options, column
storage/compression, and index state and fences generic structural migrations.
Native DDL is a reviewable export; this decision does not enable automatic
migration of these table-wide settings, nor infer tablespace or access-method
availability on another server.

## ADR-080 — Runtime administration shows bounded operation and request audit rings

Status: accepted. Date: 2026-09-29.

The Vim Runtime administration modal exposes the existing read-only
`/v1/operations` and `/v1/audit` SDK calls as separate Recent operations and
Request audit views. `operations` is a replayable in-memory ring of executed
operations, not a static catalog of operation kinds; each row displays only
`Operation::audit_summary()` and status, never the serialized operation body.
`audit` is the in-memory HTTP request ring with method, path, status and
duration. Neither endpoint supplies a cursor, so the desktop displays a fixed
maximum of the newest rows from each bounded server response, with an explicit
refresh instead of implying durable pagination. Durable operation audit remains
the existing keyset-paged view.

Every load carries a UI generation. Switching administration sections or
refreshing invalidates older responses so a delayed result cannot overwrite the
current view. The Runtime administration entry point stays Vim accessible;
these read-only lists add no mutation or new driver method.

## ADR-081 — SQLite desktop maintenance binds confirmation to the live connection

Status: accepted. Date: 2026-09-29.

The Vim Monitor maintenance view exposes the existing audited SQLite file
maintenance and integrity APIs only for an active managed SQLite connection.
The form accepts a path relative to the connection's configured root; the
preview response shows the root ID, source, destination, source bytes, and
backup expectation before a write. The desktop never accepts a server path or
executes maintenance as SQL.

The desktop keeps a preview only for the active connection generation, exact
action and destination. Apply requires a typed confirmation naming that action
and destination, plus the server's one-use preview token. A connection switch,
form edit, stale reply, or completed apply clears the preview. The server
remains authoritative for tenant/root permission, file identity, token expiry,
exclusive creation, backup limits, and audit. Integrity runs as the existing
read-only check without a write confirmation. VACUUM (later admitted by
ADR-085), restore, delete and
arbitrary PRAGMA remain outside this desktop workflow.

## ADR-082 — PostgreSQL extension export is a version-pinned install recipe

Status: accepted. Date: 2026-09-29.

`GenerateDdl` may export a PostgreSQL database extension as a quoted `CREATE
EXTENSION ... WITH SCHEMA ... VERSION ...` statement. The first verified
package is PostgreSQL 16 `pg_trgm` 1.6 with its pristine 46-member manifest.
Export resolves catalog name and schema, requires effective ownership, and
checks that the exact version is locally available and relocatable. It refuses
any changed member manifest, config table, extension-to-extension prerequisite,
or relation/sequence member. The statement is a package install recipe: the
target must have the same extension package/version, and an operator must
verify resulting membership. It is never an independent export of member
objects or a complete cross-server backup.

The PostgreSQL graph fingerprints extension version, schema, prerequisites,
configuration, and catalog member identities. Visible member nodes are marked
with their owning extension, with a catalog-proven dependency edge when the
extension node is also in scope; both the extension and its members are fenced
from generic structural migration. Native DDL for extension-owned tables,
indexes, sequences, views, routines, types,
triggers, and foreign tables refuses independent creation. Automatic extension
install/update/drop migration, package equivalence across servers, membership
changes made through `ALTER EXTENSION ADD/DROP`, data in config tables, and
cross-object ordering require a separate proof and remain unsupported.

## ADR-083 — Desktop approval requests bind a reviewed extension action

Status: accepted. Date: 2026-09-29.

The Vim Extensions view may create an operation approval only for its selected,
invocable host-projected contribution. It builds the same typed operation,
arguments, and required target context as invocation, then previews the
classification, contribution/action, scoped target, and canonical input
fingerprint without displaying secret form values. The user must type the exact
operation-and-target confirmation before the desktop calls the existing
`createOperationApproval` SDK method. Changing the selection, form arguments,
target context, or selected server invalidates that preview. The desktop
resolves the `active`
connection marker to its concrete session/connection identity for creation,
as it already does for invocation. The server binds the approval to the
authenticated principal, registered operation, authorized context, and input
fingerprint; the desktop cannot choose the actor, TTL, or approval ID.

The returned approval enters the existing approve-and-run flow. Approve uses
the record's expected revision and remains a distinct action. Running consumes
the one-use approval only with the same operation, context, and arguments.
The review is a local confirmation, not a substitute for server authorization.
No Driver or wire-contract signature changes are needed.
## ADR-084 — Owned sequence creation requires a stable existing table

Status: accepted. Date: 2026-09-29.

A complete, definition-bearing PostgreSQL live or server-captured graph may
carry catalog-generated `CREATE SEQUENCE` SQL for one ordinary sequence owned
by one column of a permanent heap table in the same schema. The first
renderable transition is creation of that sequence when the owner table and
column already exist unchanged in both graphs. The migration renderer proves
the catalog `OwnsSequence` edge, matching table/column identities and details,
and that the table's only change is its derived owned-sequence fingerprint.
It emits `CREATE SEQUENCE` before `ALTER SEQUENCE ... OWNED BY`; rollback drops
the sequence with `RESTRICT`. A selected table-fingerprint change is absorbed
only when its paired sequence creation is selected and passes the same proof.

The captured definition is bounded and accepted only for built-in integer
sequence types, one ordinary ownership dependency, and effective sequence
ownership. Identity or extension members, nondefault persistence, ACLs,
comments, other table-native state, concurrent table/column edits, missing
graph nodes, incomplete coverage, DDL-source graphs, and unproven dependency
edges remain unsupported. Sequence counters are runtime data and are outside
schema migration. Policy and partition changes continue to refuse automatic
rendering until their dependency closure can be proved separately.

## ADR-085 — SQLite VACUUM is a bounded in-place maintenance action

Status: accepted. Date: 2026-09-29.

`VACUUM` rewrites the connected `main` SQLite database. It is admitted only
through the existing audited SQLite maintenance endpoint for a managed,
writable connection in an allowed tenant and configured writable root. The
native SQLite extension owns file policy and execution; the locked `Driver`
trait remains unchanged. This adds a public maintenance action and backup
acknowledgment to the request, so the wire protocol advances to version 5.

Preview is read-only and returns the source/root identity, source size,
estimated extra space requirement, backup expectation, and a short-lived
one-use token. Apply requires that token, explicit write confirmation, and a
separate acknowledgment that a backup has been verified. It repeats root,
tenant, file identity, transaction, size and free-space checks. The server
does not infer that a backup exists from the acknowledgment. The desktop
names the connected source and asks for an exact typed confirmation; a stale
connection, action, or preview cannot authorize apply.

The worker executes only literal `VACUUM` on the connected `main` database,
with a 120-second native deadline, SQLite progress-handler cancellation, and
the existing worker close signal. The server request timeout defaults to 30
seconds and may end the HTTP request first; dropping it signals the worker's
cancel flag. A source above 1 GiB is refused. On Unix the
source filesystem must report sufficient available space for SQLite's
temporary rebuild and a safety margin; the estimate is conservative, not a
reservation, so an actual `SQLITE_FULL` remains possible. SQLite's own VACUUM
transaction provides atomic failure rollback; Sift never copies a partially
vacuumed file over the source. Cancellation, timeout and space failure return
an explicit error and leave the source connection usable when SQLite permits.
Journal modes `OFF`/`MEMORY` and `synchronous=OFF` are refused because the
rollback and durability expectation is not valid there. Preview warns that
SQLite may change implicit ROWIDs on tables without an explicit integer primary
key; callers must not treat those ROWIDs as stable identities.

This decision does not add arbitrary PRAGMA writes, checkpoint, ATTACH,
restore, delete or implicit repair. Broader real-file acceptance, including
physical ENOSPC injection and concurrent external writers, remains required.

## ADR-086 — Partition attachment requires two stable existing tables

Status: accepted. Date: 2026-09-29.

The first automatic partition migration is attachment or detachment of one
ordinary, permanent heap child to an unchanged range-partitioned parent in
the same schema. Both relations and their columns exist in both complete,
definition-bearing PostgreSQL graphs. The active graph captures the parent
identity and server-deparsed partition bound. Rendering constructs quoted
attach and detach statements from those catalog fields, and requires the paired parent and child
changes, selected together, and proves that their other details and native
state are unchanged. Attach uses `ALTER TABLE ... ATTACH PARTITION`; detach
uses `ALTER TABLE ... DETACH PARTITION`. Each is the other's rollback.

The catalog capture and renderer refuse child-local indexes, constraints,
triggers, policy, owned sequences, inheritance or nested partitions, custom
storage, extension membership, and an already populated partition tree. They
also refuse a changed parent definition, incomplete graph coverage, DDL-source
models, and mixed migration changes. Attachment may validate existing child
rows and fail atomically if they violate its bound. A partition child created
or dropped with its parent, concurrent policy or sequence edits, index and
constraint dependencies, and multi-partition ordering need separate proofs.

## ADR-087 — Desktop governed tools use the authorized registry and invoke contract

Status: accepted. Date: 2026-09-29.

The Vim desktop presents governed tools as a separate browser linked from
Extensions. Extension contributions are host projected UI actions; governed
tools are the server's context-filtered automation registry. The desktop lists
the latter with `mcp_only=false`, the selected server, and the current scoped
tenant, room, profile, connection, and document context. The server remains the
authority for which tools are available and callable. A list or result is
bounded in the desktop; a changed server or context invalidates prior replies.

Invocation uses `InvokeToolRequest` and the SDK's audited `invoke_tool` call.
The desktop previews the tool ID, classification, target context, and canonical
argument fingerprint without echoing argument values. Mutating classifications
require an exact typed confirmation before the first invoke. The first invoke
may return `ApprovalRequired`, including for reads under server policy. That
record enters an explicit revision-checked approve step, then a separate run
step with the original arguments and context. A changed input, selection,
server, or context discards the approval flow. The desktop cannot choose the
principal or bypass server authorization and approval policy. No new protocol
or Driver method is needed.

## ADR-088 — Policy migration may rename one catalog-proven policy

Status: accepted. Date: 2026-09-29.

The first automatic PostgreSQL policy migration is a rename of the sole policy
on an unchanged existing permanent heap table. Rename preserves command,
roles, permissive mode, `USING` and `WITH CHECK` expressions, and both table
RLS flags. A definition-bearing catalog graph captures the policy OID, name,
and a fingerprint of every semantic field and RLS flag, but never treats the
policy expression as executable client input. The renderer requires complete
live or server-captured graphs for the same database identity, an unchanged
table and subordinate columns, exactly one table change, the same policy OID
and body fingerprint, and only the policy name and derived security fingerprint
to differ. It emits quoted `ALTER POLICY ... RENAME TO` and the inverse as
rollback.

Catalog capture marks a policy rename candidate only for effective table
owners or superusers. Existing migration operation policy, audit, stale
revision and digest checks, and database ownership enforcement still govern
preview and apply; a snapshot's captured eligibility is not a privilege grant.
Policy create/drop, expression or role edits, RLS enable/force changes,
multiple policies, partitioned or extension-owned tables, and mixed native
changes remain unsupported until their dependency and authority boundaries
can be proved separately.

## ADR-089 — Policy create and drop replay only a catalog-proven predicate template

Status: accepted. Date: 2026-09-29.

An ordinary permanent heap table with exactly one `bigint id` column and RLS
enabled without `FORCE` may transition between no policies and one simple
policy. The supported policy is permissive `FOR SELECT TO PUBLIC` with one
catalog-deparsed `USING (id > N)` predicate, where `N` is a bounded
nonnegative integer literal; it has no `WITH CHECK`. This expression uses only
the table column, a built-in comparison, and a literal. Catalog capture marks
the empty and populated states separately and records the populated policy's
OID, name, semantic body fingerprint, and validated predicate. The renderer
requires complete definition-bearing graphs from the same database, one table
change, stable table and column identity, matching RLS state, and no other
native table state. It builds quoted `CREATE POLICY` and `DROP POLICY` SQL;
the inverse is rollback. Both directions carry privilege risk acknowledgement.

Only an effective table owner or superuser receives the replay markers.
Migration operation policy, audit, stale revision and digest checks, and
database ownership enforcement remain the final authority. A snapshot marker
does not grant permission. Other commands, roles, expressions, multiple
policies, `FORCE` or disabled RLS, partition or extension membership, table
creation, and mixed schema changes continue to refuse automatic rendering.

## ADR-090 — Independent policy and index changes may share one PostgreSQL migration

Status: accepted. Date: 2026-10-01.

A complete, definition-bearing PostgreSQL diff may combine exactly one
catalog-proven policy create, drop, or rename with exactly one native standalone
index create or drop on a different existing table. Each operation retains its
own renderer's catalog and source checks; the policy renderer additionally
verifies that the other change is an index on a distinct table. Both selected
changes are required to produce the combined plan. Forward statements follow
the dependency order supplied by the catalog diff; rollback reverses that
order. Policy changes keep privilege-risk acknowledgement.

The allowance does not apply when the index belongs to the policy table, when
the index table itself changes, or when any third change appears. Those cases
still refuse preview. No generic policy SQL or inferred cross-table dependency
is introduced; migration apply retains its existing revision, digest, audit,
and authorization checks.

---

## ADR-091 — Desktop native SQLite import uses reviewed typed CSV rows

Status: accepted. Date: 2026-10-01.

The Vim CSV review screen may send an uploaded file to SQLite's audited native
bulk target only for an existing ordinary table. The user explicitly selects
native mode and reviews a storage type for each mapped column; every field
starts as TEXT. Conversion happens in
the desktop from CSV text to typed protocol values. Empty strings remain text;
the literal `NULL` is the only null marker. Integer and real parsing reject
invalid or non-finite values, while decimal text must satisfy the native
target's canonical precision and scale. The desktop bounds row count and
serialized payload before preview; server limits and affinity checks remain
authoritative. Native mode does not infer SQLite storage class from a CSV sample.

Native preview returns a one-use token bound by the server to session,
connection, target schema and exact rows. The desktop retains the reviewed
columns, types and values and rechecks them before apply. Editing a mapping
requires another preview; changing the selected connection refuses the request.
Local cancellation discards the desktop request; the server's cancel-on-drop
guard interrupts an in-flight native worker when request drop propagates.
Because transport loss can obscure a completed commit, the UI
instructs users to inspect the target before retrying. CSV importer recipes
and streamed transfer sources remain separate until their typed storage and
resume semantics are designed.

---

## ADR-093 — SQL Server constraint-state migration uses catalog-proven state transitions

Status: accepted. Date: 2026-10-01.

The SQL Server catalog graph records the native enabled, trusted, and
replication flags for ordinary CHECK and foreign-key constraints. A migration
may change only one such constraint state on an otherwise unchanged rowstore
table. Both graphs must be complete snapshots of the same database; the
constraint name, kind, definition, table identity, and all other graph nodes
must remain stable. The driver marks a table eligible only when it has no
temporal/history role, advanced column or compression shape, or replication
constraint. The renderer validates the structured state against both graph
nodes and refuses every other native shape change.

`ALTER TABLE ... NOCHECK CONSTRAINT` disables a constraint. An enabled but
untrusted target uses `NOCHECK CONSTRAINT` followed by `CHECK CONSTRAINT`;
a trusted target uses `WITH CHECK CHECK CONSTRAINT`, which may scan and reject
existing rows. Preview labels trust restoration as a data rewrite, requires
the existing migration acknowledgment, and keeps the exact inverse SQL for
rollback where the old state is known. Native SQL Server catalog state after
execution remains the authority. Table/constraint DDL creation, temporal
versioning transitions, and replication flags remain migration-fenced.

---

## ADR-094 — Private benchmark definitions and saved comparisons

Status: accepted. Date: 2026-10-01.

Reusable benchmark definitions are distinct from immutable run snapshots. A
definition freezes SQL, engine, owner-declared parameter count, and validated serial-run limits;
it never stores bind values. The owner supplies current values and chooses a
connection before each explicitly confirmed run. Definition names and SQL live
in `SecretStore`; the metadata database keeps only owner-scoped indexes and
opaque handles. Save, list, get, update, delete, and rerun dispatch use audited
operations. A definition is not a server attestation and cannot bypass current
capability or workload confirmation checks.
The server checks the number of supplied values against the declared count on
every rerun; placeholder syntax varies by provider, so a saved count is not a
server assertion that every placeholder was enumerated. The current driver
still validates actual bind use during execution.

Definition edits use a revision compare-and-swap and a fresh secret handle. If
old-payload cleanup fails after the index commits, the edit still succeeds and
the encrypted orphan is logged without secret contents. Missing payloads remain
visible as unavailable list entries so owners can delete the index. Automatic
orphan collection is separate work.

Saved A/B comparison is a local projection of two immutable private run reports.
It checks engine, completion, and sampling configuration before showing client
elapsed median deltas. It displays both absolute and relative changes and each
run's measured sample deviation. Because saved reports do not establish common
data, parameter values, cache conditions, server version, or randomized run
order, the verdict remains inconclusive. No automatic regression or plan
equivalence claim follows from this view.

Benchmark reports may add bounded, optional execution context without changing
the version-2 sample timing contract. The server records the dedicated
connection's reported version when a bounded ping succeeds, plus the run's
preparation, connection-reuse and isolation policy. It labels connection-profile
session defaults as uninspected and cache state as unknown. Missing context in
older saved reports remains missing. A/B comparison discloses a known version or
policy mismatch; it does not turn either run into a controlled experiment.

---

## ADR-095 — Profile environment context remains separate from plan evidence

Status: accepted. Date: 2026-10-04.

An instrumented Profile response may carry optional context about its dedicated
connection: a bounded server-reported version when `ping` succeeds, the
instrumentation mode, the isolation policy, and labels for uninspected
connection-profile session settings and unknown cache state. The context does
not imply that Sift observed actual session settings, data distribution, cache
temperature, or concurrent load. Profile's native timing and per-node counters
remain separate from Benchmark's ordinary execution samples. SQLite's
estimated plan remains estimated even when the adjacent read is measured.
Older Profile responses without context continue to decode.

---

## ADR-096 — Desktop-run AI agents use server-governed Sift tools and staged proposals

Status: accepted. Date: 2026-10-05.

Sift AI chat runs an already signed-in Codex, Claude Code, or OpenCode provider
on the initiating desktop. The Sift server owns chat identity, persistence,
visibility, run records, tool dispatch, authorization, proposal state, and
audit. Provider credentials remain on the desktop; v1 has no server-owned model
API credentials or unattended agent runs. A disconnected desktop stops its
provider turn, while acknowledged chat events and proposals remain available
for a later turn. A room member continuing a public chat uses that member's
own desktop provider and Sift permissions; each turn records its initiator.

V1 agents have Read and Propose modes. Read can use bounded Sift read tools and
may run a live SELECT only through a database-enforced read-only
connection/credential path. Sift's policy flag or SQL classification alone
does not establish that path. Propose can create immutable, revision-bound
drafts but cannot apply them. A person reviews and applies a draft through the
normal Sift operation with fresh authorization and conflict checks. Agent
authorship, human approval, and execution remain distinct in audit and the
database change ledger. Execute mode is deferred.

Provider-native shell, filesystem, arbitrary network, and MCP tools are
disabled for Sift AI sessions; model-service traffic remains available. All
product access uses Sift's governed tool gateway, which enforces concrete core
operations or registered extension policy per call. An adapter is unavailable
until its restricted launch is verified. A provider prompt or approval setting
does not replace this boundary.
The first usable chat may ship with one verified provider. Codex is the first
adapter target; Claude Code and OpenCode follow after their own isolation proof.
The activity timeline separates messages, tool work, and staged proposals.
Provider-supplied progress summaries may be shown and retained under chat
retention; raw model reasoning is not required or retained as an activity log.
Desktop AI chat occupies its own right-side dock. AI and Inspector are mutually
exclusive in that slot: opening either replaces the other while preserving its
state for the next switch.
Chat context follows the active IDE tab and selected connection. Switching
context updates the pre-send preview; each turn snapshots that context when
sent and does not change mid-run. Closing a tab follows the newly active tab,
or leaves automatic SQL context empty when none remains. Room-public
publication rules still apply.

`sift.toml` chooses private chats by default or forces new chats to be public
to their Sift room. There is no per-chat visibility toggle in v1. Changing the
setting does not publish existing private chats. Room-public chats receive
Sift-sourced context and tool output only from resources publishable to every
room member, including future members. Private scratch SQL is omitted from
automatic public context until moved to a room document. Result rows are never
automatic context; they require an explicit attachment or a bounded,
authorized tool result. User-typed content in a public chat remains visible to
its room. Chat bodies and proposals persist until owner/admin deletion unless
a tenant retention policy shortens their lifetime; sanitized audit and change
ledger retention remain independent.

The protocol, storage, tool inventory, provider feasibility gates, and review
UI are specified in `docs/PLANS/ai-agent-integration.md` while they remain
active design work. The later agent harness is a separate design.

---

## ADR-097 — First AI chat uses a user-scoped Sift subprofile

Status: accepted. Date: 2026-10-05. Supersedes ADR-096's requirement for a
database-enforced read-only connection for agent SELECT.

Each AI run is attributed to its initiating Sift user. Sift creates an AI
subprofile under that user, narrows its Sift permissions for the run, and
records the user, subprofile, and run in operation audit and applicable change
ledger entries. The model cannot grant or change its own permissions. Tool
authorization is checked again at invocation, so later user, room, connection,
or policy changes can deny a call.

The first hands-on Codex test includes a private chat and governed read tools:
schema, SQL diagnostics, explain without analyze, and bounded SELECT. It uses
the signed-in CLI's default model. New local CLI turns receive bounded recent
chat history with an explicit notice when earlier turns are omitted. Forced
room-public chat support waits for server-verified publication checks.

Agent SELECT may use the initiating user's active database credential under
Sift's narrower AI subprofile, even if that database login has write grants.
Sift admits only its constrained SELECT form, bounds time, rows, bytes, and
calls, exposes cancellation and activity, and audits every invocation.
Within those limits, SELECT runs without an individual approval prompt.
This is a **Sift-restricted query**, not a database-enforced read-only
credential or a guarantee of no database side effects: SELECT can invoke
database functions with side effects. The UI, tool receipt, and audit must not
describe this path as database read-only. A future stronger database-enforced
path may be offered separately.

After human review, v1 applies a staged SQL proposal as one complete diff to
its original scratch tab or room document. Application requires current
authorization and an exact target/revision match; stale or missing targets
conflict without modifying content. Agent authorship and human application
remain distinct in audit and ledger provenance.

---

## ADR-098 — First Codex bridge uses an isolated app-server process

Status: accepted. Date: 2026-10-05.

The first Codex adapter uses the installed, signed-in CLI's app-server JSON-RPC
protocol. Each turn gets a new ephemeral thread and Sift dynamic tools. The
desktop sends a bounded context snapshot and recent saved turns, forwards
provider text and progress summaries, and returns tool requests to Sift's
server. The server chooses the connection from the saved run context,
reauthorizes each call, and emits canonical tool receipts. Only the server may
stage proposal records.

The initial verified launcher is Linux with a pnpm-installed Codex and
Bubblewrap. It starts Codex in a private mount namespace with an empty working
directory, the installed Codex package mounted read-only, a temporary Codex
home containing a protected copy of the existing CLI authentication, and no
mount of the user's workspace or other home files. The process environment is
cleared of Sift and desktop secrets. Shell, unified execution, apps, hooks,
multi-agent tools, and web search are disabled; both thread and turn use a
read-only Codex sandbox. Unsupported installations fail closed until their
isolation path has an equivalent test. The model's service connection remains
available. A manual signed-in provider test exercises dynamic tool calling
inside this launcher.

Test 1 runs private chats only. Room-public turns remain unavailable to the
desktop adapter until Sift can verify publication eligibility. The Linux
launcher is an adapter gate, not the design for future Claude Code, OpenCode,
or a broader harness.

---

## ADR-099 — Public AI SQL is materialized from committed room documents

Status: accepted. Date: 2026-10-05.

The remaining Linux Read/Propose scope uses room identity as the stable
publication label for shared SQL and staged query drafts. The server loads the
committed document actor, including durable updates after its last snapshot,
and compares the client SQL and content revision before storing a public turn.
A document ID alone never proves the accompanying bytes are publishable.
Private scratch SQL and desktop-local errors, labels, and staged counts are
omitted from public automatic context. Context for provider delivery is read
back from the server's saved run rather than reused from the desktop request.

Public proposals must target a document belonging to the chat's room. Any
currently authorized room editor may review and apply one, with original
agent authorship and the human executor retained separately. Application
checks the committed document contains the reviewed SQL before recording an
applied receipt; scratch receipts remain attributable only to their author.

Room-bound database profiles are not sufficient publication proof for a
personal connection: credentials can produce different rows and privileges.
Public database tools require a separate explicit room publication grant and
use the server-owned shared connection. Until that path is implemented, such
tools remain unavailable. Shared chat observation uses bounded incremental
replay; interrupted runs continue as new runs with the new initiator's local
provider credentials.

---

## ADR-100 — AI recovery coordinates metadata, encrypted blobs, and content keys

Status: accepted. Date: 2026-10-05.

AI bodies remain encrypted outside SQLite. New blobs use a versioned envelope
with an opaque key generation and authenticated tenant/blob identity; existing
legacy blobs remain readable until migrated. Content keys and active/pending
rotation pointers live in SecretStore. A rotation selects its replacement
key once, atomically rewrites blobs, and resumes the same generation after a
failure. Reads and writes serialize with rotation. Old keys are retired only
after all existing tenant blobs have migrated; partial rotation retains keys
needed to read either generation.

Offline state backup, under ADR-039's exclusive maintenance lock, includes a
bounded encrypted-content bundle alongside metadata and portable file secrets.
Only blobs referenced by the snapshot are exported. Recovery validates exact
blob references and decryptability before replacement. Missing blobs or keys
are errors, never successful partial chat recovery. AI bodies require portable
file-secret recovery; nonportable key backends cannot silently produce a
portable chat backup. The archive's recovery key protects the complete bundle
and source secret-key payload already used by the backup contract.

The existing restore journal gains coordinated content-directory replacement
and rollback. Both full and same-installation tenant restore preserve this
boundary. Tenant restore copies only the chosen tenant's blobs and content-key
references while preserving unrelated destination state. Restored running AI
turns become interrupted; no provider process or run lease is resumed.

Retention defaults to explicit owner/admin deletion. Tenant administrators may
shorten it, constrained by an instance maximum when configured. Expiry uses
chat activity time and skips active runs. Deletion enqueues opaque content
handles transactionally for retryable cleanup; sanitized audit and database
change history remain independent. Background maintenance reports bounded
counts and errors, never chat bodies, keys, or provider credentials.

---

## ADR-101 — Room AI database publication pins shared connection provenance

Status: accepted. Date: 2026-10-06.

Room-public chat visibility does not publish a personal connection. The room
owner explicitly grants AI access to the room's bound shared-credential profile.
Schema/estimated-plan publication and row publication are distinct choices;
rows are disabled by default. The grant disclosure states that current and
future authorized room members can read resulting public chat content. No
personal credential mode, broker binding, unrelated profile, or client-supplied
connection identifier can satisfy this publication boundary.

Each immutable grant has an opaque ID and pins the room, binder, profile,
provider/configuration identity, credential-handle identity, and policy revision.
Vault-backed sources also pin the current opaque credential version. Raw
credentials are never read to compute this identity, copied into metadata,
or shown in disclosures. Rebinding the room, changing configuration/credentials
or policy, losing issuer authority, or revoking the grant invalidates future
tools. A replacement grant gets a new ID. Server-owned connection caches are
closed when a grant changes, so a replacement cannot silently reuse an older
credential session. Per-turn public context records the selected grant ID and
server-derived connection labels/dialect; personal context remains excluded.

Public database tools run through the room's server-owned shared connection,
using the same Driver-based supervision, SQL admission, quotas, bounds, audit,
timeouts, and cancellation as other Sift operations. Authorization and grant
identity are checked before dispatch and again before returning data. Schema
and estimated explain require schema publication; bounded SELECT additionally
requires row publication. Analyze remains a separately human-authorized action.
Saved public messages retain their original publication provenance; revocation
blocks future reads from the database rather than rewriting published history.

Explicit result attachments require equivalent room-owned/shared provenance;
a private result grid cannot be relabeled public by a client. Public history
and plan tools read only resources already published into that room. Source IDs
alone are insufficient evidence of publication. Grant creation/replacement,
revocation, observation, and governed use remain Operation variants and audited.

Recovery preserves publication provenance but imports grants as revoked. Room
owners must review the restored connection scope before republishing database
access; imported provider turns never resume. Existing room/profile/vault
permissions continue to govern who may initiate database tools. A publication
grant exposes resulting chat content to the room; it does not confer database
execution or vault-use permissions on its members.

---

## ADR-102 — AI database proposals use typed previews and durable human apply claims

Status: accepted. Date: 2026-10-06.

Agent tools may stage a bounded typed row EditSet or desired CatalogGraph;
no agent-facing tool applies them. Unsupported native SQL definitions remain
blocked by the ordinary conservative migration renderer. Row updates/deletes require stable keys and
original-value expectations. Migration rendering uses Sift's existing typed
catalog diff and migration renderer, never arbitrary model-authored DDL execution.
Proposal payloads, generated previews and receipts remain in encrypted AI content.
A normal catalog source reference can resolve an authorized AI migration proposal
without copying its desired schema into unencrypted catalog snapshot metadata.

Each proposal binds its initiating turn, target profile, engine, live source
revision and immutable publication identity for public rooms. A private proposal
is owner-readable; a public proposal is room-readable only with the source's
publication proof. Shared row proposals require separately published row access.
The reviewer uses their own authorized managed connection on the same source;
a public proposal never grants use of the initiator's private session. Current
room, profile, vault and operation policy admission applies to both review and
apply. Admission is rechecked after queued waits and immediately before each
mutation; transactional work rolls back on revocation. Changed publication,
source, schema, proposal or preview requires review.

Review produces exact parameterized row statements or the normal migration plan
and risk acknowledgements. Apply explicitly confirms its reviewed digest and
target database; production-labelled sources require explicit confirmation.
The row plan regenerated during apply must exactly match the reviewed statements
before a transaction starts. Migration risk acknowledgement and existing driver
supervision stay in force.
AI authorship, proposal/run correlation, human approver and actual executor are
recorded in the sanitized database change ledger and Operation audit.

A durable per-proposal apply claim is committed before database work. It binds
the reviewer, reviewed content and client request identity. A successful response
is retained encrypted and replayed for an identical retry. Concurrent, altered or
previously consumed apply requests never dispatch a second database mutation.
Interrupted/incomplete claims and uncertain driver outcomes become
outcome_unknown; failed/unknown applications
require fresh review and a new proposal rather than an automatic retry. This
covers a database commit followed by failed receipt persistence without claiming
an atomic transaction across SQLite metadata and the external database.

Cleanup, retention, backup inventory and tenant recovery include proposal review
and receipt bodies. Recovery interrupts uncompleted apply claims and never
restarts database work or provider processes. The dock presents target, exact
preview, risks and durable application outcome; SQL-buffer apply stays distinct
from applying row/schema changes to a database.

Managed persistent SQLite file profiles have a catalog identity shared across
connections with the same validated public file configuration. This lets a
human review a room-owned file source through their own managed connection;
fresh catalog content still guards schema revisions. Transient SQLite
databases retain per-handle identities and cannot be published to room AI.
Native Unix SQLite catalog object IDs pin device/inode identity at connection
open, preserving object identity across handles to the same file generation.
Older catalog snapshots with a handle-specific database identity need recapture
before live comparisons through another connection.

## ADR-103 — Isolated provider adapters and per-chat provider/model choice

Status: accepted. Date: 2026-10-06.

Linux adapters run one ephemeral CLI process per Sift turn. Bubblewrap exposes
system runtime files, the installed executable, and an isolated temporary HOME
containing only a protected copy of provider authentication plus Sift-owned
configuration. It never exposes project/user configuration, hooks, workspace
files, SIFT environment secrets, or the Sift server bearer token. Native coding,
file, network, task, and arbitrary MCP tools are disabled; unsupported native
provider events fail the run closed. Installation support is explicit. Codex native and conventional npm/pnpm
installs resolve to the native executable plus the bundled
codex-code-mode-host helper; modern dynamic tools silently disappear without
that helper, so an incomplete install fails before a turn starts. Only those
two executables are mounted, never the whole package manager. Claude/OpenCode
currently require their native Linux CLI binaries. Runtime mounts stay bounded. Native CLIs may refresh OAuth in the private auth copy. Sift
preserves a complete refresh back to the native credential file only if the
original bytes remain current and only existing OAuth rotation fields changed;
new accounts, API keys, URLs and unrelated fields are rejected. Replacement is
atomic and private, never SQLite. Concurrent native sign-in takes precedence;
Sift never copies settings back. An authentication refresh failure asks for CLI
sign-in. OpenCode's most recently selected model is read as a preference when
no explicit configuration model exists; its session/history state is excluded.

Codex retains its dynamic app-server tools and explicitly loads the bounded
Sift tool set eagerly. Claude Code and OpenCode receive
only a parent-owned loopback Streamable HTTP MCP endpoint. It uses a per-run
unguessable capability, rejects browser origins, bounds request bodies and
parallel calls, negotiates MCP versions, and shuts down with the provider turn.
The provider sees governed reads and staging tools, never apply operations.
The bridge maps immutable RPC calls to Sift receipts without exposing a live
session selector or server credentials. Server leases, tenant/profile/room
permissions, publication, quotas, idempotency, and cancellation stay authoritative.

Provider/model choice belongs to each chat and is recorded with each run. An
empty model means the installed CLI's safely extracted preference or default;
only allowlisted model preferences are read from host config, never plugins or
MCP settings. Unsupported/unavailable providers do not silently switch provider.
Most recent run preferences restore the chat choice; pending UI choices and
active turns are not overwritten by observers. Actual provider/model identity,
context truncation, and work steps are visible. All provider selection and turn
operations continue through the existing audited AI boundary.

Claude uses print stream-json, partial messages, restricted mode, tools empty,
strict explicit MCP configuration, empty setting sources, disabled skills,
no session persistence, and an explicit Sift tool allowlist. --bare is excluded
because the installed CLI disables OAuth in that mode. OpenCode uses run --pure
--format json with an explicit Sift-only primary agent, isolated config/auth,
all built-in tools denied, and explicit Sift MCP tool permission. Never attach
to a user's existing daemon/session.

Validation gates: signed-in isolated roundtrips for all three installed CLIs
(all passed; fresh Claude Code 2.1.291 probe verified 2026-10-06);
MCP unauthorized/origin/version/unknown/native tool rejection; exact selected
model transport; server governed tool and staging behavior; cancellation;
required format, strict workspace Clippy, and workspace tests.

Primary references verified 2026-10-06:
https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle
https://modelcontextprotocol.io/specification/2025-11-25/basic/transports
https://code.claude.com/docs/en/cli-reference
https://opencode.ai/docs/mcp-servers/
Installed CLI help additionally confirms Claude --restricted, --tools,
--strict-mcp-config, and OAuth exclusion for --bare; OpenCode --pure and
--agent are available.

## ADR-104 — Bounded source-resolved AI context and attachments

Status: accepted. Date: 2026-10-06.

Turn snapshots are immutable and encrypted. The desktop sends typed source
references and inclusion choices, never authoritative inline resource content.
The server resolves those references under the human's current authorization,
bounds materialization, and rechecks permission before storing/delivering them.
A model receives only the resulting server-owned snapshot.

Default automatic context remains active SQL, connection/dialect/environment,
selection and current-statement offsets, transaction/read-only state, and the
active query's error/diagnostics. Execution provenance supplies result-tab SQL
and connection; switching the current editor never rewrites result provenance.
No bind values or rows are automatic context. UI inclusion toggles disclose
exact fields before send and persist per chat, with request-selected fields
snapshotted per turn. Public scratch SQL stays omitted.

Explicit resources: retained immutable private query rows; immutable room
results; query-history entries; saved/analyzed semantic plans. Both HTTP and desktop WebSocket executions retain a bounded immutable
AI excerpt with server-owned execution provenance. Retention never consumes or
advances a user cursor; limits, eviction and expiry are explicit. Rows are bounded
selected row ordinals and explicit columns from a server-retained source with
an exact schema digest, not client-uploaded arbitrary values. Preview includes
source, exact columns/rows, bounded content digest, truncation, and private vs
current/future-room-member visibility. Expired/mismatched sources fail clearly,
never rerun a query just to reconstruct an attachment. Private result provenance
must match the turn's tenant/profile/connection. Room-result provenance must
match the current chat room and normal room-read authorization.

Server-owned source labels are publication proof: room documents, room query
history, and shared room results already have room-public labels. Private
resources cannot enter public context solely through caller read permission.
Captured plans/history lacking a room label require a human exact-body preview
and explicit publish acknowledgement, with source ownership and the relevant
owner-reviewed database publication grant rechecked. Analyzed/native plan
content can contain literal parameters; public publication therefore needs
the separate row-data grant. The acknowledgement binds a fresh exact digest;
materialized snapshots get a durable room-public label when the turn commits.
Private result excerpts remain private; public result attachments require an
existing canonical shared room result. Historical resources stay labeled as
historical; a profile name alone never proves they describe the current DB.
Already shared room history uses current room-read permission, independently
of live database publication, profile selection, or private credential access.
Private and other-room history retain their source-specific guards.
No automatic upgrades of private sources.

New governed tools: bounded own/private or same-room/current-profile history,
DDL for exact current catalog object/revision, and saved plan summaries/content.
Estimated explain remains non-analyzing. Creating analyzed plans continues only
through ordinary human-authorized execution. Models cannot create analyzed
plans, choose another live connection, publish resources, or apply proposals.
Tool results are byte-bounded; source freshness/publication is checked both
before dispatch and after queue/driver work. Errors never leak raw credentials
or audit SQL/values. Resource previews and attachments are audited Operations.

One shared input builder bounds history/context/attachments before any CLI
receives them and makes omissions/truncation visible in durable events and UI.
Attachment chips support remove, clear-on-send, explain this error/plan, and
attach selected result rows without switching editor focus. Vim is the only
interaction mode; source changes invalidate previews rather than retargeting.

Meaningful gates: real SQLite selected rows/no bind auto-inclusion, immutable
result provenance, wrong session/tenant/profile/schema/digest/expired source,
room and future-member publication, late permission revocation, private source
omission/explicit release, plan analyze denial, object revision freshness,
history scope/privacy, byte/row/column bounds and stable per-turn context;
required format/strict workspace Clippy/workspace tests.

Implemented backend limits: immutable private excerpts expire ten minutes after
execution, retain at most 256 rows per set and eight sets, charge at most
512 KiB per result and 16 MiB globally, and cap eight results per session.
Oversized headers leave an ordinal-preserving hole. Retention charges JSON
container overhead as well as serialized bytes; deeply nested JSON is excluded
before serialization or cloning. Both HTTP and WebSocket
executions expose a server-generated result UUID independently of driver cursor
reuse. Closing a session/connection removes its excerpts. Private row attachment
permission checks use the original retained SQL against the current execution
policy, including allowed schemas; an oversized retained SQL excerpt cannot
substitute for that authorization proof. Room selection scans
at most 512 pages / 16 MiB and never materializes an entire shared result.
Selections contain 1–100 distinct source ordinals and 1–64 explicit columns;
typed row vectors preserve duplicate column names and display ordering.

Previews expire within ten minutes (earlier when their retained result expires),
bind actor/chat/target/publication and exact
source/content digest, and are limited to 16 per actor / 128 globally / 8 MiB
serialized globally. At most four sources materialize concurrently, with a
clear busy response. Four reviewed attachments may enter one turn, each with
64 KiB of content and a 1 MiB total encrypted-context limit. Whole selected
rows are trimmed visibly; an entirely empty trimmed selection fails. Inline
client attachment bodies are discarded. Send re-materializes the reviewed
source, checks its exact digest, then rechecks all source permissions with
fresh HTTP authentication. Metadata independently validates source sharing
labels and explicit private-to-room publication attribution. Public saved-plan
tools return only published snapshots in that turn, never the private plan
store. Desktop previews/chips and typed row/history/plan selection are implemented,
including explicit canonical shared-room cell selection. Source coordinates retain
server schema digests through display sorting/column reordering and window offsets;
staged edits cannot masquerade as original rows. Per-execution SQL and actual
session/connection survive editor/toolbar changes. Preview cancellation ignores
late responses, send rejects expired/retargeted sources, and prompt/attachments
survive start failures. Successful starts clear attachments without erasing a new
prompt typed meanwhile. Vim commands expose the same review flow; SQL draft copies
open separately when the original buffer is no longer applicable. Inclusion
preferences now persist with the last sent turn. Automatic diagnostics are
bounded, UTF-8 safe and revision-bound; statement/selection ranges use exact
source offsets. Transaction/read-only hints require the actual source connection
to match, otherwise they remain unknown. These advisory hints grant no authority.
Public turns omit private workspace diagnostics and state. The server enforces
exclusions independently and validates source ranges before storing context.

Implementation can graduate governed history/DDL/saved-plan reads first, then
resource previews and desktop attachments. Public saved-plan tools see only
explicitly published attachment snapshots; private reads are restricted to the
initiator, tenant and current managed profile. Every stage remains honest about
missing sources or publication, and analyzed-plan creation remains human only.

## ADR-105 — Shared bounded AI run harness and cooperative cancellation

**Status:** Accepted. Shared input/output bounds, deadlines, RPC identities,
read/proposal quotas, active read/proposal-preparation cancellation and scoped
asynchronous desktop presentation are implemented. Format, strict workspace
Clippy and workspace tests pass. Fresh isolated Codex, Claude Code and OpenCode Sift-tool
roundtrips also pass; Claude Code 2.1.291 was verified with current native sign-in
on 2026-10-06.

**Context.** Isolated adapters share governed tools but duplicate prompt assembly,
use different output bounds, and allow provider progress to refresh local waits.
Stopping a desktop process does not cancel an already accepted server read.
Proposal staging also needs to consume the same run quota as governed reads.

**Decision.** A provider-independent desktop harness builds one bounded input
from the server-authorized current context and the newest complete saved turns.
Current prompt and reviewed attachments are never silently shortened: oversized
current input fails before launching. Input is capped at 1 MiB; history contributes
at most 128 KiB and six complete turns. Omitted history is disclosed both to the
model and through a durable progress event. No private context is recovered from
native provider sessions. UTF-8 boundaries are respected throughout.

The run's absolute deadline derives from server start time and the current
instance policy, capped at one hour. Startup, provider streaming, Sift calls and
process exit share this deadline; progress cannot extend it. Provider frames are
bounded to 1 MiB before allocation, visible model text to 64 KiB per turn, and
reasoning summaries to 12 KiB. Exceeding a bound ends the run with a safe reason.
Visible token fragments publish immediately but durable deltas batch at 1 KiB;
completed messages use 2 KiB chunks so JSON escaping cannot exceed event limits.
Empty fragments spend no event receipts. Mirrored final text is validated without
double charging streamed text, while distinct completed messages share a total cap.
RPC retries preserve their invocation identity and argument digest. Repeated IDs
with different arguments fail. Read calls and SQL/database proposal staging share
the server's tool-call quota; duplicate staging replays its existing proposal
without spending quota again. Remote tools will use that same quota.

Each server read registers an in-memory cancellation guard before its final
running-state check. Stop first settles the durable run, then signals registered
work. A stop before registration is caught by the post-registration status check;
a stop after registration reaches the guard. Cancellation entries are bounded
and removed when their work settles. Driver work remains spawned and bounded;
SELECT cancellation uses its actual cursor and existing discard-on-cancel rules,
including the interval before a cursor is available. Cancellation never changes
the Driver trait. Revoked actor access or publication prevents response delivery
and still settles reserved tool receipts through a trusted internal path.

Only a genuine final provider completion may mark a run completed. Intermediate
OpenCode tool steps do not qualify. Failure, timeout, cancellation, transport loss,
and disconnection preserve their terminal status; continuation starts a new run.
The UI retains acknowledged proposals and history, preserves newly typed prompts,
and scopes asynchronous results to their originating instance, tenant and view UUID.
Captured command senders retain immutable origin tags. Switching chats or instances
invalidates presentation callbacks without moving human apply receipts to another
server. Instance/tenant changes clear source reviews, loaded private history and
provider/context choices while preserving an unsent question. A late Stop from a
previous view cannot stop a new view's turn.

Turn preparation runs outside the desktop command loop. Chat setup/loading and
review I/O have 30-second bounds; a shared-result/attachment read queue caps 16
pending requests. Once turn creation is sent, Stop awaits its lease (up to 30
seconds) and settles that created run before provider launch. Creation/terminal
transport failures preserve the prompt. ADR-107 settles a lost creation response
using the exact original request identity, with a durable marker against late
creation; if cancellation also fails, the UI instructs the user to reopen the chat
for durable recovery. Server expiry remains the fallback for uncertain transport. Native execution and saved-context fetch share the absolute run
deadline, and terminal receipt I/O has a ten-second bound. Recent history is
limited in the database query (eight continuation candidates, sixteen visible
turns); the current authorized turn is retrieved directly by chat and run UUID.
The original actor and lease can close a run after losing chat membership, even
when AI is disabled, without reading any content. Successful completion still
requires current access; retries of the same terminal status return the existing
receipt without appending another event.

Database draft catalog preparation registers the same cancellation guard as reads,
uses the remaining absolute deadline, and rechecks current authentication plus
source/publication identity before staging. Human apply keeps its ordinary audited
transaction supervision and is never canceled merely because an AI view changed.

**Validation.** Exercise byte/UTF-8/history limits, oversized newline-free frames,
absolute deadline under progress, repeated RPC IDs, shared staging quotas,
intermediate versus final completion, cancellation before/after cursor creation,
permission revocation while a read is blocked, and instance switches while events
are in flight. Use existing local fixtures and workspace gates; add no CI workflow
or standalone smoke script.

## ADR-106 — Explicitly registered external MCP through Sift governance

**Status:** Accepted and implemented for Linux Read/Propose (2026-10-06).

The registry and transport foundation is implemented: encrypted source definitions,
opaque SecretStore credentials, draft activation bound to reviewed schemas and
credential scope, revision-bound private credential resolution, durable cleanup,
and coordinated recovery that resets registrations to drafts. Turns now carry at
most four explicitly selected immutable source pins, rechecked inside turn creation.
Independent room grants require source-owner and room-owner review, identify allowed
aliases, and never grant private vault access to room readers. Revocation during
credential resolution withholds the result; previously accepted turn snapshots keep
ordinary room visibility. Local fixtures cover
modern and legacy HTTP, offline schema validation, parameter headers, changed
inventories, and exclusion of remote writes. Discovery and completed reads both
recheck authority before a remote call or result delivery. Audited HTTP/SDK routes
now expose discovery, explicit activation, closure and independent room grants.
Setup persistence is supervised independently of disconnected HTTP waiters and
tracked during shutdown. Selected-source reads share the canonical run quota,
reject reused call IDs, stop on cancellation or the absolute deadline, and persist
accepted bounded results as encrypted server receipts after fresh source checks.
Desktop source selection and operator management now require explicit source pins,
reviewed classifications, and independent room sharing acknowledgements. Credential
refresh has Keep/Replace/Clear choices, never prefills a stored secret, refuses to
reuse credentials at an edited endpoint, and resets the source to an unapproved
draft. Safe grant headers let an authorized publisher explicitly replace stale
grants after refresh or recovery. Fixed provider inventory/read tools are exposed
only for selected sources, share the canonical quota, and persist encrypted
receipts; inventory makes no remote call and returns complete schemas by alias.
Local proposal adapters stage only Sift's typed SQL/row/schema contracts and never
call the remote tool. Their immutable reviewed source/alias/schema provenance is
encrypted with the proposal, participates in replay checks, and is disclosed on
review cards. Fresh source authorization is checked in the staging transaction;
accepted drafts subsequently use the ordinary current database/catalog/human-review
boundary. Staging consumes one proposal quota entry, without a second read receipt.
The combined workspace suite, strict Clippy and formatting checks pass. Live
isolated Codex, Claude Code and OpenCode tool roundtrips pass (fresh Claude Code
2.1.291 probe on 2026-10-06); deterministic adapter, isolation and common
tool-contract tests pass. Execute/unattended runs and other
operating-system provider isolation remain outside this scope.

**Context.** Native provider homes deliberately exclude unrelated MCP servers.
External data access must preserve Sift actor, tenant, source, publication, quota,
and cancellation boundaries without granting a generic remote mutation agent.

**Decision.** Register each actual operator-supplied endpoint, protocol
revision, bounded tool inventory/schema digests, operation classifications and
opaque credential handle in Sift. Never import native CLI MCP configuration,
ambient proxy/authentication state, hooks, filesystem roots or native tools.
Remote annotations and descriptions are data, never authorization. A read tool
requires explicit registration as Read and operator-reviewed credential scope.
Use existing tenant/vault resource authorization before and after remote I/O.
Credentials remain in SecretStore, never SQLite, model input, event bodies or logs.

Support pinned modern Streamable HTTP (2026-07-28) and explicitly pinned legacy
Streamable HTTP (2025-11-25) as distinct transports. Modern requests are isolated
POST calls with matching protocol/method/name metadata and headers, no initialize
or persistent session lifecycle. Legacy uses bounded initialize/initialized,
negotiated protocol/session headers and cleanup. Accept bounded JSON or request
SSE; unsupported sampling, elicitation, roots or server-initiated requests fail
clearly. No redirects, URL credentials/query secrets, ambient proxies or external
schema-reference fetching. Verify TLS; HTTP is limited to registered loopback
fixtures/development. Actual endpoint URLs come from registration or listeners,
never guessed provider endpoints. Closing a request follows protocol cancellation.

Per-run tool discovery exposes only currently accessible registered tools with
fixed validated aliases and bounded schemas. Invocation rechecks exact registration
revision, endpoint, tool schema/classification, tenant/vault access and credential
version before/after I/O. Remote content passes the same result byte limits,
encrypted persistence, sanitized receipts, shared call quotas, absolute deadline
and cooperative cancellation as Sift reads. Response JSON-RPC identity must match
and duplicate IDs cannot change arguments. Fresh revocation withholds all bytes
while trusted receipt settlement remains possible.

Remote writes are never invoked. Explicit supported proposal adapters expose a
local Sift draft schema and stage query text, typed row edits, or desired schema
catalog through existing Sift review/apply paths. Their registration pins the
remote intent/tool schema and adapter type; it cannot introduce arbitrary code,
remote execute/apply endpoints or runtime shell hooks. Unsupported write tools
remain unavailable with a clear explanation. Human apply retains ordinary Sift
connection permissions, source/catalog revisions, production confirmation and
one-use receipts.

Private use is the default. Public remote reads require their own owner-reviewed
room source grant pinning endpoint/registration/schema/credential identity.
Existing database row publication never authorizes an external source. Public
inventory, tool descriptions, arguments and results require that independent grant;
revocation stops future calls, while accepted published snapshots remain visible
under normal room access. Context disclosure identifies accessible external sources
and distinguishes them from automatic SQL/workspace inclusion choices.

**Validation.** Local HTTP fixtures supply actual bound endpoint URLs
and fake SecretStore credentials. Cover modern/legacy request envelopes, JSON and
SSE limits/cancellation, protocol/identity mismatch, redirects, unregistered writes,
unsupported server requests, stale registrations/schemas/credential rotation,
tenant and vault revocation during reads, independent room publication, duplicate
calls, local proposal adapters and plaintext-credential absence. No standalone
smoke scripts or new CI workflows.

References reviewed:
https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http
https://modelcontextprotocol.io/specification/2025-11-25/basic/transports

## ADR-107 — Exact cancellation after a lost AI startup response

**Status:** Accepted and implemented (2026-10-06).

A lost turn-creation response must not leave an invisible active turn or cause a
blind retry with changed attachments, publications, or source pins. The desktop
cancels the exact original chat/actor/client-request/desktop identity. This returns
only an optional run ID and settlement acknowledgement, never prompt, context or
lease bytes. It preserves the prompt for an explicit fresh send after review.

Cancellation records a bounded durable marker before acknowledging an unknown
request. Creation checks the marker before allocating content and again in its
SQLite transaction. Creation and cancellation therefore serialize: either the
existing matching run is stopped, or the marker prevents late creation. Repeated
cancellation does not duplicate terminal events; terminal runs retain their
status. An existing matching request can be closed while AI is off or after room
access is revoked. Unknown requests require current chat access. Nil identifiers
and mismatched actors/desktops are denied. Markers cascade with chat deletion and
are included in tenant-selective recovery; they contain no content or secrets.

Final startup persistence runs in an owned, admitted task tracked through shutdown,
so disconnected HTTP waiters cannot drop encrypted writes or rollback cleanup.
The desktop bounds its cancellation request and reports uncertainty if that
transport also fails. It never starts a provider without a running lease.

**Validation.** Regression tests pass for cancellation before/after creation,
duplicate settlement, wrong actor/desktop, revoked membership and AI-off closure,
bounded marker admission, late creation during encrypted writes, lost-response
desktop cleanup without retry, chat deletion and tenant recovery isolation.
The full workspace suite, strict Clippy and formatting checks pass. No new smoke
script or CI workflow was added.


## ADR-108 — Governed performance snapshots and explicit CLI workloads

**Status:** Accepted. Implemented performance snapshot tools and SDK-backed CLI
workflows; acceptance evidence and command inventory live in `docs/sift-mcp.md`.

AI Read/Propose tools may list, inspect and compare the initiating user's saved
benchmark measurements in the turn's tenant. These are user-saved snapshots,
not server attestations, and have no managed-profile provenance. Tools must
label that scope, check ownership on every read and deny room-public turns until
an explicit benchmark publication contract exists. Reports never contain bind
values. Output is bounded; comparison recomputes client-elapsed statistics from
successful non-warmup samples rather than trusting saved summary fields. Different
engines/configurations or incomplete runs cannot produce a numeric comparison;
SQL differences, unknown parameter values and unverified environment/cache/data
conditions remain disclosed. Profile and Benchmark populations stay separate.

AI tools cannot launch measured workloads or manufacture `workload_confirmed`.
Humans can run Benchmark/Profile through CLI commands with an explicit workload
confirmation flag and a bounded JSON request file. The commands reuse SDK APIs,
managed connections, server authorization, resource budgets, cancellation and
existing audited Operations. They close connections/sessions after completion
or failure. No Driver trait change and no second authorization path are needed.

CLI convenience scope: governed tool list/call (including approval handoff),
managed-profile query execution, benchmark/profile execution, private benchmark
list/get/compare. Explicit server URL and protected token file remain required;
SQL/requests come from bounded files, credentials never appear as CLI options.
Only a human CLI action may confirm execution; provider-native CLI access stays
unavailable inside AI isolation. JSON output supports scripts and preserves
approval-required outcomes without implicitly granting approvals.


## ADR-109 — Desktop model capabilities and formatted AI threads

Accepted, 2026-10-07. The desktop discovers Codex models and supported reasoning
levels through its isolated app-server `model/list` transport, including paginated
catalogs. Claude uses provider aliases and CLI effort controls; OpenCode uses its
local model catalog and model variants. Unknown/custom model identifiers remain
an explicit override, without inventing supported reasoning levels. Catalogs are
capabilities, not a guarantee of account entitlement.

The selected reasoning effort is optional turn context, retained by the existing
encrypted context store and startup request digest. Provider adapters translate it
into native controls; it grants no additional Sift or OS permissions. Discovery
is bounded, does not start inference, and leaves the isolated turn launcher intact.

The AI panel width is a saved local preference with a drag handle and no fixed
maximum other than available workspace space. Transcript rendering uses CommonMark
with tables and task lists, code-fence copy controls, and Sift editor text runs for
SQL highlighting. HTML stays text; rendering never executes content.

Prompt editing is opt-in multiline native text with IME support, wrapping and a
bounded scrollable composer. Enter submits; Shift+Enter inserts a newline. SQL
blocks remain fully expanded. Draft review identifies its original query and
source, offers a complete before/after overlay, and shares apply's exact target
and SQL-revision checks. Narrow footer widths shorten labels without changing the
normal layout. Interrupted-turn retry preserves the failed prompt and never
overwrites a newly composed message.

Thread rename/delete actions live in the thread picker. Renames use an audited
server mutation, check the expected thread revision and owner/admin permission,
and replace only the encrypted title handle in SQLite. The old handle enters
durable cleanup. Deletion uses the existing audited API after an inline
confirmation. Mutations rotate the desktop view scope so earlier observer
snapshots cannot undo their presentation.

Reference: [Codex app-server models and turn controls](https://learn.chatgpt.com/docs/app-server).

---

## ADR-110 — Locally proven GitHub owner linking and desktop invitations

**Context.** Hosted OAuth and administrator-managed GitHub admission already
exist, but personal local-device mode never performs GitHub authorization and
Account hides local owner/user management. Tenant invitations are API-only.
Users need a discoverable owner identity and team admission workflow without
turning the first network visitor into an administrator or requiring internet
access for local work.

**Decision.** Personal loopback instances optionally configure an instance-owned
OAuth App public client ID for GitHub device authorization. Desktop distributions
use the publisher's public registration compiled into the bundled server;
explicit instance registrations override that default. End users sign in from
Account without configuring OAuth. The local OS owner
initiates setup from Account using the trusted-loopback authentication context.
Network, SSH, team deployment, API tokens and ordinary authenticated sessions
cannot claim local ownership. The publisher enables device flow in Sift's
GitHub registration; operator overrides require device flow too. Only the public client ID is bundled; no client secret or
Sift identity broker is bundled. Sift's public registration is the source default;
build-time and per-instance overrides support independently registered forks.

Attempts are bounded, expiring, daemon-local, principal-bound and one-use.
GitHub device codes remain in server memory; native callers receive a human
verification code and redacted random handoff. Polling enforces provider
intervals and slow-down responses. Cancelling/dismissing Account or changing
instances abandons its attempt; a newer attempt supersedes an older one. Provider
network calls have deadlines and redirects are refused. GitHub access tokens
are dropped after profile verification and never enter SQLite, settings, logs
or operation bodies.

Completion atomically links the immutable GitHub subject to the existing active
owner. Existing/manifest-declared subjects cannot be replaced, disabled
identities are not revived, and subjects owned elsewhere are rejected. The
original personal local bootstrap principal can become the first instance
administrator only while no active administrator already exists. Principal and
tenant IDs and local OS recovery remain intact. Hosted deployment is still an
explicit reviewed configuration change with closed registration and existing
per-instance authorization-code OAuth; local linking alone enables no network
access. Hosted admins admit GitHub logins through Manage users, with optional
explicit linking to an existing Sift principal and no implicit email linking.

Account exposes existing tenant invitations with workspace/role selection,
optional principal targeting, seven-day expiry, immediate copy of one-use
credentials, acceptance and unused-token revocation. Role grants and rooms stay
separate. Revocation binds both invitation ID and the authorized tenant in its
SQL mutation. Acceptance invalidates principal auth caches so membership appears
immediately. Native inputs, issued tokens and authorization codes are transient;
delayed replies carry instance/principal/generation scope and are ignored after
dismissal or identity change. Vim shortcuts and Enter/Tab cover the workflow.
Owner setup/link/cancel and invitation actions use typed Operations and durable
credential-free audits.

**Consequences.** Local Sift remains offline-capable. Operators own registration
and hosting policy. Owner verification reuses stable Sift principals rather than
creating a second account or switching local transport into a hosted session.
Users can complete team admission from the desktop; advanced recovery remains
operator tooling. Live GitHub registration/consent cannot be supplied by tests,
which instead exercise bounded provider fixtures and native state transitions.

---

## ADR-111 — Version-pinned PostgreSQL extension updates

**Context.** The workbench can install/drop extensions, but updating by an
implicit server default can run a different package version from the one the
user inspected.

**Decision.** Add a typed UpdateExtension action with explicit name and target
version through existing PreviewPostgresObject and ApplyPostgresObject audited
operations. Preview admits only an installed extension, a different target,
and a catalog-proven update path. Quote the target as an identifier. Include
installed version, extension OID, owner, namespace and update-path state in the
preview fingerprint; apply rereads it and rejects changes. Native PostgreSQL
enforces final ownership and script privileges. The Vim Monitor `u` action
captures the displayed default version and opens the existing SQL confirmation.
No Driver signature or existing wire shape changes; the action is additive.

**Consequences.** Users review exact SQL with an extension-script/locking
warning. No automatic updates or package downloads occur. Preview is a state
check rather than a database lock; concurrent changes and package-file changes
may still cause a PostgreSQL apply error. Dependency impact previews and
partition attachment remain separate milestones.

Reference: https://www.postgresql.org/docs/current/sql-alterextension.html

---

## ADR-112 — Operator-scoped PostgreSQL deadlock log projection

**Context.** PostgreSQL exposes current waits and aggregate deadlock counts,
but historical event details require an external log source. Allowing callers
to choose arbitrary server paths or returning native DETAIL would expose SQL
and unrelated databases.

**Decision.** Operators register at most 32 server-local JSON log files pinned
to managed tenant/profile IDs, original host/port and database. Existing audited
ListDeadlocks reads require current managed access, an unrestricted schema
policy and upstream superuser/pg_read_all_stats rights. HTTP supplies no path.
Read the final 4 MiB on at most two blocking workers with five-second waiter
timeouts; retain worker admission until IO finishes. Recheck authority after IO.

Project only SQLSTATE 40P01 for the exact database, at most 50 events and 16
participants per event. Only native numeric resource forms and known lock modes
leave the parser; SQL, arbitrary DETAIL/context and paths do not. Unsupported
wait descriptions mark participant coverage incomplete. UTC/numeric-offset
timestamps are required. Summaries and raw logs never enter SQLite.

**Consequences.** The existing SDK and Vim Monitor History view serve both
providers without a Driver signature or wire-shape change. Operators own log
collection, permissions, rotation and source-to-profile mapping. Missing sources
fail explicitly. This delivers bounded retained-file history, not a durable
archive, remote log transport or unrestricted log browsing.

Reference: https://www.postgresql.org/docs/17/runtime-config-logging.html#RUNTIME-CONFIG-LOGGING-JSONLOG

---

## ADR-113 — Refuse unmodeled SQL Server values before decoding

**Context.** Vendored Tiberius has panic paths for sql_variant and CLR UDT type
metadata. A placeholder display value cannot provide lossless decoding/binding,
and reusing a response after stopping partway through its metadata is unsafe.

**Decision.** The codec emits a dedicated UnsupportedType error before reading
unmodeled type metadata. The Sift driver maps it to UnsupportedResultShape and
discards the physical connection after query or CSV bulk failure. It marks the
logical handle invalid before reporting the stream error; later requests must
reconnect. Bulk failure skips an on-wire rollback when decoding stopped, letting
disconnect roll back the active transaction. Other connections remain usable.

**Consequences.** Unsupported native results fail explicitly without decoder
panics, placeholder rows or reuse of an unread TDS stream. This hardens the
existing refusal boundary; it does not claim sql_variant/UDT support. Base-type
identity, collation/precision, binary representation and symmetric parameter
encoding require a separate lossless contract before decoding can graduate.
No Driver signature or public wire shape changes.

---

## ADR-114 — Typed PostgreSQL partition attachment with reviewed impact

**Context.** Partition inspection and detach exist, but attachment requires
native bound syntax and can scan tables, attach/create indexes and clone
triggers. Caller-supplied SQL fragments would bypass a typed review contract.

**Decision.** Extend the existing audited PostgreSQL object preview/apply action
with attachment of an existing ordinary child to a partitioned parent. Bounds
are typed default/range/list/hash values; bounded literal strings become escaped
PostgreSQL E literals, with explicit NULL and MINVALUE/MAXVALUE alternatives.
Identifiers remain quoted data. Require current ownership of both tables,
matching native columns, an unattached child and a compatible parent strategy.

Fingerprint exact SQL, native catalog identity/ownership, columns/defaults,
constraints, indexes, triggers, parent attachments, full dependency descriptions
and role/search-path/date/time settings. Show at most 50 dependency descriptions
with explicit truncation; the fingerprint retains the undisplayed state. Apply
rereads the fingerprint before executing through the supervised query path.
Audit retains action identities and bound modes while removing bound literals.
Attachment audit errors use a generic message; native details stay in the request
response because coercion failures may echo supplied values.
The Vim Monitor `a` form leads to exact SQL, impact, warnings and confirmation.

**Consequences.** PostgreSQL performs final row, overlap and privilege checks.
Preview holds no locks and does not freeze data; native errors remain visible.
The warning names scans/locks and index/trigger consequences. Foreign and
partitioned child tables, creation, automatic dependency rewrites and general
cross-object management remain separate work. No Driver signature changes;
wire additions remain pure serde and existing previews omit absent impact.

Reference: https://www.postgresql.org/docs/17/sql-altertable.html

---

## ADR-115 — Native Windows artifact provenance and per-user development packaging

**Context.** Native Windows packaging needs executable metadata, all local
server helpers, corresponding source and artifact evidence. Linux compilation
cannot establish Windows install/uninstall behavior or signing trust.

**Decision.** Pin Rustup to the compiler selected by the locked Nix flake.
Generate desktop VERSIONINFO and icon resources in Cargo OUT_DIR from package
metadata. Build the native x64 MSVC desktop and five sibling helpers from a
clean checkout. Portable bundles include source, notices and a manifest of
compiler/profile/source identity, payload hashes and Authenticode status;
complete ZIPs receive SHA-256 receipts without overwriting previous artifacts.

Build the per-user development installer from a verified staging copy of the
fixed bundle payload. Use a separate development installation identity,
current-user registration/shortcuts and an explicit runtime prerequisite check.
Preserve unrelated files and application runtime data across uninstall. Record
installer, recipe, icon and compiler identities with the resulting hash.

**Consequences.** Native Windows workspace checks and actual artifact
install/uninstall acceptance support this development workflow. They do not
establish a signed stable release, a clean-machine/prerequisite matrix, macOS
coverage or native agent isolation. Publisher and signing credentials remain
operator inputs; unsigned status is retained as evidence. The desktop selects
runtime locations independently of the development installer identity.

See [native design and evidence](PLANS/windows-native-distribution.md).

---

## ADR-116 — Explicit personal-desktop metadata setup and typed activation readiness

**Context.** The desktop starts an owned daemon for singleton/runtime identity,
but automatic metadata setup admitted only in-process launchers. A fresh native
Windows installation consequently stopped at metadata V0. Activation also
probed a stale unversioned readiness path that its mock repeated.

**Decision.** The bundled personal desktop passes a launcher-only
`--personal-desktop` flag, consumed before server arguments are forwarded.
Authorize automatic metadata setup for this flag only with personal deployment,
daemon mode, loopback transport and a loopback bind. Preserve the existing
personal in-process policy and skip setup when metadata is disabled. Continue
using the offline migration command's exclusive maintenance lock, backups and
migration safety classification. Configured instances retain their applied
instance workflow; ordinary daemons retain explicit migration.

Activation uses the SDK's typed readiness and a fresh handshake under its
absolute deadline. Exercise the production HTTP router in the regression test
instead of duplicating endpoint names and fabricated responses.

**Consequences.** Native acceptance proves fresh schema setup, readiness,
protocol 5 negotiation and restart identity/generation behavior on Windows.
This adds no Driver signature or public wire shape. Automatic startup does not
bypass migration safety rules or grant remote administrative authority.

See [startup design and evidence](PLANS/windows-native-distribution.md).

---

## ADR-117 — Windows launcher ownership through process teardown

**Context.** Native acceptance reproduced an owned daemon surviving forced
launcher termination and continuing to answer readiness. Desktop lease cleanup
terminates its owned launcher, so ordinary Rust destructors cannot guarantee
server and helper cleanup.

**Decision.** Before spawning any migration helper or server on Windows, create
an unnamed job with KILL_ON_JOB_CLOSE and a non-inheritable handle, then assign
the launcher itself. Children inherit job membership at creation; no breakaway
flag is enabled. Fail before spawning if ownership cannot be established.
After successful assignment, retain the sole job handle until OS process
teardown rather than closing it during Rust stack cleanup, which would also
terminate the launcher and replace its intended exit status.

**Consequences.** Normal exit and forced launcher termination remove remaining
owned descendants. Native probes verify ordinary exit-code preservation and
forced cleanup; real launcher/server acceptance verifies server disappearance
and readiness closure. A Windows lifecycle regression captures the server
handle before termination to avoid PID reuse and cleans the fixture on failure.
This is process ownership, not filesystem/network agent isolation. Unix/macOS
supervision, signing and package trust remain separate contracts.

See [design and native evidence](PLANS/windows-launcher-lifetime.md).

---

## ADR-118 — Proven SQLite table renames preserve data and an exact inverse

**Context.** SQLite snapshots and additive migrations exist, but a rename must
preserve native declarations, file authority and dependent objects. Matching
only structural columns would discard native proof or permit implicit rewrites.

**Decision.** Admit explicitly accepted table-name mappings only between
native-proven simple ordinary main tables with identical declarations and
catalog facets. Ignore only the exact quoted table-name prefix in canonical
CREATE proof when matching names. Require unfiltered, untruncated main/temp
captures with only admitted tables/columns and containment edges; dependency
objects and unsupported native shapes remain refused. Filtered graph reads
retain required ancestors and valid edge/column references through shared
projection, while recording requested kinds so migration cannot mistake a
projection for complete native evidence.

Render quoted native ALTER TABLE RENAME and its exact inverse through existing
audited migration preview, validation and apply. Both forward and rollback
groups remain transactional even when a caller disables preferred transactions.
Existing live-revision, confirmation, read-only, timeout/cancellation and root
checks remain authoritative. No table rebuild or file access expansion occurs.

**Consequences.** Real-file HTTP acceptance proves rowid/value preservation,
validation rollback, confirmed apply, stale-index rejection and refusal of
view-bearing or kind-filtered captures. Formatting, strict workspace Clippy and
workspace tests pass. Column rename/drop/alter, dependency rewrites, rebuilds
and general designer migrations remain separate milestones.

See [design and acceptance](PLANS/sqlite-native-renames.md).

---

## ADR-119 — Monitor administration reviews bind source and request identity

**Context.** The database workbench has guarded typed administration APIs, but
missing desktop forms leave routine role/grant/ownership changes inaccessible.
A queued preview or apply must not follow a different runtime context, and a
late reply must not replace a newer review even on the same connection.

**Decision.** Add Vim Monitor forms for existing PostgreSQL NOLOGIN-role,
schema USAGE/CREATE and database/schema ownership actions, and SQL Server
role creation, membership and schema SELECT actions. Fields produce bounded
names and typed requests; no SQL fragments or credentials enter draft state.
Exact SQL and warnings precede explicit production confirmation through existing
audited preview/apply operations.

Each mutation command carries the selected instance, tenant, profile, query
session/connection identity and a unique request UUID. Before SDK access, the
desktop executor compares that identity with its actual opened context; the
administrative call uses the separate metadata connection belonging to that
same context. Replies retain source and request UUID. The UI accepts only the
current request on the exact current source. Apply checks its accepted preview
source before allocating a fresh request UUID. Missing identity fails closed;
view/connection changes invalidate outstanding review identity.

**Consequences.** Source changes and late replies cannot retarget or replace a
reviewed mutation. Tab moves between scalar fields, Escape returns command
focus then cancels, m cycles labelled action modes, and p previews. PostgreSQL
administration uses x for production confirmation; SQL Server retains its
explicit reviewed Enter/button confirmation. Login credentials, broader role
attributes, object/inherited privileges, effective permission matrices and full
RLS policy editing remain separate work. No Driver signature, server wire
shape or authorization/audit boundary changes.

---

## ADR-120 — PostgreSQL row security changes are typed, owner checked and reviewed

**Context.** Policy catalog browsing and rename leave policy definitions and
row security controls inaccessible from the Vim Monitor. Truncated display
labels cannot serve as editable definitions, and native diagnostics may echo
private condition literals into durable audit records.

**Decision.** Extend the existing audited PostgreSQL object actions with typed
CREATE/ALTER/DROP POLICY and individual ENABLE/DISABLE/FORCE/NO FORCE ROW LEVEL
SECURITY actions. Creation explicitly selects command, permissive/restrictive
mode and up to 32 roles; PUBLIC and a named role are distinct wire values.
ALTER leaves omitted roles/conditions unchanged. Native command/mode changes
require separately reviewed drop/create actions. Expressions are bounded to
2048 bytes and parsed as one PostgreSQL expression with bounded recursion,
no bind placeholders or trailing SQL; unsupported syntax fails explicitly.
Policy editing requires native standard_conforming_strings=on to preserve
canonical string-literal interpretation.
Identifiers are separately validated and quoted. Native PostgreSQL remains
authoritative for expression types, object privileges and policy semantics.

Require table ownership at preview and fresh apply. Fingerprints include
session interpretation, table identity/attributes, all policies, RLS flags and
selected role identities. Changes invalidate review; preview holds no locks.
Use existing supervised deadline/cancellation, execution authorization and
explicit production confirmation. No Driver signatures change.

The source/request-bound Monitor form supports all actions even when a table
has no policies. Scalar schema/table names remain exact. Existing catalog
roles and expressions are display-only, including untruncated values; ALTER
starts with blank optional fields and never rewrites a lossy display value.
Tab moves fields, Escape returns command focus/cancels, m cycles actions,
c selects create command, v changes create mode, p previews and x confirms
reviewed SQL. Existing source checks prevent retargeting and stale replies.

Audit projection removes condition bodies from typed operations before
in-memory, JSONL or durable audit sinks. RLS failures use a generic durable
message; the requesting client retains its native diagnostic.

**Consequences.** Native PostgreSQL acceptance covers each command, both modes,
role-specific reads and checked writes, restrictive policy composition, owner
forcing, enabled/no-policy default deny, owner refusal, stale review, statement
containment and correlated durable-audit privacy. UI acceptance covers omitted
fields, truncated catalog labels, exact-source review and explicit confirmation.
Arbitrary PostgreSQL expression grammar, atomic multi-action replacement and
effective-permission analysis remain separate work.

See [design and acceptance](PLANS/provider-and-operations-completion.md#postgresql-rls-editor-design).

---

## ADR-121 — SQL Server ownership reviews disclose native permission loss

**Context.** The security workbench lists schema owners but cannot change them
or the current database owner. SQL Server schema transfer removes schema ACLs
and permissions on contained securables with inherited ownership. A review
that shows only an owner name misses that loss and can become stale when
permissions or target principal identity change.

**Decision.** Add a typed database/schema ownership action to the audited SQL
Server preview/apply operation. Schema targets use existing database users,
roles or application roles; database targets use existing SQL/Windows-user
server logins and are confined to the current user database. Protected native
schemas and system databases are refused. External/unresolved server-owner
families require separate provider evidence. Show the current database owner
when it resolves, otherwise an explicit unknown value.

Check native TAKE OWNERSHIP and target authority (user/login IMPERSONATE, role
membership/ALTER or application-role ALTER) at preview and fresh apply. Complete
schema impact review additionally requires database VIEW DEFINITION/security
metadata. For non-dbo/non-sysadmin contexts, effective metadata DENYs on
database/schema/object/type/XML-collection/principal scopes refuse review,
including DENYs inherited through role tokens. A database grant alone cannot
prove complete visibility. Pass
QUOTENAME values into native permission-name lookups, while catalog predicates
receive exact bound names. Render each target as a bracket-escaped identifier.

Count schema permission rows and permissions on inherited-owner objects,
user-defined types and XML schema collections. Explain native loss, ownership
chains and EXECUTE AS OWNER behavior before production confirmation. Preserve
explicitly owned objects; never silently regrant permissions. Fingerprint source
session/connection, native database/schema and selected principal identity,
current authority, contained object ownership/modify state, all relevant ACLs,
and database trust/chaining settings. Any relevant difference requires review.
Preview holds no locks; native SQL Server remains final authority.

Use existing source/request-bound Vim administration forms and supervised,
policy-checked execution. Database mode labels its target as a server login;
schema mode labels it as a database principal. Preserve exact scalar names.
No Driver signature or secret-storage change occurs.

**Consequences.** Real HTTP/SDK acceptance on SQL Server 2022 proves schema and
database transfer, permission counts/loss across all three contained families,
explicit-owner preservation, user/role/application-role/certificate/key targets, minimum transfer authority plus
visibility and metadata-DENY refusal, stale ACL and recreated-login refusal, production confirmation,
protected/wrong-database refusal and bracket-bearing identifier containment.
Fixture resources are isolated and cleaned on returned errors and panics.
Vim acceptance covers both typed ownership modes and reviewed confirmation.

See [design and native acceptance](PLANS/provider-and-operations-completion.md#sql-server-ownership-editor-design).

---

## ADR-122 — SQL Server role and grant reviews bind native identity

**Context.** Existence/permission flags do not identify a principal. Native
acceptance reproduced a stale membership review applying to a newly recreated
user with the same name. USE and EXECUTE AS can also change native execution
context without changing Sift session/connection IDs. A single TOP permission
row cannot represent independent grantors.

**Decision.** Retain the typed role-creation, add/drop membership and schema
SELECT grant/revoke actions, but extend their state with a native SHA-256
fingerprint. Include actual server/database identity, database owner/collation,
actor/login identity, and the selected principal IDs, SIDs, kinds, ownership,
authentication and create/modify metadata. Membership reviews include the exact
membership; schema-grant reviews include schema ownership and all selected
metadata-visible SELECT ACL rows with their grantors' identities. The outer
fingerprint also binds Sift session/connection, exact rendered SQL and state.
Re-read before confirmed execution; native SQL Server remains final authority.
No credentials or password hashes are selected, and preview holds no locks.

Catalog predicates receive exact bound names. Native permission functions
receive QUOTENAME values, so narrow role editors work with bracket-bearing
names. Keep explicit authority, existence and no-op checks. Missing/invalid
native fingerprint state fails closed. Refuse multiple-grantor schema ACLs
rather than selecting an arbitrary row; DENY/unsupported states require their
own review. No implicit CASCADE, AS-grantor mutation or effective-permission
claim is introduced.

**Consequences.** One real HTTP/SDK fixture proves refusal after recreated
roles, members and grantees, changed schema owner, EXECUTE AS and USE; fresh
creation, membership and schema-grant mutations still succeed. Narrow role
ALTER authority and quoted names work, and multiple native grantors are
explicitly refused. Existing ownership/permission-loss acceptance remains in
the same isolated fixture, which cleans up on errors and panics. Driver
signatures, wire types, audit and authorization boundaries are unchanged.

See [design and acceptance](PLANS/provider-and-operations-completion.md#sql-server-rolegrant-review-identity-design).

---

## ADR-123 — SQLite column renames carry a proven table-definition change

**Context.** SQLite native column rename preserves table content but also
changes the stored CREATE declaration. A schema diff therefore contains both
column renames and a companion table-proof alteration. Ignoring the table
change or copying a target declaration would risk accepting unrelated structure
changes or rebuilding data. Column metadata also duplicates the column name;
that duplicate previously prevented structurally identical rename mappings.

**Decision.** Normalize only the duplicate column metadata name for rename
compatibility, requiring it to agree with the node name. Preserve all other
shape checks. Extend the existing SQLite migration backend to explicitly
accepted column renames within an unchanged ordinary main table under ADR-118's
full, unfiltered native-proof and dependency refusal boundary.

Verify both complete canonical CREATE declarations and retain the same table
identity and metadata. Column-only selections verify their chosen pairs without
claiming other target edits. When the companion table alteration is selected,
compare every ordered column: all changed names must be accounted for and all
other structure must match before consuming it. Incomplete
whole-table selections, selected-column structural edits, moves and unproven
captures fail closed.
Native ALTER TABLE RENAME COLUMN carries it; no second table DDL is emitted.
No string replacement, copied target SQL or table rebuild occurs.

Native-proven same-table SQLite column mappings reserve identity before
implicit path matching, allowing explicit chains and swaps. Schedule destinations
against the source namespace, refusing unselected prerequisites. Break selected
cycles with deterministic collision-free intermediate names visible in the
reviewed SQL. Attribute each step to its logical change and render separately
quoted statements with inverse steps in reverse order. Both directions retain
mandatory SQLite transactions, live-revision
checks, validation rollback, file/root authority, read-only enforcement and
supervised execution. No Driver signature or protocol change occurs.

**Consequences.** Real-file HTTP acceptance proves multi-column quoted-name
changes, rollback validation, unchanged rowids/runtime values/storage classes,
confirmed apply/inverse, selective column rollback validation and incomplete
whole-table-selection refusal, chain/swap apply and inverse, missing-overlap
prerequisite refusal, read-only refusal and
stale-index protection. Separate view, index and kind-filter captures are
refused. Shared comparison tests preserve structural-change refusal and reject
inconsistent node/metadata names. This is native migration backend support;
standalone SQLite designer controls, dependency rewrites, drop/alter/rebuild
and broader provider workflows remain separate work.

See [design and acceptance](PLANS/sqlite-native-renames.md#column-rename-design-2026-11-04).

---

## ADR-124 — Derive SQLite designer rename targets from live proof

**Context.** The native rename backend needs both source and target declaration
proofs. The existing designer already submits typed RenameObject intent, but
its complete-graph gate rejected SQLite and its generic rename clone left
duplicate column names and stored table proof unchanged.

**Decision.** Keep the existing wire intent and audited PreviewMigration /
ApplyMigration operations. For SQLite, accept only table/column rename intents
in the private graph mutation path. Check the original canonical declaration
against ordered simple-column facets and native proof, then derive its target
proof from the structured renamed metadata. Quote identifiers independently;
never replace SQL substrings or execute preview DDL. Update duplicate column
metadata names for all providers. Refuse SQLite ASCII-insensitive sibling
collisions and embedded NUL names. The renderer still validates both complete,
unfiltered main/TEMP captures and rejects unsupported/dependency-bearing
shapes before any plan is stored. Partial SQLite parser coverage grants no
additional mutation capability.

The existing Vim column designer offers SQLite name edits with type and
nullability displayed read-only; add/reorder actions remain unavailable. Load
table definitions with the same default graph options used by preview, because
projected/definition-enriched captures have distinct revisions. Freeze
instance/tenant/profile and runtime session/connection on preview and explicit apply; each request
has a fresh identity. Check the actual executor context before SDK access and
ignore superseded/out-of-context replies. Draft changes invalidate review.
No Driver signature or protocol type changes are needed.

**Consequences.** This connects existing-column rename controls and the typed
table-rename API to native plans. Real-file HTTP tests prove preview without
mutation, quoted/case-only apply and exact inverse, rowid/value/storage-class
preservation, and native stale/read-only/dependency/shape refusals. Focused
designer tests prove typed reviewed dispatch, source/session and stale-reply
checks, unsupported edit guards and duplicate-apply prevention.
It does not complete general SQLite designer
DDL, dependency rewrites, constraints, drop/alter/rebuild or platform acceptance.

See [design and acceptance](PLANS/sqlite-native-renames.md#designer-rename-design-2026-11-04).

---

## ADR-125 — Reviewed SQLite restore uses an atomic destination transaction

**Context.** Managed SQLite roots already support create, online backup, VACUUM
and integrity checks, but cannot restore a backup into a connected database.
Replacing a live database by renaming files would split open handles and WAL
state, while a partial page copy could otherwise destroy the destination.

**Decision.** Add a typed Restore action to the existing audited file
maintenance operation without changing Driver or native extension signatures.
The selected backup must be an existing authorized SQLite file in the same
unambiguous root, distinct from the target and its sidecars. Require a writable
managed/root/tenant context with no transaction, a fresh one-use review token,
explicit write confirmation and a verified pre-restore backup acknowledgment.
Refuse all file maintenance on schema-restricted profiles; it cannot honor a
selected-schema boundary. The Vim review names target and backup, warns that
all connected main contents are replaced, and requires the exact phrase
`RESTORE root/target FROM backup`.

Validate path/file authority, original target generation, sizes and main/WAL
identities. Refuse symlinks, hardlinks, same-inode/protected sources, non-SQLite
and corrupt archives, files/WAL above 1 GiB, active transactions and stale
reviews. Verify the source through bounded quick_check with trusted schema
disabled, then retain that read-only snapshot through the copy. Recheck the
precondition after opening it. Preview holds no lock and does not replace data.

Use SQLite's backup API into the existing main file through a private destination
connection/cache. The destination handle is never passed to managed interrupt
or query APIs while copying. Copy 128 pages per step with cancellation, close,
busy-retry and elapsed-time bounds; the destination transaction commits only on
completion and unfinished copies roll back when the backup handle drops.
No filesystem replacement, journal-mode conversion or repair is introduced.
SQLite schema reads bypass shared caching; verify fresh schema observation
through other open handles and refresh desktop schema/definition state after
reported success. A timeout alone does not prove whether restore committed.

**Consequences.** Native HTTP acceptance covers WAL source preservation,
replacement of old main schema/data, rowid/value/storage-class preservation,
integrity, fresh cross-handle schema/revision reads, confirmations, stale
target/backup, replay, read-only/transaction/policy and path/file refusals.
Driver acceptance cancels after the first page batch into a WAL destination
and proves rollback and subsequent successful restore. Vim acceptance covers
explicit preview/apply, exact target/backup phrase and acknowledgment.
Cross-root restore, deletion, external recovery orchestration and native
Windows/macOS evidence remain separate work.

See [design and acceptance](PLANS/sqlite-restore.md).

---

## ADR-126 — Saved measurement annotations stay private and revisioned

**Context.** Private saved benchmark measurements have immutable reports and
no note/tag editing. Storing commentary in SQLite or replacing an entire
caller-supplied report would violate payload privacy or measurement integrity.
Concurrent annotation editing also introduces a race with permanent deletion.

**Decision.** Add notes/tags and an annotation revision to the encrypted saved
payload. Legacy snapshots default to revision zero with empty commentary. V055
adds only a nonnegative revision integer to the existing SQLite index. Accept
only expected revision and bounded notes/tags in an authenticated owner-private
update operation: 8 KiB notes, at most 16 trimmed/deduplicated/sorted 64-byte
tags, and explicit control-character rules. Names, run IDs, save timestamps and
all measurement fields stay unchanged. Replace only the annotation object in
the original JSON, preserving unknown observations from newer payloads.

Write a fresh SecretStore handle and atomically swap it under tenant, owner,
current membership, prior-handle and revision checks. Retain 4 MiB snapshot /
64 MiB owner limits. Clean unsuccessful replacement handles. Post-commit old
payload cleanup failure is logged without content and cannot turn a committed
edit into an apparent failure. Deletion compares its selected handle so it
cannot erase a concurrent replacement or orphan the newly indexed payload.
The file backend serializes durable mutations before publishing them (ADR-127).
Existing backup/tenant recovery uses the same namespace and current indexed
handle; annotations need no separate plaintext index or secret inventory.

Audit only tenant/run/revision identifiers. Keep annotation bytes out of
SQLite, operation arguments, diagnostics and presentation storage. Private
desktop summaries include tags; AI and standalone MCP measurement tools retain
explicit projections that omit notes/tags. Portable measurement artifacts and
comparison inputs remain report-only. Annotations do not attest provenance or
grant execution authority.

The disconnected Vim saved-run browser edits full notes/tags, saves explicitly
and binds drafts/replies to instance, tenant, run and request identity. Conflicts
retain drafts. Refresh loads the latest stored revision without replacing the
draft, allowing review before another explicit save. Late replies are ignored;
new edits made during a pending save remain unsaved drafts. Opening another
record cannot retarget an edit, and active drafts cannot arm permanent deletion.

**Consequences.** This completes private note/tag editing, not workspace/Git
context capture, retention-policy editing, annotation history or sharing. The
full performance workbench remains partial. Driver signatures and execution
behavior are unchanged.

See [design and acceptance](PLANS/benchmark-run-annotations.md).
---

## ADR-127 — File secret mutations publish only after serialized persistence

**Context.** FileSecretStore rewrites one encrypted map per mutation. Concurrent
writers previously cloned independently and shared a temporary filename, risking
lost durable entries and rename failures. Failed persistence also left changes
visible in memory. Revisioned benchmark annotations exercise these overlapping
writes during replacement and cleanup.

**Decision.** Serialize put, delete and coordinated AI tenant-key replacement
through one writer gate per backend instance. Acquire and retain the gate inside
the blocking worker, including when its asynchronous waiter is canceled. Clone
the last committed map, apply the mutation privately, encrypt and persist using
the existing file fsync, atomic rename and parent-directory flush, then publish
the resulting map. Failed persistence leaves the committed in-memory map intact.
Readers use a separate short-lived map lock and remain available during disk I/O.

**Consequences.** Concurrent mutations preserve all accepted entries across
reopen. Cancellation of a waiting caller does not cancel a running disk write
or release its serialization gate. This does not add cross-process coordination;
existing exclusive offline recovery and single-writer lifecycle rules still
apply. Acceptance covers concurrent puts/deletes, cancellation while a worker
owns the gate, reads during a blocked writer, failed put/delete rollback and
successful subsequent persistence.

See [design and acceptance](PLANS/benchmark-run-annotations.md#file-backend-prerequisite).
---

## ADR-128 — Performance captures bounded settings before workload setup

**Context.** Performance reports declare connection/preparation/transaction
policies but cannot compare actual database session settings. Inspecting the
editor session would describe a different connection, while mixing setup reads
with sample clocks would contaminate workload measurements.

**Decision.** Benchmark and Profile observe nine selected settings on their
owned disposable connection before workload-specific transactions, timeout
changes or instrumentation. Retain a typed optional phase and ordered scalar
map alongside the existing policy declarations. PostgreSQL uses current_setting,
SQL Server selects only its own session row, and SQLite reads table-valued
PRAGMAs with explicit call syntax and no assignments. SQLite authorizes only
argument-free diagnostic reads; assignments stay denied. Namespace collisions
cannot supply settings from user tables. PostgreSQL functions are schema-qualified
and unknown settings remain unavailable. Omit identity, paths, arbitrary configuration and
credentials. Legacy reports decode with observations absent.

Run capture in a spawned task under cancellation, query-resource reservation,
a five-second cap and the remaining benchmark deadline. Accept only one bounded
result row and a matching fixed projection, then drain to Done. Extra rows or
result sets, incomplete streams and driver errors stop the workload and discard
the owned connection. Cancellation/deadline during Benchmark setup yields an
incomplete report without fabricated samples. A completed unrecognized scalar
projection is explicitly unavailable. No Driver or extension signature changes.

Observed values are bounded to 128 bytes; saved user snapshots also validate
observation shape and bounds. They remain unattested submissions. Performance
and saved-run views display the selected settings with their before-setup
phase. Comparisons disclose missing/different observations and limited coverage;
matching values do not establish equivalent data, complete settings, later
transaction overrides, preparation internals, cache temperature or server load.
Inspection itself can warm metadata caches and is outside reported sample and
instrumentation clocks. The read belongs to the existing audited operation.

**Consequences.** This supplies actual selected session-setting evidence while
preserving the separation between observed defaults and applied workload policy.
Full environment equivalence and remaining performance features stay open.
Native Windows/macOS acceptance remains deferred.

See [design and acceptance](PLANS/performance-session-settings.md).
---

## ADR-129 — Agent-role inspection uses native authority on an owned session

**Context.** Native SQL Server 2022 acceptance found that SQLAgentUserRole can
select sysjobs_view and execute job-history procedures, but cannot select the
underlying sysjobs/sysjobhistory tables. The existing joined SELECT incorrectly
reported PermissionRequired for legitimate Agent-role readers. Session ROWCOUNT
also cannot be used to bound procedure results on the editor connection.

**Decision.** Read at most 101 jobs through sysjobs_view, retaining Sift's
explicit owner-SID filter for every non-sysadmin, including Reader/Operator
roles with broader native visibility. Sysadmins retain bounded direct whole-job
history reads. Restricted readers use documented sp_help_jobhistory SUMMARY,
step_id=0, newest first, ROWCOUNT 1 and the local server filter. Remote-target
history may remain unavailable. Capture that row in a native table variable and
project only outcome, local date/time and duration; no history messages, job
steps, commands, descriptions or operator metadata enter the driver result.
Never-run jobs retain absent history fields. The response remains capped at
100 jobs with a truncation sentinel.

Both Agent and configuration reads use a supervised task owning a disposable
connection cloned from the authorized source and charged to its connection quota.
Configuration reads also reset private ROWCOUNT so editor limits cannot silently
shorten the catalog.
Preserve typed session-variable setup. Replay only explicitly supported literal
session SET statements; reject arbitrary/unknown startup batches before opening
the private connection. Do not drop startup authority or repeat write-bearing
startup SQL during an inspection read. Its fixed batch uses msdb, local
table variables and private session settings without persistent writes. The
existing read operation permits this trusted batch on read-only profiles;
schema-restricted profiles remain refused. Check source authority before
opening, before reading and before publication. Close the owned connection on
success/error; the supervisor survives a lost HTTP waiter and a drop guard
covers unwind. Existing execution supervision, timeouts and resource limits
bound the complete batch. Native roles/procedures enforce their own rights;
Sift never requests a new broad catalog grant or exposes job execution.

Close observation is bounded separately from native close ownership. Both normal
completion and guard destruction start one owned close task; its five-second
observer borrows the task handle instead of dropping the native future. An expired
observer does not abort cleanup or release the connection quota/tunnel guard.
The guard disarms after scheduling, so completion and unwind cannot start duplicate
native closes. Paused-time regression covers both paths through a wedged driver
close and proves quota releases once only after native completion.

**Consequences.** All three Agent roles can inspect owned local job history;
broader native roles do not broaden Sift's response. Editor ROWCOUNT is preserved.
SQL Server configuration inspection remains a numeric cast-only read, with
native denial mapped to PermissionRequired. The tested 2022 build permits a
public configuration read despite newer documentation naming a server-state
permission, so no guessed version gate replaces native authority. Disposable
role, history, truncation, settings denial/recovery, redaction and audit
acceptance records the tested boundary. Agent execution and broader distributed
job management remain outside this read-only feature.

See [design and acceptance](PLANS/sqlserver-inspection-acceptance.md).


## ADR-130 — CSV recipe projection precedes import and binds original resume input

**Context.** Rename-only recipes cannot drop private fields, reorder destinations
or explicitly copy a source. Rewriting before import must not change row offsets,
leak dropped values into quarantine, expand memory without a bound, or allow a
changed original upload to resume merely because projected bytes match.

**Decision.** CSV upload imports at format version `2` accept a nonempty ordered
`column_projection` array of strict `{source,target}` records. Only selected
fields survive; repeated sources copy values to distinct targets. Source lookup
must resolve exactly one header. Validate shape, scope and limits at recipe
create/update/validate and execution; refuse simultaneous legacy
`column_mappings`. Version 1 retains rename-only behavior. Reserve both keys for
CSV imports and reject unsupported bundled format versions.

Rewrite on a blocking worker, preserving record order and field text while
validating every original record width. Bound input and output to 64 MiB and
check output as emitted, with at most 256 projection outputs. Existing inference,
provider type checks, dry run, apply and quarantine consume the same mapped CSV.
Only projected values reach importer samples or quarantine. Destination names
are distinct, trimmed, nonempty, NUL-free and at most 128 bytes; source names
are exact UTF-8 strings up to 4096 bytes.

For durable projections, append the original upload SHA-256 to the existing
recipe/workspace/revision/actor authority scope. The checkpoint still binds
mapped bytes and target shape; a changed dropped field produces the existing
HTTP 409 mismatch. Preserve legacy rename/no-selection checkpoint identity.
Existing transfer/import Operations supply authorization and auditing; no new
wire types or Driver signatures are needed. Expose options and guidance through
the multiline Vim recipe JSON editor. Expressions, other-format projection and
dedicated visual selection remain separate work.

**Validation.** Unit coverage checks ordering/copying, malformed/ambiguous input,
legacy renaming, strict option scope and bounded expansion. Native integration
covers PostgreSQL, SQL Server and SQLite: dry run without writes, quoted Unicode,
NULL/empty values, apply, quarantine privacy, idempotent checkpoint replay and
refusal after changing only a dropped source field.


## ADR-131 — Performance progress observes active jobs without retaining rows

**Context.** Dedicated Benchmark/Profile supervisors already keep driver work
away from the UI, but users cannot distinguish setup, execution, delay or
cleanup while the original HTTP request is pending. Streaming result rows or
cloning growing sample arrays for progress would duplicate sensitive data and
allow an event backlog to undermine responsiveness.

**Decision.** Each existing active registration owns a fixed-size row-free
progress snapshot and bounded frozen SQL used only for current-policy checks.
Expose exact session/source/run/mode reads as `ReadPerformanceProgress`, with
session-owner fencing and current progress, original workload and Execute
permissions. Recheck active SQL against current policy. Do not call the driver
to read progress; do not put SQL, parameters, rows or diagnostic text in its
response or audit. Use no-store and remove state with the existing registration.
Null is an inactive observation, never a terminal outcome. The original request
remains authoritative for results, cancellation errors and cleanup warnings.

Benchmark snapshots count completed warm-up and measured attempts separately,
including unsuccessful attempts, and retain the last outcome. Setup, warm-up,
measurement, inter-query delay and cleanup are distinct stages. Profile exposes
setup, instrumentation and cleanup without a fabricated database completion
percentage. A monotonic server elapsed clock includes setup/cleanup and stays
separate from every sample/instrumentation clock. Existing deadlines, quotas,
read restrictions and Driver signatures stay authoritative.

The desktop polls beside the result future at most twice per second with one
two-second bounded progress request in flight. Each update requires a UI
acknowledgement before another read, bounding the queue to one event per run.
Completion drops the polling future; unsupported/transient reads never replace
the workload result. Bind updates to item, instance, profile, per-connection generation,
exact session/connection, mode and run UUID. Results reject stale
or post-completion snapshots without changing keyboard focus. Definition reruns
reuse the same runner/poller and their existing strict review context. The Vim
panel presents stages/counts/elapsed time; completed samples retain their
existing virtualization and validated 10,100-attempt maximum.


**Validation.** Real managed SQLite HTTP/SDK acceptance covers counters, held
Profile cancellation, current permissions and frozen-SQL schema policy,
wrong identities, no-store, audit privacy and cleanup. The mock PostgreSQL
cancellation regression observes driver calls around progress reads. GPUI tests
cover stale/wrong-mode/finished updates, source changes, background switching,
numeric-ID reuse and Vim input. A desktop polling test holds acknowledgements,
proves no second read/event can accumulate, verifies the 500 ms interval and
completes the result while progress remains unacknowledged. Formatting, strict
workspace Clippy and workspace tests pass. No new native percentage or renderer
frame-budget claim is introduced.


## ADR-132 — SQL Server database users use native identity-bound review

**Context.** The security workbench already reviews roles, membership, schema
permissions and ownership, but cannot create or repair SQL-login database-user
mappings, change a user's default schema, or remove a user. Mapping by names
alone could apply a reviewed access change to a recreated login/user or a
changed database/actor. Remapping must preserve user identity and existing
rights rather than silently recreate the principal.

**Decision.** Extend existing Preview/ApplySqlServerSecurity Operations with
CreateDatabaseUser, RemapDatabaseUser, SetUserDefaultSchema and DropDatabaseUser.
Support existing SQL-authenticated logins and instance-authenticated SQL users;
WITHOUT LOGIN SQL users can change default schema or be dropped, but cannot be
remapped. Built-ins, roles, contained-password, Windows/external, certificate
and asymmetric-key identity families remain explicit refusals for these actions.
Credentials, login creation/reset and login enablement remain separate work.

Create explicitly uses default schema dbo; schema changes select an existing
visible schema. Remap emits both LOGIN and NAME so native auto-rename cannot
change the reviewed name. Refuse duplicate SID mappings, owner-login mappings,
no-ops, and remapping/dropping the current actor. Disabled logins are disclosed
and remain disabled. SQL login-mapped user names reject backslash before driver
work, consistent with the native identity family.

Native review checks action-specific effective rights: CREATE USER/ALTER ANY
USER, database CONTROL for remap, ALTER-on-user/self for schema, and ALTER ANY
USER for drop. Create/remap/drop require database security metadata without
metadata DENYs; narrow schema/self editing uses selected visible identities.
Bind database ID/creation/owner/collation, actor/login, principal IDs/SIDs and
creation/modification state, selected login enablement, schema/owner identity,
direct memberships, explicit permissions involving the user and known owned
securables into the hash. Re-preview and compare before supervised dispatch,
with existing Execute/Apply policy and confirmation. Review holds no lock;
native SQL Server validates final authority and concurrent DDL.

Drop refuses known owned securables and uses native DROP USER with no cascade,
object deletion or ownership transfer. The server login survives. Native SQL
Server validates remaining ownership families and permission dependencies.
Remap preserves principal ID, name, default schema, ownership and database
rights while changing login SID mapping. Warn about existing session tokens and
unexpanded inherited/fixed-role privileges. Direct affected state is reviewed;
this is not an effective-permission matrix or a lock on concurrent role changes.

The bounded principal report adds nullable resolved login and default-schema
fields. An unresolved login can mean no login, an orphan, or visibility limits.
The Vim administration form exposes all four actions through the existing
source-bound review and confirmation flow. No Driver signature, metadata
migration or credential-storage change is needed.

Native references: [CREATE USER](https://learn.microsoft.com/en-us/sql/t-sql/statements/create-user-transact-sql?view=sql-server-ver16),
[ALTER USER](https://learn.microsoft.com/en-us/sql/t-sql/statements/alter-user-transact-sql),
[DROP USER](https://learn.microsoft.com/en-us/sql/t-sql/statements/drop-user-transact-sql?view=sql-server-ver17).

SQL Server security inspection and edits require a database-wide Sift connection
scope; a schema-only profile, including one scoped to sys, cannot authorize
these database-wide actions. A read-only Sift profile rejects Apply before
catalog work or SQL parsing. Native permissions remain independently required.


**Validation.** Native SQL Server 2022 acceptance exercises quoted create,
disabled-login preservation, schema resolution, remap/orphan repair with real
preserved SELECT rights, drop/login survival, current-actor and unsupported
refusals, narrow CREATE USER versus CONTROL/ALTER ANY USER rights, revocation,
metadata DENY, and changed principal/login/schema/actor/database/security state.
ROWCOUNT 1 leaves both the review hash and editor limit intact. GPUI verifies
all four forms, exact source and confirmation guards. API/policy tests cover
invalid SQL-user names, database-wide scope and early read-only Apply refusal.
Formatting, strict workspace/native-target Clippy and workspace tests pass.
Broader identity families and native Windows/macOS hosts are not claimed.


## ADR-133 — Desktop CSV review retains immutable sources and ordered outputs

Status: accepted.

CSV review edits an ordered vector of original source positions and distinct
output names. Dropping, reordering, copying and renaming never mutate the source
file. Vim controls and buttons edit this vector; the sample grid, generated DDL,
CSV payload and SQLite native conversion all use it. Each copied output retains
its own target name and type input. Saved recipes use ADR-130 CSV version 2 with
exact original source headers, rejecting ambiguous selected headers. Durable
resume continues to fingerprint the original upload.

Desktop review explicitly limits source and output width to 256 columns,
source headers to 4096 bytes, target names to 128 bytes and original/mapped CSV
to 64 MiB. Inference retains classifications from 200 rows; 20 display rows use
bounded UTF-8 snippets. Immutable shared source bytes keep local edits cheap.
Full-file rewriting and native conversion run on a background executor. A bounded
writer enforces output limits during emission; typed JSON size is checked through
a bounded sink. SQLite retains its tighter native import limits.

A frozen preparation binds the mapping, type choices, source bytes, policy and
server/tenant/profile/session/connection identity. UI connection generations
reject disconnect/reconnect reuse. Both UI completion and executor dispatch
check the source, including parked CSV connections. Local cancellation fences
late preparation without dispatch; cancellation after dispatch retains the
inspect-target warning. Mapping and type edits invalidate old dry-runs and native
tokens; native apply reuses only the exact reviewed typed rows. Existing audited
ImportCsv, BulkInsert and transfer recipe operations remain the write boundary;
no protocol or Driver signature changes are needed.

Expressions, other formats, wider desktop files and native Windows/macOS
acceptance remain separate work. This decision does not expand native SQLite's
supported values or the server's CSV projection semantics.


## ADR-134 — Benchmark source context is optional private client metadata

Status: accepted.

A saved benchmark may contain immutable client-reported source context beside
its immutable measurement report and editable notes/tags. Keep this context
outside BenchmarkReport and portable artifacts: source references do not prove
execution provenance or database/data equivalence and grant no workspace access.
Absent context defaults cleanly for existing requests and saved snapshots.

Desktop capture requires a document explicitly bound to the current instance,
workspace and room snapshot. Freeze its relative path and workspace revision
before workload review, including reusable-definition runs. Cached Git status
must match the observed repository binding and revision; retain its own time,
workspace revision and truncation state. An incomplete empty status is unknown,
not clean. Capture no native roots, remotes, URLs, diffs, contents or bind values.
Run-ID-bound view state carries the context with the report; failed runs retain
prior measurement context and stale responses cannot replace it. Do not look up
the current workspace when saving an earlier run.

Saving context requires explicit opt-in, off by default, with the frozen context
and observation timestamps visible in the Vim browser. API clients may explicitly
provide the same bounded typed metadata. Validate positive workspace IDs,
normalized relative paths, Git object IDs and bounded printable labels; reject
unknown context fields. No supplied reference is used for authorization or I/O.
Existing owner/tenant/membership checks remain authoritative.

Store context only inside the encrypted SecretStore payload, sharing existing
snapshot/owner quotas. Annotation replacement preserves it. SQLite indexes,
audit records, list summaries, AI/MCP measurement projections and portable
measurement exchange omit context. Explicit owner-private saved-snapshot JSON
copy includes it and says so. Existing audited save/get/delete operations cover
its lifecycle; no Driver change or new I/O in protocol is needed. Retention,
attached profiling plans and native Windows/macOS acceptance remain separate.


## ADR-135 — Private benchmark retention uses reviewed policy and durable cleanup

Status: accepted.

Saved benchmarks have an owner-private, tenant-scoped age/count policy, disabled
by default. Age uses the immutable server save timestamp; count keeps the newest
records with a stable ID tie-break. Either criterion can remove the complete
snapshot, including notes and source context. Hard snapshot/owner quotas remain.
V056 stores only numeric criteria, revisions and maintenance timestamps in
SQLite. Policy membership foreign keys stop automatic deletion after revocation;
rejoining does not reactivate the old policy.

Policy preview binds the authenticated owner/tenant, desired criteria, policy
revision, server cutoff and current indexed snapshot identities, timestamps,
handles, byte counts and annotation revisions. Activation requires confirmation
and recomputes the five-minute digest under the same transaction as policy update
and reviewed removals. The digest is a state fence, not authorization or server
attestation. New saves enforce retention within their insert transaction, then
check hard quotas and confirm the newly saved entry survived. Failed inserts
roll back removals, cleanup enqueueing and prune audits.

A bounded fair worker visits enabled policies independently of database drivers.
Logical removals write typed PruneBenchmarkRuns audits in their transaction.
DELETE and handle-replacement triggers queue obsolete encrypted payloads in the
existing durable cleanup queue, including cascades. Cleanup supports the benchmark
namespace, rejects deletion of current handles and retries backend failures.
Enabled-owner selection uses a partial ordered index; cleanup merges bounded
indexed batches from each supported namespace. SQLite selection, reference
checks and acknowledgments run in blocking jobs. A current-reference check and
stale-queue acknowledgment share a transaction so a later reference removal
can enqueue cleanup without losing it. Secret backend I/O holds no SQLite lock.
Rejected saves and annotation replacements also queue abandoned payloads; cleanup
failure must not hide the original conflict or claim an accepted write failed.

The existing audited private-library API and thin SDK expose policy read,
preview and update. The disconnected Vim editor preserves conflict drafts,
invalidates reviews on edits, binds responses/dispatch to server/tenant/request,
and refuses to replace an active note/tag draft. No secret bytes, snapshot text
or handle lists enter policy responses/audits. Full metadata backups and same-ID
tenant recovery preserve policies; original timestamps can cause restored old
snapshots to expire after startup. Physical deletion can lag logical removal;
backups retain their own copies. This is not a secure-erasure guarantee.

Driver signatures, protocol I/O and CI workflows remain unchanged. Attached
Profile plans, advanced performance tools and native Windows/macOS acceptance
remain separate work.


## ADR-136 — Private Profile attachments preserve separate measurement populations

Status: accepted.

A saved benchmark may retain an explicitly selected Profile attachment in its
existing encrypted SecretStore payload. The attachment records capture time,
exact SQL, parameter count and typed Profile evidence. The server validates
matching SQL, engine and parameter count, a distinct run ID, bounded raw/normalized
plans and finite nonnegative clocks/counters. SQLite estimated nodes cannot
acquire native runtime counters. This is caller-associated evidence, never a
server attestation or proof of equivalent data/cache conditions. Profile timing
remains separate from ordinary benchmark samples and comparisons.

The desktop freezes an ephemeral salted fingerprint at confirmed workload
dispatch, including SQL, bind values, instance/profile and session/connection
generation. Only matching captures offer an attachment. Salt/digests and bind
lists are never persisted. Pending, failed or mismatched responses cannot create
an association. Saving freezes the offered capture and requires separate opt-in,
off by default. Raw plans may themselves contain sensitive names/literals/values.

Attachments share snapshot and owner quotas, ownership, annotation preservation,
retention, backup and durable cleanup. Legacy payloads decode without them. No
new metadata migration is required. Existing audited save/get/delete operations
cover their lifecycle; no Driver change or protocol I/O is introduced. Private
snapshot JSON copy discloses included plans; portable measurement artifacts,
numeric exports, list summaries and AI/MCP measurement projections omit them.

Vim saved-run browsing can reopen an attached plan through the existing Plan
renderer in a query tab without execution. It refuses server/tenant scope changes
and active Benchmark/Profile jobs, labels historical caller-associated evidence,
and clears live association on reopening. Baselines remain measurement-only.
Broader plan comparison, controlled variants, advanced performance tools and
native Windows/macOS acceptance remain separate work.


## ADR-137 — Saved plan comparison uses bounded heuristic tree correspondence

Status: accepted.

Private benchmark snapshots can compare attached Profile evidence without
executing work. Both attachments must validate and share engine and analyzed
state. The pure core compares normalized operators/relations, reported numeric
fields, child counts and native attributes. Missing remains unavailable, distinct
from zero. Capture clocks remain separate observations; costs/node times are
never summed and provider loop/rebind semantics are not normalized. Benchmark
samples and regression thresholds remain a separate population.

At paired parents, ordered LCS aligns unique sibling (operator, relation) keys.
Unmatched gaps pair as replacements, with excess additions/removals. Duplicate
keys or exhaustion of a global one-million-cell matrix budget use labelled
positional comparison. Every input node remains represented; output is bounded
by the sum of input nodes under existing 4096-node/64-level attachment limits.
Moves can appear as removal/addition. Paths are structural addresses and all
correspondence is heuristic, not semantic node identity. No controlled-regression
or result-equivalence conclusion follows from identical or changed plans.

The Vim saved-run browser freezes loaded A/B captures, computes in the background,
fences late results by request/source/record identity, and shows a virtualized
node view with bounded detail and explicit private JSON copy. Source changes
refuse actions and record refresh clears cached comparison. Escape cancels pending
presentation or returns to browsing. CLI `performance compare --plans` uses the
two existing audited owner-private reads; default measurement output and AI/MCP
projections stay unchanged. Raw plans/SQL/bind lists are omitted from comparison
rows, while identifiers/attributes retain an explicit sensitive-data warning.

The UI depends on pure core comparison logic; shared crates gain no UI dependency.
No wire contract, Driver signature, schema migration or CI change is needed.
Untimed result equivalence, controlled A/B execution and native Windows/macOS
acceptance remain separate work.


## ADR-138 — SQLite binding uses shared native slot discovery

Status: accepted.

SQLite's positional value vector follows native bind indexes for anonymous `?`,
positive `?NNN` and case-sensitive `:name`, `@name`, `$name` tokens. Prefixes belong
to names. Repeated names and explicit numbered aliases share a slot; new names
and anonymous parameters follow the largest assigned index. Mixed forms are
supported only when the final slots are contiguous. Zero/oversized indexes,
gaps, malformed names and internal `#` parameters are refused. Unicode and Tcl
namespace/suffix tokens follow the bundled tokenizer, including its whitespace
rules. Quoted text, comments, blob contents and identifier bodies do not supply
parameters. Alias lookup and rendering are bounded; labels disclose truncation.

A pure core lexer/layout serves the driver, policy/read classification, SQL
variables and desktop. Normalization replaces only complete host-parameter tokens
with canonical placeholders for classification; original SQL executes unchanged.
Native prepared count/name/index metadata must agree before binding. Per-slot
cancellation checks retain responsiveness during native name lookup. Typed lexer
errors preserve syntax versus invalid-parameter codes. Parameterized batches are
natively prepared and refused before any step, retaining native
read-only/authorizer and cancellation boundaries. No Driver or wire signature
change is required, and no UI dependency enters shared crates.

SQLite template scanning treats native parameter names atomically and reserves
existing slots before generated `?NNN` bindings. Actual template references are
excluded from discovery; templates embedded inside a native name are refused.
PostgreSQL/SQL Server template behavior remains unchanged. WebSocket template
history validates descriptors using the actual connection engine. This performs
no textual value substitution and stores no new secret bytes.

Desktop parameter forms are engine-aware, show bounded native index/alias labels
and use explicit native focus targets and Submit handling for Vim interaction.
Desktop and definition forms cap at 256 slots; native API calls retain 32766.
Definition save/edit/rerun validates actual cardinality, legacy mismatches require
editing, and definition review always creates fresh values with persistent alias
labels. Existing source/revision and whole-workload confirmation guards remain.
Existing audited execute/Benchmark/Profile/definition operations cover behavior;
no schema migration, environment variable or CI change is introduced.

Native prepare metadata, real-file driver/API and actual Vim interactions provide
acceptance. Native Windows/macOS host acceptance remains deferred.


## ADR-139 — Shared ordered XLSX/Parquet import projection

Accepted 2026-11-05. Metadata now admits persisted Parquet recipes alongside its
existing execution support. Import format version 2 extends the ordered selection and
legacy partial-rename contracts to XLSX and Parquet. Pure core parsing and exact
source resolution serve the server and Vim recipe editor. Selections remain
bounded, mutually exclusive and free of expressions. CSV version-1 rename
compatibility remains; export version 2 stays refused.

Parquet resolves against the original Arrow schema, reads unique selected roots,
and then reorders or copies native values. Unsupported omitted nested fields are
not decoded. Selected scalar types and field metadata survive decoding; native NULL/empty
text/binary values survive selection. No new database metadata is created. Original source limits remain, and decoded copy expansion is checked
before cloning variable payloads. Type overrides, resume and non-abort conflict
policies stay refused. Existing transactional import and audit operations apply.

XLSX retains explicit sheet selection and text-cell conversion through the CSV
importer. ZIP/XML conversion runs in a blocking task with archive, XML, cell-index,
actual read and emitted-output limits. It introduces no formula evaluation or
Excel type interpretation. Durable XLSX resume remains unsupported; stateless
CSV-style resume is preserved. SQLite refuses case-folding output aliases before
preview or writes, including CSV-backed XLSX, to prevent multiple outputs binding
one physical column.

Parameterized PostgreSQL runtime failures terminate result streams with error
pages, allowing import to roll back instead of returning successful completion.

The generic Vim recipe editor validates the shared contract and exposes field
focus, save and Escape commands with format-specific guidance. No new Driver
signature, wire shape, schema migration, environment variable or workflow is
needed. Expressions, direct non-CSV column grids and wider sources remain open.
Native Windows/macOS acceptance remains deferred.


## ADR-140 — Explicit PostgreSQL membership grantors and options

Accepted 2026-11-05. PostgreSQL 16+ membership inspection and administration use
native explicit edges, keeping role, member, grantor and ADMIN/INHERIT/SET options
separate from effective object privileges. Older engines receive an unsupported
response before new catalog columns are queried; other workbench views retain
their current version support.

Typed grant/update requests specify all three options and a named grantor. Typed
revoke requests identify that grantor and use RESTRICT. Other grantors can retain
effective membership; reviews disclose this and the cluster-wide authorization
impact. No-op grants, missing selected edges and self-membership are refused.
Native PostgreSQL enforces permissions, grant dependencies and cycles.

Preview captures public role/member/grantor attributes and OIDs, actor/session
and database identity, selected membership edge identities/options and the
grantor's ADMIN edges. The bootstrap grantor uses PostgreSQL's fixed identity,
not its mutable name. Captured edge sets cap at 200; overflow refuses review.
Passwords and role configuration are never selected. Exact quoted SQL and the
observations form the precondition hash. Apply repeats this capture and requires
both normal and production confirmation through existing audited, supervised
List/Preview/ApplyPostgresObject operations and current profile policy. Identity,
authority or membership changes observed at revalidation invalidate review;
preview does not lock catalogs against later concurrent DDL.

The SDK and Vim Monitor Memberships view expose bounded paging and explicit
role/member/grantor forms. Vim toggles all options, chooses grant versus revoke,
and stages exact SQL for explicit apply. Catalog replies bind full database
context, request identity and an invalidated read generation; mutation reviews
retain the existing source/request fences. No Driver signature, metadata schema,
secret storage, environment variable or CI workflow changes are introduced.
Role attributes/lifecycle, passwords, effective privilege matrices and reviewed
cascade remain separate work. Native Windows/macOS acceptance stays deferred.


## ADR-141 — Ownership of connection results after abandoned waits

Accepted 2026-11-05. A driver opening result owns its handle, tunnel and tenant
connection reservation before it crosses the supervised task boundary. Only
successful registration in a live Sift session adopts those resources. A timeout,
dropped caller or missing session cannot discard an unregistered native handle.

Dropped results start exactly one supervised cleanup. A five-second observation
deadline does not abort native close or release its retained tunnel, tenant quota
or opening admission; pending cleanup continues to own them. Pending opens and
late cleanup share a 256-slot admission bound, independent of established
connection limits. A wedged cleanup therefore cannot admit unbounded replacement
opens. Native cleanup failure is reported with a static warning, without secret
bytes. Runtime teardown remains the fallback when no runtime can accept cleanup.

The captured shutdown query guard also belongs to the pending opening result and
travels through actual late close, including after the cleanup observer returns.
Successful adoption releases this opening guard; the established connection does
not keep the opening request in flight. Timeout, caller loss and missing-session
tests gate actual close and verify that shutdown tracking follows this lifetime.

Successful adoption transfers handle, tunnel and tenant reservation into the
existing connection entry and releases opening admission. Driver task limits,
source authority, request timeouts and provider open/close contracts remain in
force. No Driver signature, protocol, metadata, environment or CI change is
introduced. Deterministic gates exercise timeout, caller cancellation, session
closure, pending native close and normal adoption through the actual SessionStore.
This ownership foundation precedes disposable SQL Server generated-column writes;
those import modes and their progress/cancellation workflow are separate work.

## ADR-142 — Reviewed generated-column imports on owned SQL Server connections

Accepted. The opt-in CSV/XLSX/typed-Parquet workflow separates explicit identity
Reject/Generate/Preserve and computed Reject/Omit choices. Existing ordinary
native tables, unique native-collation bindings, supported scalar types, native
INSERT authority, and ALTER authority for preservation are required. Hidden,
generated-always, rowversion and column-set input is refused. Only atomic Abort
without resume, target creation or type overrides is supported by these modes.

Dry runs report actual native target identity and imported/omitted columns. A
process-salted digest binds source session/connection, parsing/data, modes, native
actor/target/column metadata and authority; recipe execution also binds the saved
recipe and actor. Apply requires an exact transient confirmation and rechecks
metadata before writes and commit. Identity counter last values are deliberately
excluded: the import itself changes them. Reviews are not durable catalog locks.

The write coordinator owns a disposable native connection using ADR-141 opening
ownership, bounded task admission, tenant connection quota and tunnel ownership.
Only the existing literal startup allowlist is replayable; normalized strict SET
options and IDENTITY_INSERT never contaminate editor sessions. Native parameter
batches run inside one serializable transaction. Counter, trigger and external
side effects may survive rollback; row counts describe accepted input rather
than target equivalence. Native failures expose stable codes and numeric native
codes without row values.

Caller-selected run UUIDs bind active progress and audited cancellation to the
original source. Four global ownership slots and one active run per source bound
work. Caller abandonment/source closure requests cancellation. Attempted and
confirmed committed rows remain separate. Cancellation stops future batches;
COMMIT uncertainty is explicit. Cleanup has a five-second observer and retains
admission, quota and tunnels through native close, including session closure.
The desktop keeps the original RPC alive after cancellation, retries cancellation
while registration is pending, and acknowledges one progress update at a time.
Vim mode changes invalidate review; saved recipe modes exclude confirmations.

Lost COMMIT acknowledgement, cancellation/timeout during COMMIT and supervisor
failure use the typed ImportOutcomeUnknown error and OutcomeUnknown change-ledger
outcome. Metadata V057 expands the outcome constraint by rebuilding the ledger,
preserving historical row/hash bytes, AUTOINCREMENT high-water marks, indexes and
append-only triggers. This is an additive migration. Driver signatures and pure
serde protocol boundaries remain intact; CI stays manual. Native Windows/macOS
acceptance remains explicitly deferred.

## ADR-143 — PostgreSQL public role lifecycle with native identity review

Accepted. The role editor now supports typed create-with-public-attributes,
rename, attribute patch and drop actions, alongside compatible NOLOGIN creation
and ADR-140 explicit membership management. Public patches cover LOGIN, INHERIT,
CREATEDB, CREATEROLE, SUPERUSER, REPLICATION, BYPASSRLS, connection limit and
password validity (typed UTC expiry or infinity). Unspecified values retain
native attributes/defaults; invalid limits and empty/no-op updates are refused.
Catalog detail never selects password hashes. Credentials and per-role GUC
settings remain separate workflows.

Review snapshots select explicit public role columns and bounded native
membership/shared-dependency records. PostgreSQL JSON encodes OIDs as strings;
identities retain that native representation. Finite expiry in review uses
canonical UTC timestamps, independent of DateStyle/TimeZone. Tokens bind exact
rendered SQL, source session/connection, database/version, actor/session actor,
target/rename destination identities and attributes, native ADMIN authority and
complete bounded impact. More than 200 memberships or dependency addresses
refuses preview. Apply repeats native review and policy checks, then uses the
existing supervised Driver query path; no Driver signature changes are needed.

Authority follows the native operation: CREATEROLE plus ADMIN for the relevant
non-superuser target, privileged-attribute restrictions, and special treatment
of replication targets. SUPERUSER changes require native superuser authority;
the bootstrap superuser's SUPERUSER property is immutable. Reserved roles and
rename collisions are refused. Rename/drop of current/session actors requires
a different source connection. Native checks remain authoritative under races.
No automatic SET ROLE, DROP OWNED, ownership reassignment or cascade is issued.

Every action reuses audited typed PreviewPostgresObject/ApplyPostgresObject
operations with exact SQL and explicit production confirmation. Previews disclose
cluster scope, attribute/session and membership-default effects, conditional MD5
password clearing on rename and dependency-blocked drop. Impact is bounded native
addresses, not complete effective-privilege or dependency-remediation analysis.
The Vim form cycles lifecycle actions, preserves keep/on/off attribute choices,
validates optional limits/expiry, and uses existing source/request/reconnect fences.
Native Windows/macOS acceptance remains deferred; CI remains manual.

## ADR-144 — Controlled paired benchmarks with separate populations

Accepted. Paired benchmarks freeze A/B SQL and binds, one validated set of
per-population sampling limits and a shared sampling deadline. They reuse one
owned dedicated connection and existing native read-only policy. SQL Server
still requires a read-only account for database-enforced protection. Both SQLs
require current benchmark_pair and ordinary execution authority; progress reads
and every attempt recheck policy. The audited operation retains source/run IDs,
never SQL or binds. No Driver signature changes are needed.

Warmup rounds precede measured rounds. Each round runs each population once.
Alternating starts A/B and flips B/A, resetting at the measured phase. Version-1
randomization uses a specified SplitMix64 coin per round, seeded by the recorded
u64 (including zero), continuing state across phases. The first failed, cancelled
or timed-out attempt, authority/resource loss or budget exhaustion stops the pair.
Each population retains contiguous local ordinals and its own report/statistics;
warmups and unsuccessful attempts do not enter latency distributions. Rows are
fully consumed and discarded. No significance or result equivalence is claimed.

Optional version-1 pair context records the original experiment UUID, population,
order/seed, actual global attempt prefix and overall completion. Private save
validates the bounded schedule and local sample count after normalization. JSON
and text artifacts preserve context; import allocates a fresh local report UUID
without rewriting original experiment lineage. Context is unattested measurement
data and never grants execution authority. An incomplete pair withholds deltas
even when one population locally completed. Complementary completed populations
may have different parameter counts; shared cache, session and transaction effects
remain explicit limitations. Instrumented Profile samples stay separate.

Vim pins A from a frozen ordinary review without executing it, then reviews B in
single/alternating/randomized mode. Both anchors, binds, original instance/tenant/
profile/runtime generation and source SQL are checked before exact-source dispatch.
The seed is editable, including zero. Results select A/B through existing private
save/export workflows with independent opt-in source context. CLI execution needs
an explicit confirmation flag, never approval supplied by JSON; optional A/B saves
are separate and report the first saved ID if the second fails. AI/MCP can inspect
bounded pair metadata but cannot start paired execution.

Root progress/cancellation uses the existing benchmark path and aggregate attempt
counters. HTTP caller abandonment cancels a shared token while a separate audit
supervisor remains alive to record the final outcome. Native close ownership
outlives its five-second observer without task
abort, retaining quota/tunnels until native cleanup settles. Linux native acceptance
covers PostgreSQL, SQL Server and SQLite; native Windows/macOS remains deferred.
CI stays manual. The broader performance workbench remains partial.

## ADR-145 — Reviewed public PostgreSQL role session defaults

Accepted. Typed role set/reset supports work_mem, maintenance_work_mem,
statement_timeout, lock_timeout, idle_in_transaction_session_timeout, jit,
default_transaction_read_only and max_parallel_workers_per_gather. Input uses
bounded integer KiB/milliseconds/workers or on/off according to the setting.
Arbitrary/secret-bearing GUCs, SQL/unit strings, RESET ALL and FROM CURRENT are
excluded. Scope is all databases or the connected database for one named role;
there is no ALL-roles or arbitrary database target.

Role catalog details select only those public defaults at the two scopes from
pg_db_role_setting. Values cap at 64 characters with explicit truncation flags;
they describe stored defaults, not effective configuration. No unfiltered
setconfig/rolconfig or credential-bearing catalog field crosses the API.

Existing audited PreviewPostgresObject/ApplyPostgresObject and production
confirmation cover changes. A supervised native snapshot binds exact SQL, source
session/connection, database identity, current/session actor, target identity and
attributes, ADMIN authority and both global/current-database values for the chosen
setting. Apply repeats review and rejects changes; it is not a catalog lock.
Authority follows PostgreSQL 16+: superuser, the role itself, or CREATEROLE plus
ADMIN on a non-superuser target. Reserved roles stay gated. No SET ROLE or privilege
elevation is performed; Driver signatures remain unchanged.

Defaults affect new logins. Existing sessions and SET ROLE retain their values;
database-specific role defaults override global role defaults, and RESET falls
back to other configuration. These are overrideable workload defaults, not
security/resource-limit enforcement. See PostgreSQL's
[ALTER ROLE contract](https://www.postgresql.org/docs/17/sql-alterrole.html).

Vim adds set/reset modes, explicit scope toggle and typed units to the existing
source-bound role form. Review shows stored values and exact SQL; apply needs
explicit confirmation with the original request/source guards. Native acceptance
covers own/delegated authority, revocation, fresh-login precedence, retained
current-session/SET ROLE settings, stale values/role replacement and custom-setting
privacy. Credentials and arbitrary GUC management remain open. Windows/macOS
acceptance stays deferred; CI stays manual.

PostgreSQL pool reuse is not a fresh native login: logical close/open can retain
the prior native session defaults. Review discloses this; changes do not recycle
active/idle pool sessions. Native acceptance uses new psql processes to prove
login behavior, alongside SDK checks for unchanged existing sessions.

## ADR-146 — Explicit-target SQLite statistics and index maintenance

Accepted. Managed SQLite maintenance adds ANALYZE of one ordinary main table and
REINDEX of one main index on an ordinary table, including constraint indexes.
Shared pure SQL rendering always qualifies main and quotes bounded identifiers;
database-wide, collation-name, TEMP/attached, virtual, shadow and internal-table
targets are excluded. Full statistics capture can change plans; no optimization,
approximation or latency guarantee is implied. Reports add optional exact SQL.

Audited ManageSqliteDatabase, current root/tenant/read-write authority, original
opened file identity, one-use action/source-bound preview, explicit write and
verified-backup acknowledgment remain mandatory. Native proof binds schema_version
and the owning table/index definitions (at most 201 rows and 1 MiB). Oversized
names/definitions are refused before application allocation. Leases retain a
SHA-256 metadata digest. Native preview uses a ten-second progress deadline;
apply uses a 120-second deadline and BEGIN IMMEDIATE, repeating proof under the
write lock before execution. The 1 GiB source ceiling includes WAL for these
actions. Preview does not hold a catalog lock.

The admitted SQLite worker owns one transaction, bounded busy waits and progress/
cancel/close checks. Failures and cancellation roll back; guards restore internal
authority, progress and busy policy. A rollback failure closes the worker instead
of reusing an uncertain transaction. Durable journaling/synchronous checks apply.
Caller abandonment requests native cancellation while an owned HTTP audit task
retains shutdown tracking and records the terminal request outcome.
Timeout alone cannot establish commit outcome. Native values are not edited;
locks, journal/disk writes and native uniqueness/collation failures remain visible.
No extension loading, arbitrary PRAGMAs or Driver signatures are introduced.

Vim adds explicit table/index choices, exact SQL and connected-source typed
confirmation to the existing Maintenance panel. Source, generation and form
changes invalidate reviews. Real-file HTTP/SDK acceptance verifies statistics
scope, native writes, quoted names, values/integrity, active transactions, stale
schema, competing writers and unsupported/read-only targets. Worker acceptance
cancels after observing a write transaction, verifies rollback and subsequent
connection authority/usability, and unwinds a native uniqueness failure. Existing
GPUI confirmation coverage includes both new choices. Physical ENOSPC and native
Windows/macOS acceptance remain open/deferred; CI remains manual.


## ADR-147 — Reviewed SQL Server index maintenance on owned sessions

Accepted. A typed audited MaintainSqlServerIndex operation exposes single-index
REBUILD (ONLINE OFF, MAXDOP 1), REORGANIZE and index UPDATE STATISTICS (FULLSCAN,
MAXDOP 1). Pure SQL rendering quotes bounded UTF-16 sysname identifiers. Scope is
one enabled, non-hypothetical, single-partition disk rowstore index on an ordinary
writable user-database table, including constraint and supported filtered/computed
indexes. Partition schemes, special table families, disabled indexes and ALL,
online/resumable or automatic threshold-based actions are excluded. REORGANIZE
requires page locks. No performance gain is promised.

Preview requires native ALTER and VIEW DEFINITION plus current source operation
and execution policy. Read-only/schema-restricted profiles are refused. Bounded
native metadata binds database creation identity, actor/SIDs, table/index shape,
columns, physical partition/HoBT identity and statistics timestamp. Native
SHA-256 then a source/session/exact-SQL digest bind review without returning raw
catalog definitions. Apply repeats policy, authority and identity and requires
production and verified-backup acknowledgments. Optimistic review is not a
catalog lock; native DDL remains authoritative under concurrent changes.

A disposable connection replays only the existing strict startup allowlist and
preserves source provenance. Required SET options, ROWCOUNT 0, XACT_ABORT and a
five-second lock timeout are normalized there; source editor state is preserved.
Native source/dedicated actors and database must match; unreplayable impersonation,
USE changes and active transactions are refused. Driver supervision and owned
late-open/close machinery retain quota and tunnel ownership beyond observer
timeouts (ADR-129); Driver signatures remain unchanged. Caller abandonment
signals cancellation while an owned HTTP supervisor records the terminal audit.

Acknowledgment is the completion signal. REORGANIZE may retain partial progress;
timeout, cancellation, failure or disconnect cannot prove completion or rollback.
Review discloses offline blocking, log/temp/disk load, statistics/plan changes and
backup expectations. Vim review binds the original instance/tenant/profile/
session/query connection, request generation, unchanged form, backup acknowledgment
and exact typed phrase. Native restricted-user HTTP/SDK acceptance proves actual
physical/statistics changes, denial/revocation/stale targets, preserved editor
state and cancellation behind an observed write lock. GPUI covers the source and
confirmation guards. Native Windows/macOS acceptance remains deferred; CI remains
manual.


## ADR-148 — Native selected-role PostgreSQL privilege matrix

Accepted. ReadPostgresPrivileges returns bounded effective ACL checks for a named
public role, defaulting to current_user, within the connected database. Objects
include the database, user schemas, ordinary/partitioned/foreign tables, views,
materialized views, individual live user columns, sequences and routines. Checks
use native has_database/schema/table/column/sequence/function_privilege for both
access and WITH GRANT OPTION. PostgreSQL 17+ adds MAINTAIN. Native membership,
PUBLIC, ownership and superuser semantics remain authoritative; no explicit-ACL
union, inferred grant provenance or SET ROLE is used in implementation.

Admission uses the connected actor's ownership/privileges and visible user
schemas, independently of the selected role. Selecting a stronger role never
broadens actor catalog visibility. System/TEMP objects, definitions, row data and
protected catalogs are excluded. Managed schema-restricted profiles are refused;
read-only and current operation allow/block policy remain applicable. One
parameterized supervised Driver read returns at most limit+1 objects (limit
1..200, offset <=10000), with a 1 MiB response ceiling. Roles use bounded names.
A page ceiling reports incompleteness explicitly; paging observes a changing
catalog, not a stable or exhaustive snapshot. Current policy is rechecked before
publication. An owned HTTP audit supervisor retains shutdown tracking and records
the terminal outcome even if the caller stops waiting. Driver signatures and
protocol purity remain unchanged.

ACL flags are not executable-query authorization. Schema USAGE, RLS, view and
security-definer behavior, dependencies, HBA/login rules and concurrent changes
still matter. RLS enabled/forced flags and subject superuser/BYPASSRLS flags are
observations, not policy evaluation. Object OIDs and column numbers distinguish
native identities and routine overloads without returning definitions. Omitted
objects do not imply denials. Languages/types/tablespaces/large objects/parameters,
role-membership paths, grant provenance and object grant/revoke editors remain
separate work.

Vim Roles opens the selected role matrix with v, pages/refreshes with n/p/r,
selects with j/k and closes with Escape. Virtualized object navigation shows
native access/grant cells for the selection. Requests and replies bind exact
instance/tenant/profile/session/query connection, role and request identity.
Source/view changes invalidate the matrix; refresh cannot silently rebind it.
Native HTTP/SDK acceptance covers inherited versus SET-only membership, PUBLIC,
column-only grants, revoked owner ordinary access with retained grant options,
RLS restrictions despite ACL access, sequence/routine checks, overload identity,
native revocation, restricted-actor visibility, paging and audit. GPUI covers
Vim entry/paging, stale request responses, changed native source and close.
Native Windows/macOS acceptance remains deferred; CI remains manual.


## ADR-149 — Proven SQLite nullable column additions without destructive inverses

Accepted. Existing audited schema-diff/migration and typed designer preview/apply
admit native ADD COLUMN of a nullable INTEGER, REAL, TEXT, BLOB or NUMERIC column
at the end of an ordinary main table. Names are quoted identifiers and types
are a fixed native whitelist. Defaults, constraints, keys, generated/hidden/
virtual columns, type parameters, reorder and existing-column type/nullability
changes remain excluded. Native additions preserve existing rowids and values;
existing rows expose NULL in the new column. No table rebuild or root/file
boundary change is introduced.

Reuse the full unfiltered main/TEMP simple-table capture boundary, bounded native
canonical declaration proof, dependency refusal, original source policy/live
revision, forced native transaction, supervised execution and validation rollback.
Both declarations and every original column must match except for a contiguous
appended tail. A selected whole-table proof change requires all additions;
column-only selection admits a proven prefix and refuses skipped ordinals.
Mixed existing-column changes on that table remain unsupported. Typed AddColumn
preview derives native facets and the new canonical declaration from structured
fields after verifying the source proof; it never executes DDL or replaces SQL
substrings to discover a target.

SQLite column creation is classified lossy for data rollback. New clients may
populate it immediately; automatic DROP COLUMN would destroy their values.
Addition steps therefore omit rollback SQL and disclose this in a deduplicated
plan warning, visible in the designer and as comments in exact SQL review.
Validation rolls back its own transaction; it does not use a destructive inverse.
Existing native rename steps retain exact reverse-order inverses. A future drop
workflow needs independent destructive review. See SQLite's
[ALTER TABLE contract](https://www.sqlite.org/lang_altertable.html).

Vim enables the new-column row with TEXT as the initial type and fixed nullable
state. Existing columns retain rename-only controls, and reordering stays gated.
Original instance/tenant/profile/session/query connection and request/form guards
still govern preview/apply. Real-file HTTP/SDK acceptance covers all five types,
quoted SQL-looking identifiers, preview without writes, validation rollback,
existing rowid/value/storage-class and populated-cell preservation, native NULLs,
read-only/stale/index/view refusals and coupled multi-column selections. Focused
GPUI acceptance retains the existing rename/source guards and verifies typed
preview-only nullable addition. Driver signatures and protocol wire types remain
unchanged; native Windows/macOS acceptance stays deferred and CI stays manual.


## ADR-150 — Conditional order-statistic median bounds for comparisons

Accepted. Shared benchmark comparisons add an optional, read-derived
median_uncertainty model: non-interpolated binomial order-statistic intervals
with 97.5% target marginal coverage and Bonferroni 95% target joint coverage.
This is conditional on IID observations within each population; independence
between the populations is not required for the union bound. Native serial or
interleaved sampling does not establish those assumptions. Ties are conservative;
zero observed width does not establish zero variability. Model coverage concerns
repeated sampling, not a posterior probability or controlled regression verdict.
See [NIST TN 2119, section 5.3](https://doi.org/10.6028/NIST.TN.2119).

Require compatible complete reports, 1..10000 measured and at most 100 warm-up
observations, exact requested cardinality, zero-based contiguous native ordinals,
correct phases, successful outcomes and consistent client-elapsed aliases. At
least seven measured observations are required for finite sample-order bounds.
Warm-ups never enter the intervals. Failures, malformed traces and missing
observations withhold the model instead of being discarded. Existing descriptive
success-only summaries retain outcome counts. Completion-based selection,
stopping/retries and timeout censoring can violate the model; failure risk and
unconditional future latency are not estimated.

Mode-normalized binomial weights avoid large-n tail underflow; a conservative
cutoff margin favors wider bounds over accepting a rounded probability boundary.
Order-statistic endpoints remain exact u64 nanoseconds. Difference/percentage
bounds use integer numerators and exact binary-rational comparisons to round
finite float endpoints outward, with bounded correction work. Human endpoint
labels also round outward to 0.001 ms and 0.001%, and show explicit relative
unavailability reasons; formatting must not invent zero-width intervals.
Point median
deltas also subtract before float conversion, retaining small differences near
the u64 boundary. Absolute changes support an actual zero baseline; relative
points/bounds remain unavailable when their denominator can be zero. Missing
values are never fabricated as zero.

The model is recomputed from raw ordinary client-elapsed samples, never cached
summary fields, instrumented Profile clocks, SQL or plan payloads. CLI, governed
AI/MCP measurement reads and Vim live/saved comparisons share the calculation.
Existing owner/tenant reads and audit operations remain authoritative; no database
execution is introduced. AI/MCP projection adds bounded aggregate intervals and
assumptions without SQL, binds, raw sample vectors, notes, plans or source context.
Desktop comparison strings refresh on measurement/baseline changes and are reused
in rendering. Existing scalar threshold observations and CLI exit behavior remain
unchanged; repeated comparisons have no additional multiplicity correction.

Verification compares rank selection against an independent exact integer
binomial oracle and arbitrary-precision large-n fixtures, and covers tiny/tied/
zero/max-u64 data, signed rational rounding, invalid/failed traces, warm-ups and
summary forgery. Existing CLI/SQLite acceptance exercises eight real measured
attempts and private MCP/CLI output. GPUI checks baseline/report refresh and stale
completion refusal. Broader sampling evidence, controlled regression inference,
cache/load equivalence and Windows/macOS native acceptance remain open/deferred.

## ADR-151 — Negotiated external savepoints with owned transaction control

Status: accepted.

External-provider transactions previously stopped at begin/commit/rollback:
Driver RPC v1 had no typed savepoint methods. Driver family v2 adds savepoint,
rollback-to and release requests carrying the remote transaction ID and a bounded
label. Success must echo both exactly. The existing public Operations, HTTP API,
SDK and Vim controls remain the user surface; the locked Driver trait and native
engine extension signatures do not change. Default unsupported methods extend
only the server-internal DatabaseProvider adapter.

The supervisor selects the highest common host/manifest/Hello Driver version,
sends it in Welcome and retains it on the generation. Provider packages must
advertise that family; duplicate/invalid family ranges fail startup. Action-only
packages need not advertise Driver. Existing v1 packages retain their wire
contract. Archive and development validation require driver.core@1 for every
provider; driver.savepoints@1 requires driver.transactions@1 and a manifest
minimum of v2, preventing a downgraded generation from advertising savepoints.
Other undispatched advanced families remain refused.

An owned task serializes external transaction controls, including commit/rollback
and close cleanup, through acknowledgment and host-state settlement. Admission
waits at most five seconds (less for a shorter request timeout), then rechecks
current policy, source connection, transaction state and active savepoint names.
The task retains quota/tunnel leases through pending work. Connection cleanup
also has an independent owner, so dropping a close observer cannot abandon the
removed native connection. HTTP transaction-control/close supervisors retain
terminal audit and shutdown admission if a caller disconnects. An observation
timeout remains an audited failure even when its owned remote operation later
succeeds; late success still settles host state if its source remains active.

Only an exact ack updates the stack: rollback-to invalidates younger points;
release releases the selected point and younger points. Recovery clears only
failures known before its RPC began; a later query-failure report remains failed.
A provider error, transport deadline/disconnect, malformed response or mismatched
ack makes the native outcome uncertain. Stop the tenant/package generation,
return connection_invalidated and require reconnect. Other connections in that
generation are affected. Killing a process provides no native rollback guarantee
and process_only provides no OS sandbox guarantee.

The host never constructs provider-native savepoint SQL. Providers must quote
labels and serialize native per-connection/transaction work themselves. The
executable corpus models nested savepoints and covers negotiation/downgrade,
invalid names and ownership, exact acks, failed recovery, uncertainty fencing,
actual RPC deadline, observer/admission expiry (including an already-ready
lock), queued durable-policy revocation, concurrent failure reports,
abandoned HTTP audit/shutdown ownership and retained close quota/cleanup.
The server case explicitly requires building the existing host fixture binary.
This is host wire/lifecycle evidence, not native database certification; no quality
label is awarded. Provider-specific native evidence and Windows/macOS acceptance
remain separate, with the latter deferred by the user.

## ADR-152 — Authoritative, source-bound desktop savepoint snapshots

Status: accepted.

Rolling back to an outer savepoint previously left younger names in the desktop,
and release removed only the selected name. The server already knew which points
were active, invalidated or released. Query workers also read transaction snapshots
independently, allowing delayed replies to overwrite newer control outcomes.

The desktop now renders active names and transaction condition from the server's
transaction snapshot. A savepoint acknowledgment shows its outcome but does not
finish pending state or mutate the local name vector. The serial SDK executor
reads an authoritative snapshot immediately afterward; that read settles its
control. Refresh failure retains the last known names, reports unknown state and
prevents commit through the existing failed-state guard. No healthy recovery is
inferred from acknowledgment alone.

Query workers emit refresh requests rather than independently reading snapshots.
The UI queues those requests through the same executor as savepoint controls.
All snapshots are therefore read and emitted in executor order. Query-triggered
snapshots preserve a concurrent control's pending state; only its own subsequent
snapshot settles it. Authoritative absence clears an ended transaction instead
of retaining a pending control for a target that no longer exists. Reads have a ten-second deadline. Connection activation
restores the parked context's transaction and reads current state; disconnect or
an absent transaction clears old names.

Savepoint commands, refresh requests, acknowledgments and snapshots bind to
session, connection, transaction ID and opened-at identity. Both executor and UI
refuse stale targets; the UI also checks snapshot payload identity. This protects
replacement targets without adding shared/UI dependencies or public wire fields.
The existing Savepoint, RollbackToSavepoint, ReleaseSavepoint and ListTransactions
operations retain server authorization and audit. Native SQL Server release
refusal and capability checks remain authoritative.

Existing GPUI transaction tests cover authoritative active/inactive names,
concurrent pending ownership, failed recovery and stale session/connection/ID/
opened-at replies. An SDK/executor fixture covers stale-source refusal before
HTTP, server failure/absence, and query workers requesting rather than reading
snapshots. These are desktop state proofs. Server query/end admission fencing
is now covered by ADR-153 and terminal-control source binding by ADR-154;
provider-native transaction semantics remain separate follow-ups; Windows/macOS native acceptance remains deferred.

## ADR-153 — Connection admission through native query EOF

Status: accepted.

A query admitted with a managed transaction could previously reach native execute
behind commit and run in autocommit. A canceled observer could also abandon
execute before cursor creation or release ownership while a native stream was
still active. Independent native transaction ID allocators could overwrite host
metadata for a different engine's transaction.

Each live connection now owns an asynchronous read/write barrier. Ordinary
nontransactional queries take read leases; explicit transaction queries, begin,
savepoints and terminal controls take write leases. Admission is bounded by five
seconds and any shorter request timeout. After admission, workers recheck current
policy, connection identity and transaction/ending state, including an explicit
check when an expired lock is already ready. Host transaction IDs use one runtime
allocator; raw IDs remain inside native handles and engine savepoint calls.
Driver trait signatures and public protocol shapes are unchanged.

Query owners run independently of observers. They retain connection/tunnel quota,
query admission and a bounded global driver-work permit through native execute
and the original page channel's EOF. A terminal page alone does not release the
barrier. A bounded proxy separates native work from application consumers. On
consumer loss, the owner requests bounded cancellation and keeps draining the
original channel; pre-cursor abandonment retains execute until it can dispose of
its eventual cursor. HTTP draining retains its own deadline after caller loss.
Native failure is recorded before admission can pass to recovery. Missing terminal
pages and owner loss fence reuse rather than inventing successful completion.

Begin publishes metadata under the barrier and a live source borrow. If its
source disappears or policy changes before publication, its owner rolls the late
handle back. Reconnect waits for queries, refuses active transactions, rechecks
authority and replaces execution identity so queued old-source work cannot run.
Connection/session close removes the source before collecting transactions,
cancels cursor delivery, then waits for native leases before cleanup. Cleanup
avoids a second terminal dispatch for handles already consumed by end owners.
Transaction mutations and HTTP begin audit retain independent ownership when
observers disconnect or expire. A wedged native task may retain its leases;
bounded observers and admission preserve server responsiveness, without claiming
that native work was rolled back or forcibly stopped.

Dispatched native terminal errors fence the connection and keep ending state;
there is no speculative retry. RPC begin/commit/rollback failures, including a
malformed begin response, stop the provider generation and report an uncertain
outcome. Existing v1 success payloads remain compatible. Transport deadlines now
cover writer admission/frame writes and responses together. A partial or timed-out
write kills the generation; cancel and shutdown writes also have bounded grace.
A provider that stops reading stdin cannot prevent termination through a blocked
writer mutex.

Behavioral tests exercise terminal-page versus native EOF, end versus queued
queries, parallel nontransactional queries versus begin, pre-cursor observer loss,
late begin versus source close, failure ordering, missing terminal fencing and
independent native ID allocators. The existing executable corpus additionally
checks uncertain begin/end responses; the supervisor corpus exercises full pipes
and blocked shutdown. Native SQLite acceptance remains part of the workspace
suite. This proves host lifecycle behavior, not provider-native certification.
Broader native extension workflows and opaque cursor ID namespaces remain
separate work. Desktop terminal-command source binding is covered by ADR-154. Windows/macOS acceptance
remains deferred by the user.

## ADR-154 — Source-bound desktop transaction begin and end

Status: accepted.

Desktop savepoint commands already carried a transaction identity, but begin,
commit and rollback used unqualified commands and transaction-changed events.
A queued command could therefore operate on whichever connection was current
when the serial SDK executor reached it. A late response could also replace the
transaction display of a newly activated connection.

Every begin/end command now captures instance ID, profile ID, session, connection
and a fresh request UUID. End additionally captures transaction ID and opened-at
identity. The SDK executor checks the complete source before issuing HTTP, then
requires the expected absence/presence of the captured transaction. It refuses
stale commands without retargeting them. Begin validates the returned connection
before adopting its transaction. Commit still refuses a running query; failed
end retains its transaction instead of inferring native completion.

Control responses carry the original target. The UI settles only its matching
pending UUID on the still-active source and expected transaction. Stale success
and error responses cannot clear savepoints, unlock a newer request, or overwrite
another connection's condition. A successful begin must return its captured
connection; end success must have no transaction payload. Unknown payloads do
not establish valid settlement.

Connection activation uses a separate source-bound metadata event, followed by
ADR-152's authoritative refresh. It cannot settle a pending control on that
source. Changing source or connection identity invalidates pending ownership;
query snapshots preserve pending controls, while authoritative transaction
absence clears obsolete ownership. Vim commands and footer actions share this
path. HTTP wire shapes, Operation authorization/audit and the Driver trait are
unchanged; no UI dependencies enter shared crates.

Existing GPUI transaction tests cover captured commands, stale source fields,
transaction ID/opened-at/request-ID mismatch, late replies after source changes,
a newer request on the same transaction, query-refresh pending preservation and
failed-end retention. The existing SDK fixture covers refusal before mutation
HTTP and valid begin/commit/rollback dispatch to the captured IDs. These are
host desktop lifecycle proofs; HTTP query settlement is covered by ADR-155.
Provider-native certification, other transport observer audit/history ownership
and Windows/macOS native acceptance remain separate,
with Windows/macOS acceptance deferred by the user.

## ADR-155 — Owned HTTP query settlement and shared native shutdown lifetime

Status: accepted.

ADR-153 preserved native execute and original page-channel ownership after caller
loss, but synchronous HTTP query handlers still owned audit, history, ledger,
room retention and shutdown bookkeeping. A disconnected caller could therefore
leave accepted native work running without its terminal records. Room retention
failure could also bypass the operation audit after execution had already settled.

HTTP ExecuteQuery now reserves a bounded settlement owner before metadata
admission. A server-owned task retains the admitted request, captured metadata
actor, correlation ID and complete result workflow independently of its HTTP
observer. There are at most 256 such owners, separately bounded from native driver
tasks. Saturation refuses the request before dispatch. The existing execute/drain
deadline remains in force after disconnect; caller loss does not extend it.

The observed native result is audited once before auxiliary work can fail.
Ledger/history settlement precedes room result retention, and inactive-room
cleanup runs even if that retention task fails. Response serialization or byte
quota failure cannot erase or rewrite the already-observed execution outcome.
History retains the existing store_sql policy; operation audit and change-ledger
SQL remain fingerprinted. Metadata writer failure retains its existing logged/
bounded-writer semantics; this is not a new atomic durable-ledger guarantee.

Timeout, cancellation, connection loss/invalidation and internal driver failure
leave write outcomes partial in the change ledger rather than implying that no
write applied. Ordinary native statement errors remain failed observations.
Late native completion never rewrites the earlier observation. Neither a timeout
nor loss of a response proves rollback.

One shared shutdown guard spans the HTTP settlement owner and native execution.
A task-local carries its reference on the current task, and both the application
drainer and independent native worker capture it explicitly before spawning.
The count remains one logical query. It cannot reach zero merely because a
terminal page was observed, a result was returned, metadata settled, or an HTTP
deadline expired: the original native channel must also reach EOF. A wedged driver
may retain that guard; the configured shutdown deadline still bounds draining.

The existing shutdown/HTTP fixtures exercise caller loss before cursor publication,
success, native statement failure, timeout followed by late native completion,
and response-quota failure after successful execution. They check original actor
and correlation, private SQL exclusion, one terminal audit/history/ledger record,
and shutdown tracking through terminal page versus raw EOF. Admission saturation
is checked before dispatch and capacity is reusable. Driver signatures, public
protocol shapes and Operation variants remain unchanged.

This evidence covers synchronous HTTP ExecuteQuery. ADR-156 adds WebSocket
settlement and refines uncertainty to OutcomeUnknown. ADR-157 adds export terminal
observation ownership. ADR-158 binds HTTP source attribution; other transports and
mutation attribution remain separate work. Provider-native behavior and OS
isolation require separate certification. Native Windows/macOS acceptance remains
deferred by the user.

## ADR-156 — WebSocket terminal query settlement and private diagnostics

Status: accepted.

Accepted session-WebSocket queries previously had no typed ExecuteQuery audit.
Socket/ack errors returned before history and change-ledger settlement, and a
silent native producer could hide a closed socket because only page input was
polled. Shutdown admission also ended with the transport rather than native EOF.

Each accepted stream now reserves the same bounded settlement-owner admission as
HTTP. Its upgraded connection task owns terminal bookkeeping independently of the
network peer. It captures actor/correlation, validates metadata and source context,
then shares one shutdown reference into native dispatch. Audit/history/ledger
settlement and original native channel EOF must both finish before that logical
query leaves shutdown admission. A terminal page, canceled observer, or network
failure alone cannot prove native ownership has ended.

A shared query-operation audit records once, including a synchronous exceptional-
drop fallback with the captured correlation restored. HTTP also uses that owner;
metadata-free HTTP requests retain a known session owner when available. Explicit
WebSocket cancel requests use their own typed CancelQuery audit, including target
refusal, observer expiry and exceptional drop. Current cancel authorization is
retained, target mismatch is refused before native dispatch, and cancellation
observation is bounded to two seconds. Safety cleanup uses the existing bounded
server cleanup path. Losing an acknowledgment never proves cancellation applied.

The stream distinguishes a native terminal observation from delivery success.
Done/Error is captured before pacing, byte quota, or sends. A subsequent network
or quota failure cannot replace a known native result. Missing terminal output
never means success. While output/progress is pending, the task also reads socket
close/cancel controls; ack mismatch, silent-producer disconnect and cancellation
all reach terminal settlement. Legacy pages and explicit execution-event output
retain their wire contract. Ordinary streams now retain private history as well
as variable-template runs, with observed row counts and native source attribution.

Uncertain write observations use the existing OutcomeUnknown ledger state:
observation loss, cancellation without a terminal observation, timeout, connection
loss/invalidation, internal supervision/driver failure and cursor/resource/result
loss. This refines ADR-155's
partial classification. Completed statements in an open transaction remain
Partial; completed autocommit statements remain Committed. Failed records denote
failed observations, not a rollback guarantee. Late completion does not rewrite
the prior record or replay work.

Native diagnostic text can echo SQL or binds. Query audit and history therefore
retain stable codes and generic error messages, while authorized wire responses
keep their native diagnostics. Transport-close logging also avoids copying
untrusted diagnostic text. SQL remains fingerprinted in operation/ledger records,
and the existing store_sql setting controls query/template history. No bind values
enter those records. Operation replay also omits result transforms, whose filters
can contain private user values. This applies to both HTTP and WebSocket query failures.

Existing real-socket fixtures exercise normal and event-mode completion, native
error, silent cancellation, cancel timeout, wrong target, silent close, malformed
acknowledgment, buffer quota loss, known terminal result followed by delivery
quota refusal, and pre-cursor close. They check one terminal query audit, separate cancel audit,
actor/correlation, managed profile attribution, SQL/bind/echo privacy and shutdown
tracking through raw EOF. Pre-cursor loss settles before the blocked driver
returns, including with observation timeouts disabled. A separate drop fixture
verifies original correlation
rather than a later task-local scope. HTTP ownership/privacy acceptance remains
part of the workspace suite.

Public protocol shapes, Operation variants and Driver signatures are unchanged.
This proves host query observation behavior, not native provider certification or
OS isolation. ADR-157 adds export terminal observation ownership. Broader metadata
source attribution for HTTP is covered by ADR-158; other transport/mutation
attribution remains separate work. Windows/macOS native acceptance remains
deferred by the user.

## ADR-157 — Export terminal observation audit and native EOF ownership

Status: accepted.

An export previously recorded successful ExportQuery audit when its native cursor
opened, before encoding or HTTP-body production. The body alone retained shutdown
admission, so abandoning it could hide native work that still held database state.
Most encoders also accepted a page channel closing without Done and could append
apparently successful JSON/HTML trailers or an XLSX file.

The HTTP startup future now captures one typed audit owner before native dispatch,
using the authenticated principal or known session owner and the original request
correlation. Startup errors record failure; caller loss synchronously records an
uncertain failed observation even before cursor publication. Startup and the body
share the existing bounded query-settlement admission across HTTP, WebSocket and
export requests. The body owns that permit and audit through encoding and pacing;
an unpolled or abandoned body still drops the audit owner. No detached task drains
an unwanted response merely to produce bookkeeping.

Export success means the encoder reached normal EOF and every produced chunk
passed server quota/pacing admission. It does not prove that a client received or
saved the download. Native errors, encoder errors, missing terminal output,
resource/pacing refusal, server-drain interruption and observer loss are failed
export observations. A known native Done cannot make failed encoding or delivery
an export success; a failed export does not prove SQL rollback. Errors yielded
from the HTTP body can occur after successful HTTP headers, as with the previous
streaming interface. Native error objects survive inside internal I/O errors so
audit retains stable codes without parsing untrusted diagnostics. Audit messages
are generic; authorized wire errors preserve their native details.

All export encoders require Done. Early close cannot append successful trailers
or finish an XLSX/Parquet artifact. A shared logical shutdown reference passes
explicitly into the independently owned native worker and stays there through
original raw page-channel EOF. Body observation and native lifetime must both end
before that logical query drains; terminal delivery or observer cancellation alone
cannot release native ownership, connection/transaction fences or native quotas.
The body also observes server drain while awaiting silent output, independently
of whether byte pacing is configured. Existing cursor cleanup remains responsible
for cancellation and discarding unwanted output.

Acceptance extends existing export/shutdown fixtures for normal completion,
invalid-connection startup refusal, startup caller loss with no observation timeout, timeout followed
by late native return, unpolled body, row delivery followed by body loss, native
error echo privacy, missing terminal output, encoder refusal and drain. Separate
encoder acceptance checks all nine formats; the existing pacing fixture also
checks quota and drain refusal. Audits retain original actor/correlation once,
never private SQL, binds, rows or echoed native diagnostics. Native shutdown
tracking remains after observation settlement until raw EOF. Workspace validation
is recorded in the export ownership plan.

Public wire shapes and locked Driver signatures remain unchanged. Export-specific
query-history/change-ledger source attribution, provider-native certification and
OS isolation remain separate requirements. Native Windows/macOS acceptance remains
deferred by the user. This decision establishes host observation semantics rather
than durable client delivery or database rollback guarantees.

## ADR-158 — HTTP query attribution from captured execution sources

Status: accepted.

HTTP ExecuteQuery previously enabled durable history/change attribution only when
room/profile hints were present. A profile hint was checked for visibility but
not tied to the native connection, allowing an accessible unrelated profile or
tenant to label another connection's work. A source-only request skipped versioned
source validation. Re-reading a vault profile also described edited configuration
rather than the input used to open a still-live connection.

Direct HTTP queries now capture their session connection before asynchronous
metadata preparation. Managed principal/tenant/profile and configured database
context derive from that entry. A profile hint is an assertion: it must match the
captured managed profile. Ad-hoc connections cannot claim managed provenance;
they retain owner-private history and unscoped change observations when metadata
is available. Metadata-free ordinary direct execution remains supported, while
room/profile/versioned-source claims require metadata. Managed queries receive
history and change observations even when no hints are supplied.

The captured source passes into HTTP execution. Admission checks its generation
against the current connection, and the existing native worker checks again under
the connection gate. A reconnect/close during preparation cannot silently select
a new native source. Current policy and transaction checks remain at execution;
capturing a source is not an authorization bypass. Configured database context
identifies connection input, not every database/object accessible to arbitrary SQL
or the current database after session-changing SQL.

Versioned execution is validated whenever supplied. Its workspace tenant must
match proven managed connection provenance. Commit inference now orders actual
repository columns deterministically and only links commits for the requested
workspace revision; a newer workspace cannot inherit an older checkpoint/author
merely because its commit was most recent. The first valid source-only fixture
exposed the previous query's reference to a nonexistent repository_commit.id. A room-routed query additionally
requires the same workspace room. A room profile assertion must match the server
binding; caller-session transaction references are refused because room execution
uses a different hidden session/connection. Room execution uses the shared stream
preparation path, including requested result transforms.

Shared-room cache reuse checks binder, tenant, profile, provider and captured
profile configuration. Changed cached sources reopen. Current room binding is
checked under the room query gate; if a newly opened connection differs from the
prepared configuration, execution is refused. The selected room entry passes
through the same native generation checks, so stale cached or reopened sources
cannot produce successful observations under a different configuration. No atomic
metadata/remote-database transaction or continuing authority guarantee after native
dispatch is claimed.

The existing typed query audit owner now starts before preparation and settlement
admission. Refused claims, metadata failures and preparation observer loss retain
one failed query audit with original principal/correlation and private diagnostics,
without query history/change rows or native dispatch. Owned HTTP settlement and
raw-EOF shutdown lifetime remain as established in ADR-155/156.

Acceptance extends existing fixtures with managed execution without hints,
matching/unrelated/cross-tenant profile assertions, ad-hoc claims, source-only
missing/stale/cross-tenant references, same-tenant wrong-room references, room
profile/transaction refusal, metadata-free claims, profile configuration edits,
room cache rebind/configuration edits and reconnect before dispatch. The same
actor owns both tested tenants, so cross-tenant refusal proves source binding
rather than incidental visibility denial. Workspace validation is recorded in the
HTTP source-attribution plan.

Public protocol shapes and locked Driver signatures remain unchanged. ADR-159
adds captured WebSocket attribution. Other mutation/export metadata attribution
and native provider certification remain separate requirements. Windows/macOS
native acceptance remains deferred.

## ADR-159 — Captured WebSocket sources and pre-dispatch annotation limits

Status: accepted.

WebSocket query metadata previously came from a separate managed-scope/profile
lookup, while stream execution later looked up its connection again. Profile edits
could describe a still-live native connection using new configuration. An ad-hoc
connection could also attach an accessible workspace reference without proven
managed tenant provenance. Deliberate preparation errors fell through the audit's
exceptional observer-loss fallback.

Each admitted socket Execute now authorizes and captures one connection entry.
Variable-template validation uses that entry's semantic engine. Metadata scope
derives managed principal/tenant/profile and configured database context from the
captured entry. An ad-hoc query remains valid without managed/workspace claims.
Stream execution receives the captured source; admission and the native connection
gate reject generation replacement before dispatch, as with HTTP in ADR-158.
Policy and transaction checks still run at execution. Configured target identifies
connection input, not SQL object access or current database after session-changing
SQL.

Versioned references require metadata and proven managed tenant provenance. The
workspace reference is validated for current access/revision/checkpoint/Git
observations, and its room tenant must match the captured source. Visibility in
both tenants does not permit cross-tenant attribution. Variable/source preparation
refusals explicitly record one failed typed ExecuteQuery audit with original
principal/correlation and private diagnostics, before native dispatch and without
history/change observations. Exceptional observer loss retains the existing
uncertain fallback. Failures after prepared source admission keep ADR-156's
terminal settlement behavior and uncertainty classification.

The shared metadata validator now enforces the ledger's existing nonempty,
100-byte source-workflow limit before execution. Previously an empty or oversized
annotation could apply SQL and only then fail ledger insertion. The optional
workflow default and valid labels remain accepted. HTTP and WebSocket paths share
this check rather than duplicating limits or parsing native diagnostics.

Acceptance extends real-socket fixtures for valid managed and unannotated ad-hoc
execution; accessible cross-tenant, stale/missing, ad-hoc and metadata-free source
claims; empty/oversized workflows; and mismatched variable descriptors. Refusals
must not invoke native execute, publish Started/rows or create history/change rows.
One audit retains original actor/correlation without SQL/bind/template values.
The eleven existing socket settlement cases now edit the vault profile after
opening the native connection and verify the original configured target. The
reconnect fixture covers captured HTTP and stream source refusal. HTTP and metadata
fixtures also cover shared annotation limits. Workspace evidence is recorded in
the WebSocket source-attribution plan.

Public wire shapes, Operation variants and locked Driver signatures remain
unchanged. Owned terminal audit/history/ledger and raw native EOF lifetime remain
as established in ADR-156. Other mutation/export attribution and provider-native
certification remain separate. Windows/macOS native acceptance remains deferred.


## ADR-160: Bounded portable text expressions for upload imports

**Status:** Accepted

CSV, XLSX and Parquet upload-to-table recipes opt into format version 3 for
ordered `column_expressions`. Shared core code parses, resolves and evaluates a
small deterministic expression tree; it never executes SQL or external code.
Version 1/2 remains compatible. Expressions cannot be combined with rename or
projection options. Existing Vim JSON recipe editing exposes the new contract.

Bare references retain source types for Parquet and CSV inference for CSV/XLSX.
All computed expressions produce nullable text. Internal computed-column metadata
forces CSV preparation to retain text inference, including leading zeros, without
changing public wire contracts or the locked Driver trait. Destination types
remain authoritative. Durable CSV checkpoints bind this metadata when present,
while preserving legacy checkpoint digests when absent. They also bind original
upload bytes, including dropped fields.

Text operations preserve NULL and empty-text distinctions. CSV/XLSX v3 uses one
explicit bounded null marker for source decoding and native import, default NULL;
computed text equal to that marker is refused before writes. Parquet retains typed
NULL versus text directly. Unicode substring uses scalar indices. Binary values
require bare references. Depth, node, cell, copied-text and transformed-output
budgets bound evaluation and expansion. Parquet reads only referenced roots and
retains row counts for constant-only expressions, with separate source/output
bounds and schema-metadata preflight.

Preview, create-table inference, native apply, quarantine and checkpoint replay
share expression outputs. Acceptance covers Unicode, null/empty/literal marker
semantics, numeric-looking text, typed references, unknown/ambiguous sources,
malformed grammar and bounded expansion. PostgreSQL, SQL Server and SQLite use
the existing provider-specific transfer paths. Numeric expressions, non-CSV visual
review grids and wider sources remain open; Windows/macOS acceptance is deferred.
See the transfer-text-expressions plan for validation evidence.


## ADR-161: Exact decimal arithmetic retains computed transfer text types

**Status:** Accepted

Recipe-v3 expressions add explicit decimal conversion and bounded exact arithmetic
without changing the computed-text type contract from ADR-160. Unary decimal,
negate and absolute value take one input; binary add/subtract/multiply take two.
Division and rounding require fractional scale 0–18 and use ties away from zero.
Results normalize negative zero and redundant leading/fractional zeros.

Operands accept signed base-10 text or native signed integers, bounded to 128 input
bytes, 38 normalized coefficient digits and scale 18. Implicit float conversion,
whitespace, exponent notation, empty text, booleans and binary are refused. NULL
propagates. Exact results outside precision/scale limits and division by zero fail
before native write work. Shared node/copy/output budgets remain authoritative.
Bounded BigInt intermediates avoid machine-integer overflow when a mathematically
representable result needs wider temporary coefficients; operands and scales cap
intermediate sizes independently of upload size. Error messages omit operands.

Shared evaluation feeds CSV/XLSX preparation and typed Parquet transformation,
with nullable text outputs for all computed expressions. Existing checkpoint
identity, provider destination validation and native type conversion remain in
place. The Vim recipe editor retains numeric operations and scale explicitly.
Acceptance covers exact decimal addition, signed ties, integer limits, malformed
operands, NULL, overflow, native decimal destinations, quarantine and checkpoint
replay across bundled formats and all three engines. The decimal-expression plan
records validation evidence. Non-CSV review grids and wider sources remain open;
native Windows/macOS acceptance is deferred.


SQL Server native acceptance also exposed lossy schema typing in checkpointed
imports. Both deep schema paths now preserve decimal/numeric precision and scale
in the existing Native Numeric type, and preserve money/smallmoney names. This
prevents resumed/quarantined insert casts from silently rounding through a bare
decimal type. Invalid/missing native modifiers fail schema inspection. Query-result
primitive rendering and Driver signatures are unchanged. Existing checkpoints
with the old lossy target-type digest safely refuse replay under the corrected
native definition; an operator must use a new run identity after reconciling any
previously stored values.


## ADR-162: Native ACL provenance accompanies effective PostgreSQL checks

**Status:** Accepted

The selected-role privilege matrix adds an optional per-object ACL evidence
record across its existing database/schema/table/column/sequence/routine scope.
PostgreSQL aclexplode supplies grantor/grantee identities, privilege and grant
option; NULL ACLs use acldefault and retain a defaulted flag. Stored empty ACLs
remain empty. Owner identity and unknown role names are explicit. PUBLIC is
identified by grantee OID zero, independently of names.

A native subject-grantee access flag checks PUBLIC/direct identity or pg_has_role
USAGE, including native superuser semantics. It does not reconstruct membership
paths, grant-chain dependencies or executable-query authority. Existing native
has_*_privilege checks stay authoritative, including table-to-column inheritance,
owner grant options, PUBLIC and effective membership. RLS/login/view/dependency
warnings remain. No SET ROLE or grant mutation occurs.

Provenance is extracted after bounded actor-visible object selection. At most 65
native ACL items are expanded and 64 sorted grant rows are returned per object;
truncation is explicit. Empty arrays bypass aclexplode to avoid invalid array
dimensions while retaining their empty/default distinction. The existing object
page ceiling, native supervision and 1 MiB response limit stay in force. Missing optional provenance from an older
server renders unavailable, rather than becoming an empty or default ACL.

The Vim matrix toggles provenance with g, keeps j/k object selection, and uses J/K
for virtualized grant rows. Default/stored/empty/unavailable/truncated states,
owner and native grant edges stay tied to the same source/request response as
privilege checks. Native role names are escaped for display. Existing paging,
refresh, source-change refusal and late-reply rules apply.

Acceptance extends the disposable PostgreSQL membership/matrix fixture for native
delegated grants, PUBLIC, inherited versus SET-only membership, NULL/default and
empty ACLs, table/column distinctions, bounded output, revocation and actor
visibility. GPUI acceptance covers source-bound navigation and older responses.
The provenance plan records validation evidence. Object grant/revoke editors,
explicit membership/grant-chain analysis and additional families remain open;
native Windows/macOS acceptance remains deferred. Public protocol stays pure
serde and locked Driver signatures are unchanged.


## ADR-163: Native object-privilege reviews bind exact ACL transitions

**Status:** Accepted

PostgreSQL object administration adds one typed change_object_privilege action for
the matrix's connected database/schema/table/column/sequence/routine families.
The client supplies native kind/OID/column number, one privilege, a typed role or
PUBLIC grantee, and grant/revoke/grant-option intent. The server resolves names
and routine signatures. Native has_*_privilege grant-option checks remain
mandatory. Acting with owner privileges selects that owner as grantor; otherwise
one accessible native grant-option holder is required. Ambiguous grantor paths
require an explicit user SET ROLE outside the editor and a fresh review.

PUBLIC is a distinct fieldless typed variant with strict parsing. A quoted role
named PUBLIC is valid PostgreSQL and remains a literal role in the form; an
explicit u toggle selects the all-role grantee. PUBLIC grant options are refused.
No-op checks use the selected native grantor's direct edge and, for table revoke,
corresponding column edges. Effective access is not reconstructed or presented as
a guarantee of removal. Revoke uses RESTRICT, preserving dependent grants on
failure. Cascading review and additional privilege families remain open.

Review binds session/connection and native backend/database/actor/session-user,
object/schema/owner/grantee identities, relevant ACLs and column metadata. Native
state is capped at 128 KiB. Public review displays ordinary SQL and identity
observations. Apply prepares fresh state and compares the fingerprint, then one
native DO statement rechecks that state, executes the reviewed SQL and verifies
identity plus the complete expected grant-edge/column transition. Unexpected
changes or warning-only no-ops raise and roll back the statement. Bodies use
escaped E string literals, and parameter substitution is one-pass, preserving
quoted names containing dollar parameters, quotes and backslashes. There is no
fixed dollar delimiter or client-provided SQL fragment. Native external function
side effects are outside transactional rollback guarantees.

New privilege preview/apply HTTP workflows use bounded owned supervision with the
original principal/correlation, logical shutdown guard and exactly one terminal
typed audit after observer loss. Existing driver supervision, timeout/cancel,
current policy checks and production confirmation remain. This does not graduate
other administrative workflows' ownership or mutation-attribution work.

Vim a opens a frozen target from the matrix; m selects intent, u selects PUBLIC,
p requests native review and x confirms. Source/request fencing and shared
administration form validation apply. Native PostgreSQL acceptance covers all
families, grant options, PUBLIC versus the named role, delegated/ambiguous grantors,
RESTRICT dependencies, stale ACL/actor/grantee/object identities, quoted names with
standard-conforming strings disabled, and event-trigger drift/target replacement
rollback. Unit acceptance covers owned observer loss and native transition/literal
logic; GPUI covers frozen target, PUBLIC options and explicit confirmation. The
object-privilege editor plan records validation evidence. Protocol stays pure
serde, locked Driver signatures are unchanged, and native Windows/macOS
acceptance remains deferred.

## ADR-164: Serial benchmark suites retain separate parameter-case populations

Status: Accepted

Named queries and explicit named parameter cases form a bounded suite. There is
no implicit Cartesian expansion. Flatten query/case order and execute each case's
warmups then measured attempts on the existing dedicated benchmark connection.
One registration, total deadline, resource admission, cancellation identity and
owned native cleanup cover the entire suite. All queries require BenchmarkSuite
and ordinary ExecuteQuery policy before setup; the current query is reauthorized
before each attempt, and progress reauthorizes every frozen query.

Admission limits are 32 cases, 256 binds per case, 1 MiB expanded SQL, 256 KiB
serialized bind data and 4,096 total attempts. Query/case IDs use a bounded ASCII
identifier grammar. Whole-workload confirmation is mandatory. Existing classifier,
native read-only transaction where available, SQL Server read-only-login warning,
stream drain and failed-attempt stopping rules remain authoritative. No Driver
signature changes, cache clearing, write benchmark or automatic schema experiment
is introduced.

Each case has an independent run identity and report. Raw bind values stay in
requests. Reports retain bounded suite lineage, actual attempt order and shared
completion state through private saves and JSON artifacts. Save normalization
validates the trace against the serial schedule and case sample count. Lineage
is caller-supplied evidence, never a native attestation. Incomplete suites and
different suite query/case IDs withhold numeric comparisons; no suite aggregate
mixes latency populations. Shared session/cache state, data changes, unknown bind
equivalence and load remain explicit limitations.

The distinct audited API/SDK operation reuses benchmark progress/cancel routes.
HTTP suite ownership shares the bounded audit owner already used for PostgreSQL
privilege editing: capture original principal/correlation and shutdown guard,
retain cleanup and exactly one terminal audit after observer loss, and request
suite cancellation when its observer disappears. This helper generalization does
not change other legacy benchmark workflow ownership claims.

The headless CLI requires explicit confirmation, supports private per-case saves
with returned ID mappings, and accepts a complete explicit private baseline map.
Resolve and verify all mappings before opening an execution session; refuse
missing/duplicate/unknown cells and relabelled saved suite identities. Comparison
is descriptive per case and cannot prove historical bind values. Native fixtures
and CLI acceptance cover execution and lineage; the suite plan records validation
results. Desktop forms and independent per-case configuration remain open, and
native Windows/macOS acceptance remains deferred.

## ADR-165: Desktop suite reviews freeze all cases and preserve separate reports

Status: Accepted

The desktop exposes ADR-164 suites through a structured Vim composer. Named query
rows own named parameter-case rows; multiline SQL and explicit typed parameter
arrays are separate fields, alongside five shared limits. Add/remove/navigation
never executes SQL. Generated IDs avoid existing draft IDs after row deletion.
The frozen review lists every query/binding, query/case counts, total planned
reads and shared budgets. Enter cannot confirm; x acknowledges the entire suite.

Draft/review bind to original server, tenant, profile, query tab, native session/
connection and connection generation. An independent editor-statement anchor
allows suite SQL to be edited without silently changing its tab/source identity.
Changed editor intent before confirmation refuses dispatch. Confirmation rebuilds
and compares the entire draft against the frozen request, checks current operation
availability and refuses hidden field changes. The desktop executor matches the
captured source against its actual open/parked native context before invoking the
existing audited API. The server retains all native policy and read-only controls.

Suites reuse bounded progress transport and benchmark cancellation with the root
run ID. Closing the composer keeps the run/report owner; reopening the original
source returns to it. Replies must match the active item/run and original source/
generation, and reports must match frozen case identities, SQL, parameter counts,
limits, complete serial trace and independent run identities. Stale, malformed or
duplicate replies cannot replace a newer report. A refused fresh composer leaves
prior reports intact and cannot overwrite an active Benchmark/Profile.

All case reports remain separately navigable, including partial/unattempted cases.
Opening a case shows only that population in Performance and labels its suite,
query/case identity and whole-suite completion. Unrelated Profile/workspace source
observations and prior case baselines are cleared. Private save/export retain suite
lineage; selected-case save names include query/case IDs. Existing explicit saved
baseline controls and ADR-164 incomplete-suite comparison refusal remain in force.
There is no pooled score, automatic cross-case baseline or result-equivalence claim.

The desktop suite plan records GPUI and workspace acceptance evidence. The native
runner remains ADR-164's three-engine implementation. Independent case budgets,
other ordering strategies and native Windows/macOS acceptance remain open.

## ADR-166: Tabular mapping review shares import converters and binds uploads

Status: Accepted

XLSX/Parquet mapping review uses a workspace/recipe-authorized, revision-bound
source-preview API and SDK. The request supplies candidate options/version,
upload bytes and explicit XLSX sheet. The endpoint invokes only bundled native
conversion and portable projection/expression code, with no database connection,
external formatter, artifact creation or retained upload. Source schema is returned
separately from mapping availability so unsupported Parquet roots or ambiguous
XLSX headers can be dropped and blank XLSX headers can be renamed.

Preview shares CSV parsing/inference and Parquet projection decoding with import.
XLSX sample values remain text before destination inference; Parquet samples keep
native scalar/null/blob types. Column labels disclose representation. Exact decimal
expressions retain the existing canonical-text semantics. Parquet source counts
come from the footer; ready mappings decode the entire selected source before
sampling. Preview does not establish destination affinity, constraints, privileges,
write success or equivalence of another upload.

Existing 64 MiB upload/expansion/materialization/expression limits remain. Preview
adds 1–2000 bounded source names, a 256 KiB source-schema budget, at most 256
outputs, twenty sample rows and a 1 MiB response ceiling. Byte-limited samples are
explicitly marked, even when no complete row fits. Non-finite JSON samples are
unavailable rather than converted to NULL. Two shared permits are acquired before JSON buffering, with a request-size budget
for the encoded upload, 1 MiB options and bounded selectors. A thirty-second
request deadline bounds intake. Decode permits admit no waiting worker queue; a thirty-second observation timeout leaves its blocking worker,
permit and shared shutdown guard owned until actual completion. Admitted previews
and recipe updates use the bounded audit owner with original principal/correlation
and exactly one terminal audit after observer loss. Audits exclude uploads, sample
cells, expression options and sheet/file names.

The Vim recipe dialog opens a direct mapping grid. Source and ordered output rows
support add/copy/drop/reorder/rename and optional per-output portable expressions.
The grid preserves other recipe options and existing expressions. Initial schema
loading grants no save approval. Candidate preview binds exact options/version,
upload digest and immutable buffer identity; save rebuilds and compares the draft
and uses existing optimistic recipe update authority. Instance/tenant/workspace,
recipe ID/revision, original editor snapshot and request UUID fence file selection,
dispatch, replies and saves. Desktop dispatch checks the original server before
uploading. Sample cells distinguish text, empty strings and actual NULL, with
explicit display limits. Save fields freeze while the receipt is pending.

Saving changes recipe settings only. It retains unrelated unsaved recipe controls,
invalidates old execution previews and never starts an import. Normal dry-run and
native write confirmation remain separate. Successful save receipts must match the
reviewed recipe/version/options and incremented revision. Scope changes and late
or duplicate replies cannot replace another draft. Shared converter/native,
bounds/worker-lifetime and GPUI acceptance evidence lives in the mapping-review
plan. Broader formats, larger sources and native Windows/macOS acceptance remain
separate work; Driver signatures and protocol purity are unchanged.


## ADR-167: Suite cases override limits within one shared deadline

Status: Accepted

Explicit benchmark suite cases optionally supply a complete BenchmarkLimits
override. Omission inherits suite defaults. Validate every effective configuration,
case budget no greater than the shared budget and total planned attempts at most
4096 before native setup. Serial schedules and progress counters use independent
warm-up and measurement counts. The original outer deadline still includes setup.
Case budgets start at first attempt and include internal delays; query execution
uses the earliest query, case or shared deadline. Between-case delays use the
previous case's delay and shared deadline. Exhaustion retains fail-fast semantics:
partial results survive, later cases remain unattempted and comparisons withhold.
One dedicated connection, cancellation identity, policy rechecks, read transaction,
owned cleanup and typed terminal audit retain the established behavior.

Portable suite context version 2 carries the bounded effective-limit vector and
shared budget. Validation reconstructs the heterogeneous serial schedule, checks
its exact observed prefix and binds each report's counts/timeouts/budget/delay to
its indexed configuration. Version 1 remains readable with an empty case-limit vector, no shared-budget
value and its original uniform schedule. Saved snapshots and artifacts retain these
unattested observations; neither labels nor limits establish bind/data/cache/load
equivalence. No samples are pooled and no implicit baseline is selected.

The Vim composer toggles a selected case override with u and exposes five fields,
initially copied from current defaults. Disabling restores inheritance. Frozen
review lists every effective configuration and shared budget; hidden case edits
invalidate confirmation. Replies must match all expected case limits, lineage,
source and identities. Existing API/SDK and explicitly confirmed CLI requests use
the same optional override. The [case-limit plan](PLANS/benchmark-case-limits.md)
records acceptance. Alternative ordering and native Windows/macOS acceptance
remain separate work; no Driver signatures or CI workflows change.


## ADR-168: Suite dispatch order is explicit and replayable

Status: Accepted

Suites default to Serial and optionally use RoundRobin or RandomizedRoundsV1
with an explicit u64 seed. Round modes dispatch one attempt per active case per
round; each case retains its own warm-ups, measurement count and ordinal. All
execution remains serial on one dedicated connection. RandomizedRoundsV1 uses
SplitMix64 wrapping increment/mix and Fisher–Yates over active indices with
multiply-high range reduction. This bounded deterministic algorithm supports
zero and maximum seeds and has stable golden traces; it is not cryptographic
or a claim of uniform experimental sampling, IID observations or controlled load.
Existing case/attempt bounds apply before native setup.

Per-case budgets remain elapsed wall time from first dispatch, including other
cases and delays. Check the selected case deadline before dispatch and bound
its query by the earliest query/case/shared deadline. Exhaustion stops the suite
at that dispatch. Partial attempts and completed case reports survive; report
list order remains flattened query/case order, independent of execution order.
Cancellation, current-policy checks, read-only protection, owned cleanup and
terminal audit keep their existing contracts. Populations are never pooled.

Suite lineage version 3 records ordering/seed, effective-limit vector and shared
budget. Reconstruct and validate the exact trace prefix and indexed sample count.
Versions 1/2 retain serial semantics and refuse ordering data. Private saving,
artifacts, text reports and Performance keep the recorded ordering. Provenance
remains caller-associated and unattested; order changes can affect comparisons.

The Vim composer cycles ordering with r and exposes a seed field for shuffled
rounds. Frozen review names the algorithm/seed, shows the complete bounded trace
and includes every effective case configuration. Hidden edits invalidate review;
replies must match ordering, trace, limits and source identity. API/SDK and the
confirmed CLI share the same optional request. The [ordering plan](PLANS/benchmark-suite-ordering.md)
records core, three-engine and GPUI evidence. Native Windows/macOS acceptance
remains deferred; no Driver signatures or CI workflows change.


## ADR-169: Desktop manifest integrity is separate from native publisher acceptance

Status: Accepted

The existing native Windows development bundle gains a Linux offline integrity
gate: exact manifest bytes must verify against an operator-selected external
Ed25519 public PEM and raw 64-byte signature. Snapshot bounded inputs, require
Ed25519 SPKI, verify before interpreting JSON and admit only schema 1's fixed
Windows x64 inventory. Unknown/ambiguous metadata, path substitution, extras,
missing files, links and special files refuse. Descriptor-rooted/no-follow
inspection streams signed lengths/hashes and detects observed in-place changes.
The OpenSSL executable must also be external to the bundle; payloads and the
source archive are never executed/extracted. Trust keys never come from artifacts.

The receipt binds manifest/public-key digests and source/target/profile claims,
counts and bytes. Native acceptance and source-build attestation remain explicitly
unperformed; recorded Authenticode/source values are publisher claims. Inspection
does not freeze future installer inputs. Native certificate/timestamp/notarization
trust, prerequisites, install/uninstall/runtime and desktop activation/rollback
retain independent gates. The existing unsigned development workflow remains
usable; the server updater's encoded signatures, embedded trust, antirollback
and activation contract are unchanged.

The [artifact matrix](DESKTOP-ARTIFACT-VALIDATION.md) distinguishes historical
Windows development receipts from current crypto/filesystem fixture evidence and
missing production gates. Disposable signing keys and synthetic bytes never count
as native executable or publisher acceptance. No CI workflow is added/enabled.
Linux/macOS distribution producers, actual publisher identities and native desktop
update integration remain open; current Windows/macOS acceptance remains deferred.
The [verification plan](PLANS/desktop-artifact-verification.md) records scope and
acceptance. No Rust/protocol/Driver interfaces or application authority change.



### Linux package-byte verification extension — 2026-11-09

The offline gate also accepts the native `sift-linux-deb-v1` outer receipt and
matching `.deb`. An independent external Ed25519 trust key authenticates exact
receipt bytes before schema parsing, then the complete artifact length/SHA-256
through pinned no-follow descriptors. Producer metadata is a signed claim;
archive extraction/execution, control/data semantics, APT/native publisher trust,
source-build attestation and runtime/update acceptance are not performed. The
Windows bundle contract remains unchanged. Twelve verifier groups and seven
native producer groups pass, including a real Debian archive fixture signed
with disposable keys. See the artifact matrix for exact commands and limits.


## ADR-170: Native Debian packages preserve sibling runtime and host ABI boundaries

Status: Accepted

Build amd64 Debian/Ubuntu desktop distributions from a clean committed checkout
and locked Cargo graph. Use Cargo's machine-reported executable paths for all six
binaries; validate native x86-64 ELF, the system GNU loader and resolved system
libraries. Reject private/Nix RPATHs and unresolved/non-system dependencies.
Generate ABI floors with dpkg-shlibdeps and include explicit dynamic Vulkan/
Wayland/XKB/XCB/Fontconfig runtime packages. Record host release and observed dependencies;
this is a host-specific package, not universal Linux or clean-machine acceptance.

All executables are siblings under /usr/lib/sift, exposed by relative /usr/bin
links. The existing app identity binds a validated desktop entry and original
icon. Package notices, clean Git source archive and content/build manifest live
under /usr/share/doc. No maintainer scripts, triggers, services, global PATH
changes or user-runtime files are installed. Stable/development package identities
conflict explicitly; dev/release-dev versions sort before the matching release.
Low-memory dev overrides retain normal UI optimization and record actual options.

Staging uses normalized modes/times and root-owned archive metadata. Source/output
checks precede publication; no-replacement file links preserve competing outputs
and the outer receipt hashes the completed package. Source/compiler/profile and
hashes are provenance claims, not third-party source-build or publisher attestation.
Production signing, native graphical/prerequisite and signed update gates remain
separate. The server updater's trust and activation contracts do not change.

The [Linux distribution plan](PLANS/linux-debian-distribution.md) records native
construction and alternate-root install/remove fixture evidence and subsequent
actual Sift artifact evidence. No Rust or Driver signatures or CI workflows change;
all Rust/Cargo descendants retain the caller's resource limits. Windows/macOS
acceptance remains deferred.

## ADR-171: Profile observations link to captured preorder node identity

Status: accepted.

Performance Plan inspection retains the captured normalized tree's preorder
indices. Bounded observations carry an optional exact node index, rather than
matching operator labels or relation names. Multiple observations can target the
same node; the truncation notice has no target. The desktop caches observations
when accepting a live or private saved Profile and resets selected node and list
scroll whenever the plan is replaced. Existing run-ID checks reject stale replies
before replacing any plan/navigation state. Finding/row click handlers also bind
the captured run ID and reject queued clicks after a different Profile replaces it.

Vim j/k and gg/G select nodes, n/N cycle distinct observed nodes, and clicking an
observation or row selects its exact node in the virtualized list. Bindings apply
only in Performance Plan outside text inputs. A wrapped detail view exposes the
selected operator/relation, native estimates/actuals, row-count delta/ratio and
available runtime counters. Counter loop/rebind semantics, instrumentation costs,
inclusive counters and unavailable data retain their existing caveats; navigation
does not establish a bottleneck or tuning recommendation.

Observation traversal visits at most 4096 preorder nodes, retains at most 32
observations including an explicit untargeted truncation notice, and keeps pending
iterators by depth instead of collecting wide sibling lists. Repeated operator
names cannot redirect links. Inspection performs no database execution or API
request; saved owner-private plans remain historical captured evidence. Protocol,
Driver signatures, authorization and audit-operation contracts do not change.

See [design and verification](PLANS/profile-node-navigation.md). Native graphical
artifact acceptance remains independent of GPUI behavior tests.

## ADR-172: SQL Server effective checks use a disposable native token

Status: accepted.

ReadSqlServerPrivileges is a distinct audited, database-wide, read-only inspection
operation. Its SDK and Vim Security matrix inspect represented database/schema/
table/view/column/sequence/SQL-routine permissions with native HAS_PERMS_BY_NAME.
True, false and NULL remain allowed, denied and unavailable; neither explicit
ACL rows nor Sift role graphs are used to reconstruct effective rights.

The existing owned inspection connection supplies supervision, quota ownership,
restricted startup replay and bounded cleanup. The editor's transaction, token
and ROWCOUNT are never changed. Inspect the fresh connected actor without a
context switch, or use native-authorized EXECUTE AS USER for an ordinary SQL
database user. Native database-user tokens exclude login/server permissions and
do not prove login admission. Other selected identity families are refused.

Require complete native database definition/security metadata authority without
effective metadata DENYs. Capture native database, actor, subject and page object
identities before the token switch, check the selected token, restore context and
refuse changed identities. Recheck source authorization after owned cleanup as
well as during inspection, so a source close or policy change during cleanup
cannot publish a report. No protected rows or selected routines are executed.
Pages observe changing native catalogs and permissions, not a stable snapshot;
grant options, grant chains, module/ownership-chain execution and external rights
are not inferred. Native SQL Server remains authority for each represented check.

Each page retains at most 100 objects, with a sentinel, bounded offset and 1 MiB
JSON limit. The pure-serde wire types retain native NULLs. Vim subject selection,
paging, navigation, reload and replies bind captured instance/tenant/profile/
session/connection/request identity; native object labels quote components
and escape controls. Subject-field submission owns its event subscription and
rechecks the current input entity, so a discarded field cannot submit a new form.
No Driver signature, secret backend or CI workflow changes.
The [design and native evidence](PLANS/sqlserver-effective-privileges.md) records
scope, quoted-column behavior and acceptance. Windows/macOS acceptance stays
deferred; native database-token inspection is not desktop platform acceptance.

## ADR-173: Nested extension forms project bounded host-owned data

Status: accepted.

The trusted desktop form renderer projects explicit object properties into at
most 16 controls across four nested object levels. Object presence is an explicit
true/false control; required groups start included. Required children apply only
inside included groups, and entered descendants of omitted groups are refused.
Property-path components preserve literal dots, slashes and tildes. Required empty
strings remain strings; optional blank leaves are omitted. No schema defaults are
applied. JSON arrays use one host text control, preserve signed/unsigned 64-bit
integer tokens, and reject integers that would otherwise round through f64.

Raw aggregate and encoded arguments each have a 64 KiB limit. Password/writeOnly
annotations inherit through containers; aggregate fields with sensitive schema
annotations, references or inconclusive bounded inspection are masked. No schema
reference is loaded. Unsupported property shapes fail closed, including keyboard
invocation. Ordinary correction of a field validation error can retry normally.

Vim input traversal reveals controls in a bounded scroll region. Approval review
and Run share argument construction and byte/schema availability checks. Approved
requests remain frozen to the complete typed operation, arguments and context.
The server remains authoritative for full JSON Schema validation, authorization,
classification, supervised execution, one-use approval consumption and audit.
No protocol or Driver signature, dependency, secret storage or CI change is needed.
See [design and verification](PLANS/nested-extension-forms.md).


## ADR-174: SQLite column removal uses native data loss with explicit review

Status: accepted.

A typed DropColumn diagram intent derives an ephemeral target only from a
verified ordinary main-table declaration, preserving surviving identities/order
and compressing ordinals. At least one column must remain. The complete
unfiltered main/TEMP simple-catalog proof permits definitive absence despite the
known dependency-parser coverage marker; other partial, filtered, truncated,
stale or unparsed captures retain unknown missing objects. Dependency-bearing
and unsupported native shapes remain refused.

Migration rendering proves both complete declarations, unchanged surviving
column facets and order, and the exact induced ordinal shifts. Table or ordinal
companion selection requires all associated removals; column-only selection can
remove a subset. Mixed rename/add/type/nullability/reorder changes are refused.
The deterministic quoted native DROP COLUMN statements require DataLoss
acknowledgement and a transaction. Native validation rolls back its transaction;
committed removed cells cannot be recreated by adding a column, so no inverse
SQL is generated and backup recovery is disclosed. The migration executor checks
cancellation again after the final statement and before dispatching COMMIT,
rolling back an observed cancellation at that safe boundary. Existing execution
supervision, live revision, policy, production confirmation, one-use plan and audit
stay in force.

Vim designer removal is preview-only until exact DDL review, a plan-identity-bound
explicit data-loss acknowledgement and Apply. Pending and source/session/request
checks cover both preview and apply; a replaced plan cannot inherit the old
acknowledgement. No Driver signature, dependency, secret store or CI change.
See [design and evidence](PLANS/sqlite-drop-columns.md). Native Windows/macOS
acceptance remains deferred.


## ADR-175: Migration apply retains captured source and terminal settlement

Status: accepted.

HTTP ApplyMigration captures the native connection entry before asynchronous
metadata work. Actor, tenant, profile and configured database target come from
that entry. A versioned workspace source must be valid for the initiating actor
and the captured managed tenant, even when that actor owns both tenants.
Editing a vault profile cannot relabel an existing native connection. A configured
target describes connection input, not every object accessed by arbitrary SQL.

Admission, transaction begin and statement execution validate the captured
connection generation and carry the actual captured entry into native work.
The existing plan/policy/live-revision/production/one-use guards and independent
AI human-review dispatch guard remain. Preparation and admission refusals are
failed typed audits and produce no migration change rows; they do not consume a
valid plan before apply admission.

One bounded task owns execution, ledger settlement and terminal audit after HTTP
caller loss. It uses the existing shared settlement semaphore and one logical
shutdown guard, propagated to native work through raw EOF. Original actor and
correlation survive task boundaries. Only Applied runs produce successful apply
audits; canceled, rolled-back, failed, partial and incomplete runs use failed
status and stable terminal codes. Diagnostics remain generic and exclude native
SQL, binds, rows and echoed private messages. Known/uncertain ledger outcomes
remain distinct; missing native row counts or checked-sum overflow stay unknown.

Post-native source closure cannot discard a run merely because catalog refresh
is no longer available. Terminal in-memory observations remain usable for ledger
settlement when durable receipt persistence fails. Recovery of that observation
requires this invocation to have won the atomic plan claim; a concurrent retry
cannot settle or duplicate the original request's run. Active cancellation
bookkeeping is removed even on that persistence failure. Durable storage and database commit
are not atomic, and no client delivery or rollback of uncertain effects is
asserted. Other grid/bulk/transfer and export attribution remain separate work.
No wire, locked Driver, dependency or CI changes. See [design and verification](PLANS/migration-source-settlement.md).
Native Windows/macOS acceptance remains deferred.


## ADR-176: Grid apply retains its native source and owns terminal settlement

Status: accepted.

HTTP ApplyEdits captures the authorized native entry before asynchronous metadata
work and rejects path/body connection mismatches before preparation. Actor,
tenant, profile and configured database target derive from that entry; editing a
saved profile cannot relabel an already-open connection. Authentication failures
never fall back to unscoped managed attribution. Metadata-free personal mode
remains supported.

Schema planning, transaction begin and parameterized statements retain the
captured entry and reject a replaced generation. Current authorization, object
policy, optimistic row identity and the independent AI reviewed-plan/dispatch
guards remain. One bounded owner retains the original actor/correlation,
per-kind ledger observations and terminal typed audit after HTTP caller loss.
It shares the existing settlement semaphore and logical shutdown lifetime through
native EOF. Preparation refusals produce no change rows; later unattempted kinds
cannot inherit an earlier statement's failure.

UPDATE/DELETE require a known native count of exactly one. A missing count is
unavailable, never a fabricated zero-row conflict. INSERT may derive its count
from returned rows. Confirmed owned rollback is recorded as RolledBack; failed
rollback fences the original generation and remains OutcomeUnknown. An uncertain
COMMIT never claims rollback. Caller-owned transactions are neither committed nor
rolled back by grid apply: successful writes remain Partial pending caller action,
and unknown native counts remain unknown. Ledger transaction IDs refer to the
actual transaction, including server-owned transactions. Known optimistic
conflicts remain Conflicted.

Operation/ledger messages exclude row values and native diagnostic echoes.
Database commit, durable metadata and client receipt are not atomic. Bulk/CSV and
export attribution remain separate work; native Windows/macOS acceptance stays
deferred. No protocol, locked Driver, dependency or CI changes. See
[design and verification](PLANS/grid-source-settlement.md).

## ADR-177: Bulk insert owns captured-source execution and terminal settlement

Status: accepted.

HTTP BulkInsert captures the authorized native source before asynchronous metadata
work. Actor, tenant, profile and configured database target derive from that entry,
so a saved-profile edit cannot relabel an already-open native connection. Managed
authorization errors never fall back to unscoped attribution. Typed preparation
and bounded-admission refusals are audited; preview and pre-write preparation
failures produce no change rows.

One bounded task owns execution and terminal ledger/audit after caller loss,
retaining original actor/correlation and one shared logical shutdown lifetime.
Nested supervised driver work and transaction tasks propagate that lifetime;
SQLite batch workers carry it through native EOF and terminal transaction control.
The shared settlement semaphore bounds abandoned observers without aborting
native mutations at an unsafe point.

Direct SQL Server bulk uses bounded exclusive connection admission, captured
source validation, current authorization and query resources. Existing caller
transactions are refused before native dispatch. The gate and source remain
owned through actual native completion after a timeout. Dispatch admission and
timeout closure share one lock: a late native write cannot appear after an
unattempted observation has settled. Uncertain native work
fences the captured generation and remains OutcomeUnknown rather than claiming
commit or rollback.

SQLite bulk retains exact preview/value/schema checks and one-use confirmation.
Schema, DDL, owned transaction begin and parameterized batches use the captured
source. Missing affected counts fail closed rather than becoming zero. Positive
rollback confirmation is RolledBack; unconfirmed rollback fences the original
source and remains unknown. An uncertain COMMIT never becomes a claimed rollback.
Ledger rows include the actual owned transaction identity. Raw data, native rows
and diagnostic echoes stay out of operation audits and ledger messages.

Database effects, durable metadata and client delivery are not atomic. CSV's
resumable, quarantine and reviewed SQL Server paths remain separate work. Native
Windows/macOS acceptance remains deferred. No wire, locked Driver, dependency or
CI changes. See [design and evidence](PLANS/bulk-source-settlement.md).

## ADR-178: CSV imports retain captured sources and phase-specific settlement

Status: accepted.

HTTP ImportCsv captures its authorized native source before asynchronous metadata
work. A request-local SessionStore clone binds ordinary connection lookup to that
entry across nested workers, including checkpoint imports and reviewed SQL Server
startup. The global store remains unbound. Current authorization still runs;
closed/replaced sources and automatic reconnect during import are refused. The
reviewed dedicated connection copies configuration/credentials under a generation
check and never rereads an edited vault profile to select its native target.

A bounded HTTP owner retains original actor/correlation, terminal typed audit and
ledger settlement after caller loss, sharing one native-EOF shutdown lifetime.
Native PostgreSQL COPY uses the exclusive, source-bound admission and atomic timeout/
dispatch closure from ADR-177. Generated statements and reviewed native statements
also close dispatch admission before an unattempted observation can settle.
Preparation, dry-run and review refusals produce no change rows. Metadata-free
personal mode remains supported; managed failures never fall back to unscoped
attribution.

PostgreSQL keeps native COPY inside a server-owned transaction, so failed COPY
and missing counts can be positively rolled back. Earlier autocommitted DDL keeps
its independent outcome and transaction identity; data receives the actual COPY
transaction identity.

Ordinary SQL Server abort imports use a server-owned transaction and parameterized
batches with a returned native row-count witness, including under NOCOUNT. Table
creation shares that transaction. This makes rollback positively observable,
rather than inferring it from the legacy native bulk method's opaque error.

Data and DDL observations are distinct. Autocommitted CREATE TABLE remains
Committed if later data import fails, without inheriting the data failure code.
SQLite reports both phases RolledBack only after confirmed owned rollback.
Known earlier row/chunk commits remain partial observations on later failure;
uncertain effects remain OutcomeUnknown. Owned transaction identities are retained.
An existing caller transaction is refused for ordinary imports; reviewed SQL
Server writes retain their independent dedicated transaction. No import commits
or rolls back the caller's transaction.

Missing COPY/affected counts never become zero or skipped rows. Checkpoint
advancement requires an observed inserted row; skip/quarantine retains fatal
infrastructure failures. Reviewed generated-column counts keep the existing
accepted-input meaning and warning, rather than claiming trigger equivalence.
Unknown COMMIT and failed rollback fence the mutated native generation; cleanup
never closes a replacement by connection ID. Dedicated SQL Server uncertainty does
not fence the editor connection or its independent caller-owned transaction.
Dedicated cleanup retains shared shutdown tracking.
Audits/ledger omit SQL, data, rejects and native diagnostic echoes. Native effects,
durable metadata and client delivery are not atomic.

The enclosing file-transfer ledger and export attribution remain separate work.
No protocol, locked Driver, dependency or CI changes. See
[design and verification](PLANS/csv-source-settlement.md). Native Windows/macOS
acceptance remains deferred.


## ADR-179: HTTP transfer recipes own captured execution and import receipts

Status: accepted.

HTTP recipe execution reserves bounded settlement capacity and captures the native
connection before asynchronous authentication, recipe lookup or format conversion.
A request-local source binding survives CSV, XLSX, Parquet, extension conversion,
checkpoint and dedicated reviewed SQL Server paths. Managed source and recipe
workspace tenants must match, even when the principal owns both. Native admission
rechecks current workspace execute authority and recipe revision. Owned rollback
may bypass that recipe guard after revocation; original connection generation and
native policy checks still apply.

One supervised owner retains original actor/correlation, exact recipe operation,
shared shutdown lifetime and terminal settlement after HTTP caller loss. The
additive pure-serde ExecuteTransferRecipe operation variant carries the known
recipe ID before workspace lookup and reuses the existing operation kind. It does
not fabricate a workspace ID for preparation failures. Export success means server
artifact production and persistence; native EOF tracking outlives early result
completion. It makes no claim about client receipt.

All attempted import formats share phase-specific database receipts, captured
configuration and actual owned transaction IDs. Receipts attach the authorized
workspace without inventing versioned query provenance. Dry runs, conversion and
preparation refusals have no database change rows. A confirmed import commit stays
Committed when later quarantine/artifact persistence fails, while the enclosing
recipe operation fails. Checkpoint DDL identifies the checkpoint table separately
from the data destination. Audits and ledger omit SQL, binds, uploads, rejects,
formatter options and native diagnostic echoes.

Ordinary Parquet imports now observe each inserted row, use owned transaction
commit/rollback tracking and fence uncertain terminal outcomes. Missing native
counts never become input-length estimates. SQL Server uses an explicit row-count
witness under NOCOUNT. Reviewed generated-column modes retain their documented
accepted-input counts.

Database effects, metadata persistence and client delivery are not atomic. Direct
streaming export attribution and arbitrary write SQL routed through exports remain
separate work. No locked Driver signature, dependency or CI changes. Native
Windows/macOS acceptance remains deferred. See
[design and verification](PLANS/recipe-source-settlement.md).


## ADR-180: Export startup ownership and native mutation receipts are independent

Status: accepted.

Direct HTTP exports reserve bounded settlement capacity and capture the authorized
native source before asynchronous startup. A supervised owner survives startup
caller loss; the existing paced body audit still records server production or
observation failure. The captured tenant supplies byte pacing. Native generation,
current policy and active-transaction checks remain enforced. An unpolled or
abandoned body cannot claim export success.

Direct and recipe exports install a request-scoped redacted observation in the
existing independent native execution worker. An atomic dispatch closure refuses
late timeout admission; only actual attempts can create change receipts. Native
observations retain all Done/error pages and raw EOF even after an encoder stops
at its first result or its observer disappears. A bounded settlement task shares
capacity and shutdown lifetime through native completion and ledger append.

Native effects are separate from encoding, artifact persistence and delivery.
Ordinary classified single DML/DDL on a known autocommit source may record Committed
after native completion and EOF, even when body production or artifact creation
fails. Native failure, missing completion and uncertain statement shapes remain
OutcomeUnknown. Counts come only from checked native affected counts; returned or
encoded rows never substitute for missing counts. Mixed/unclassifiable statements
omit aggregate counts that could include reads. No rollback, transaction ID or
query revision is invented. Recipe receipts retain their authorized workspace.

Raw opaque/control SQL makes native transaction
confidence unknown for that connection generation, including settings inside
managed transactions that can survive COMMIT. Export observations check this
at dispatch and EOF, so earlier/concurrent raw BEGIN, SET or procedure execution
cannot create a false commit claim. Confidence is restored by a freshly opened or
reconnected native source. A known statement can retain its observed count while
its commit outcome remains unknown. Structural SELECT classification does not
attest that arbitrary functions are side-effect free.

Unsupported grammar still executes under existing native policy, with conservative
receipts. In particular the bundled parser cannot classify SQL Server UPDATE OUTPUT,
and the streaming SQL Server driver supplies no affected count for that shape;
its returned rows cannot justify either inference. PostgreSQL RETURNING also
currently omits affected counts at the driver boundary; receipts retain that
absence, while plain PostgreSQL DML preserves counted positive and zero effects.
Plain classified SQL Server DML
retains the native affected count. The existing vendored TDS layer now preserves
DONE_COUNT presence in an optional checked observed total, while retaining its
legacy total/iterator APIs. The driver uses that observation: NOCOUNT stays absent,
an explicitly counted zero stays zero, and overflow is not an invented count.

Audits and ledger exclude SQL, binds, rows and native diagnostic echoes; only a SQL
fingerprint and generic codes persist. Effects, durable receipts and client delivery
are not atomic. This changes no Driver signature, protocol, dependency version or CI configuration.
Native Windows/macOS acceptance remains deferred. See
[design and verification](PLANS/export-source-settlement.md).


## ADR-181: Public SQL mutation receipts follow native EOF and transaction confidence

Status: accepted.

HTTP direct queries, HTTP bound-room queries and WebSocket queries share ADR-180's
native observation and receipt writer with exports. Native observation is explicitly
inherited by HTTP's nested drain task and captured by the independent native worker.
Room queries observe the room's server-owned source, carrying the submitter's
prepared actor/tenant/profile/configuration and room context. Versioned query
receipts preserve validated workspace/revision/path provenance. No editor source
or current vault configuration is substituted during settlement.

Result audit/history and database effects have separate lifetimes. Result observation
can finish at Done, fail during delivery, or end on timeout/cancel/socket loss;
mutation receipts wait independently for all raw native pages and EOF. A shared
bounded settlement permit and shutdown lifetime survive observer loss through
receipt append, retaining original actor/correlation. Atomic dispatch closure
prevents a late queued request from turning an unattempted refusal into a write.
Preparation/admission refusals and classified reads produce no change rows.

Dialect-aware possible-effect classification replaces the first-word mutation
heuristic, including comments, CTE writes, SELECT INTO and mixed/opaque SQL. A
completed ordinary statement on a known autocommit source can be Committed after
native EOF. The same observation in a known managed transaction is Partial with its
actual server transaction ID. Raw transaction uncertainty, unsupported/mixed shapes,
missing completion and native errors remain OutcomeUnknown. Native failure does not
prove that an attempted statement changed nothing. Opaque/control SQL can leave a
conservative possible-effect receipt. Structural read classification does not
attest arbitrary function side effects.

Mutation counts come only from checked native affected counts, preserving explicit
zero and absent/overflow counts without substituting returned rows or history
counts. Confirmed native completion can outlive failed delivery, timeout, cancel or
result retention. Conversely a late native error cannot be hidden by an earlier
observed Done. Audits/history describe result observation, not client receipt,
rollback or native commit. Extensible Other-code text stays out of persisted audit,
history and receipts through the shared redaction helper.

Receipts can settle after the public response. Database effects, metadata persistence
and delivery are not atomic; managed-transaction statement observations are not
retrospectively promoted when a later caller commit/rollback occurs. No Driver
signature, protocol, dependency version or CI changes. Native Windows/macOS
acceptance remains deferred. See [design and verification](PLANS/query-native-settlement.md).

## ADR-182: Paired result checks are bounded untimed observations

Opt-in paired Benchmark verification runs baseline then candidate once before any
warm-up or timing sample. Ordered checks compare complete decoded column names/type
refs and typed rows; unordered checks sort bounded row digests with duplicates
preserved. Canonical JSON object ordering is stable. Missing metadata/completion,
multiple results, late errors, opaque/display-only values, nonfinite floats,
unexpected typed-null output and excessive JSON structures fail closed. Completion
requires raw EOF. PostgreSQL retains the prepared descriptor explicitly so empty
extended-query results announce columns without adding another prepare. Existing
native command-tag count semantics are preserved. Canonical encoding is streamed under row/byte limits; scratch
capacity participates in tenant retained-byte accounting. Fingerprints and result
rows never enter reports, persistence or inspection projections.

Only matching fingerprints permit timed attempts. Other outcomes retain incomplete
zero-sample reports with fixed status and completed-observation counts. Checks use
the existing shared deadline, query timeouts, cancellation, owned connection cleanup
and current policy. The source generation is frozen before the pair supervisor
starts and checked while copying connection credentials/configuration and before
each dispatch. Verified PostgreSQL workloads use read-only repeatable read, SQLite
read-only serializable, and SQL Server independent reads with the existing
read-only-account warning. No cache clearing or shared settings changes occur.

Pair context v2 carries optional verification; v1 remains readable without it.
Private save/import rejects invalid limits, outcomes, snapshot policies and timed
attempts after unsuccessful verification. Complementary populations must retain the
same check context. Saved/imported measurements remain caller-supplied and
unattested. Vim review freezes the mode alongside workload/order; confirmed CLI,
API/SDK, private saves, artifacts and read-only AI/MCP inspection preserve fixed
row-free outcomes. These observations do not establish SQL semantic equivalence,
all native value precision, later timed data equality or controlled cache/load/IID
conditions. Two extra reads may warm caches and invoke external function effects;
workload confirmation covers them. No Driver signature, dependency or CI changes.
Native Windows/macOS acceptance remains deferred. See
[design and acceptance](PLANS/paired-result-verification.md).

## ADR-183: Default privileges review future creator ACLs as separate scopes

Typed PostgreSQL object preview/apply now covers explicit one-creator/recipient
future-object grants, grant options and RESTRICT revocations for tables,
sequences, routines, types and schemas. Global scope means this database; a named
schema adds to global defaults. Schemas are global-only and MAINTAIN requires 17;
reviews require PostgreSQL 16+. PUBLIC is tagged separately from a literal role.
Existing object ACLs are unchanged; role membership does not inherit another
creator's defaults for member-created objects. Ownership and other access can
remain after revocation. Native USAGE role authority matches has_privs_of_role;
no stronger account or SET ROLE is replayed to apply a change.

Bounded snapshots expand built-in global ACLs and preserve empty schema ACLs.
Creator/grantee/actor/database/schema identity, authority, server version, both
ACLs, SQL and source session/connection plus an ephemeral native generation UUID
bind review. Limits are 256 edges per scope and 128 KiB. Unsupported states,
no-ops and stale proofs are refused. Qualified catalog functions, types,
comparisons/operators and fixed C ordering preserve native lookup semantics under
custom search paths; one-pass E-literal substitution preserves quoted identifiers
with dollar parameters, backslashes and nonstandard string settings.

Native apply rechecks reviewed state, executes exact DDL, then verifies the whole
expected semantic ACL transition in one statement. Failure raises and rolls back
that statement. No future pg_default_acl row OID is invented for rows that native
DDL may create or remove. Optimistic checks do not lock concurrent creation or
attest all concurrent DDL. Success acknowledges the native statement; a caller
transaction may still need commit. Owned HTTP supervision retains captured source,
actor/correlation, shutdown lifetime and terminal typed audit after caller loss.

Source-bound Vim Roles forms expose creator/schema/recipient, family, right,
PUBLIC and all four intents, then frozen SQL/ACL preview and explicit production
confirmation. API/SDK reuse existing object endpoints and pure serde wire types.
No Driver signature, dependency or CI changes. Native Windows/macOS acceptance
remains deferred. See [design and acceptance](PLANS/postgres-default-privileges.md).


## ADR-184: External estimated plans use a distinct Driver RPC v3 family

`driver.explain.estimated@1` requires minimum Driver RPC v3 and dispatches typed
`explain_estimated`; v3 inherits v1 methods and v2 savepoints. Manifest and
development validation reject claims below their minimum. The broader
`driver.explain@1` remains unavailable to external packages because it would imply
analyzed/Profile execution. No locked Driver/PgExt/MssqlExt signature or public
HTTP protocol version changes.

Requests preserve original validated SQL and typed parameters. Responses echo the
native connection, declare reusable state and carry one dialect-matched PostgreSQL
JSON, SQL Server ShowPlan XML or SQLite EQP-row payload. Estimated-only execution,
restored settings and native provenance remain operator-trusted provider claims;
quality stays unset. Existing API/SDK, Vim and governed estimated reads return the
shared tree/raw representation with an explicit unattested warning. Unknown
semantic dialects and analyzed/Profile requests fail without fallback.

Frame/result/1 MiB byte ceilings, bounded JSON/XML depth and counts, bounded plan
trees, runtime-evidence checks and finite nonnegative normalized estimates limit
host interpretation. Malformed/uncertain responses, changed handles and RPC
failures after dispatch fence the entire tenant/package generation with fixed
non-sensitive diagnostics. Independent generations remain live. Raw plans can
contain query data and retain existing access policy.

Owned capture takes exclusive native connection admission, refuses managed
transactions and rechecks policy and captured source after its bounded wait.
Quota, gate, driver task and shutdown ownership survive observer loss/timeout
until native completion. HTTP Explain captures actor/correlation and source before
supervision and retains one terminal typed audit. A timed-out observer records
failure even if native completion later succeeds. Built-in capture algorithms stay
on their existing paths. Process conformance does not certify third-party native
semantics; Windows/macOS acceptance remains deferred. See
[design and acceptance](PLANS/external-estimated-plans.md).


## ADR-185: External notifications own a dedicated subscription and stop barrier

Driver RPC v4 inherits v1–v3 and graduates `driver.notifications@1`. Package and
development validation require minimum v4. Typed listen/stop messages preserve
exact connection, host-generated subscription and channel identities. Events are
opaque channel/payload data, so a host SQL dialect is unnecessary. Native naming
constraints remain provider-specific. Schema-restricted profiles cannot subscribe:
channel payloads have no proven schema mapping, and live scope changes terminate
subscriptions. Public protocol 5 and locked Driver/PgExt/
MssqlExt signatures stay unchanged; server-internal adapters bridge the streams.

Listen narrows initial byte credit to at most 128 KiB and framed events to 64 KiB,
bounded further by negotiated limits. Sequence/credit and typed identity/channel/
shape checks precede forwarding through bounded queues. Active subscriptions hold
concurrent-query/cursor quota and a 4 MiB retained-byte reservation through actual
cleanup. Credit writes and stop work have deadlines. Host errors/audit do not
retain notification bodies or channel labels.

Startup owns short source-bound connection admission, rechecks authority after
waiting and retains work when its observer times out or disappears. Dedicated
lifetime leases are independent of ordinary query admission: editor queries and
transaction controls remain available. Connection/session close, reconnect and
invalidating close cancel/drain listener leases before closing the base handle.
Live authority/source checks remain observable under backpressure; quiet socket
EOF/close, auth revocation and server drain trigger cleanup.

Stop requires both a reusable exact-ID acknowledgment and a terminal Stopped
stream barrier; either may arrive first. The stream stays registered while queued
frames drain under credit. A five-second cleanup deadline plus bounded kill grace
fences uncertain/malformed state across the tenant/package generation. Healthy
observer stop preserves that generation; independent processes remain live.
Unexpected remote stop fails the subscription and still requires idempotent stop
acknowledgment. No statement retry, stronger-principal replay or native readiness
attestation is introduced. Dedicated native listener scope/reusability remain
operator-trusted provider claims, with separate native certification.

Existing SDK and Vim subscription controls retain source/generation guards and
bounded history with provider-neutral labels and verbatim UTF-8 input. One terminal
Listen audit retains original actor/correlation, with shutdown and quota ownership
through cleanup/audit even after caller loss. Native Windows/macOS acceptance
remains deferred. See [design and acceptance](PLANS/external-notifications.md).


## ADR-186: Concurrent benchmarks own private workers and client observation clocks

Concurrent suite v1 extends the existing audited BenchmarkSuite operation with
1–8 private workers, a global 1–1000 workload admissions/second ceiling and explicit
stop-run or retire-worker failure policy. Each worker executes every frozen case's
warmups and measured iterations; multiplied attempts are bounded at 4096 before
native opening. Older servers reject the unknown order tag without serial fallback.
Public protocol 5 and locked Driver signatures remain unchanged.

Capture the original source generation and authority before owned supervision.
Reserve all connection slots before any worker opens, retain connection-provenance
quota, and recheck source and both BenchmarkSuite/Execute permission after waits,
opens and before dispatch. Worker handles never enter the public session connection
map. Profile defaults are used without replaying editor overrides or its active
transaction. PostgreSQL/SQLite use private read-only transactions; SQL Server
requires native read-only credentials. SQL classification is not a sandbox for
external function effects.

Every worker owns driver admission, connection quota, tunnel and shared shutdown
tracking through actual rollback/close. Query/cursor/byte ownership survives
observer loss, timeout, cancellation and late native startup through raw stream EOF
and cancellation completion. Done is not EOF: late errors fail observation. No
native owner is aborted at an observation deadline. Retired workers are never
replaced or retried; stop-run closes peer admission, while retire-worker lets healthy
workers finish their fixed workload. Warmup failure always prevents measurement.
Pending/failed/unobserved cleanup prevents a complete-suite claim.

Setup precedes a shared warmup barrier and measurement window. A minimum-spaced
shared pacer releases bookkeeping while sleeping, does not accumulate burst credit,
and preserves spacing across the phase barrier. Per-case budgets start at first
admission across workers and share the outer deadline. Setup/version/settings
reads consume the total budget but are additional to paced workload admissions.

Suite lineage v4 retains raw per-worker limits, global case order, worker IDs,
phase window, client admission/end/wait clocks and bounded worker environments.
Legacy v1–v3 remain readable. Validate case-local and complete-suite traces and
recompute descriptive metrics on inspection/import. Pool only identical frozen
SQL/parameter cases. Successful latency excludes failures and warmups; throughput,
peak concurrency and busy-worker utilization describe the client observation
window. These clocks do not attest native wire dispatch, database CPU/IO/saturation,
commit outcomes or outstanding native lifetime after a stopped observation.
Concurrent samples withhold IID median uncertainty; comparison requires complete,
matching configuration/limits and still cannot prove environment/data equivalence.

Existing API/SDK/confirmed CLI, private saves/artifacts, governed read inspection
and source-bound Vim review/progress/cancel expose the mode without adding another
execution authority. Saved/imported context remains caller-supplied and unattested.
Native Windows/macOS acceptance stays deferred. See
[design and acceptance](PLANS/benchmark-load.md).


## ADR-187: AI database apply owns preflight, receipt and terminal audit

The human AI row/migration apply endpoint reserves the shared bounded settlement
owner before asynchronous proposal/review preflight. It captures the original
actor/correlation and one shared shutdown lifetime, then owns validation, exact
single-dispatch claim, guarded native application, ledger, durable receipt and one
terminal typed AI audit after HTTP caller loss. Draining/owner-admission refusals
are audited before dispatch. Chat/run IDs discovered in preflight refine only the
audit target; action, actor and correlation remain captured.

Pin the authorized native source before native catalog/preview validation. The
bound SessionStore and existing reviewed-plan/dispatch guards retain generation,
configuration and current authority through application. Complete the claimed
apply directly within that owner rather than a nested unscoped spawn, so existing native
query/transaction/cleanup owners can capture the shared shutdown reference before
spawning. An observer
or request timeout can settle before actual native EOF; independent native owners
retain tracking and resources until they finish. Done/error is not EOF.

Audit settlement describes the durable observed application receipt. A confirmed
Applied receipt succeeds; pending, failed and outcome-unknown receipts remain
explicit unsuccessful observations without asserting rollback or a new commit.
Response authorization occurs separately afterward. Losing chat access or delivery
after native settlement cannot rewrite a known applied receipt or create another
terminal audit. An identical retry reads the existing immutable claim/receipt; it
does not execute database work or append the original ledger effects again.

Extensible native error text is excluded from AI ledger error codes and migration
statement codes in receipts and persisted runs. Those codes use the existing
stable sanitizer. Database commit, metadata persistence and client delivery remain
separate; receipt/claim recovery never implies an automatic retry of uncertain work.

The existing gated migration fixture now checks caller loss, timeout before a
cursor, extensible native errors and room revocation during commit acknowledgement.
It retains original actor/correlation, raw-EOF shutdown tracking and exact replay.
The revoked recipient cannot receive the body while an authorized peer sees the
known Applied receipt and the original successful audit remains intact. Native PostgreSQL/SQL Server/SQLite AI fixtures
cover reviewed row/migration application and existing refusal/revocation behavior.
This ownership foundation does not enable agent execution. Execute permissions,
approvals, run delegation, execution journals and headless/unattended lifecycle
remain explicit work in the [execution plan](PLANS/ai-execution.md). Public protocol,
locked Driver signatures, dependencies and CI behavior stay unchanged; native
Windows/macOS acceptance remains deferred.

## ADR-188 — Human-approved, source-bound AI SQL execution

**Status:** accepted for the interactive SQL family; broader execution remains planned.

**Context:** Read/Propose and human proposal apply do not grant an agent authority
to dispatch arbitrary SQL. Execute needs an exact human decision and a durable
outcome even when its caller, provider or result observer disappears.

**Decision:** Execute is separately opt-in and initially private to the initiating
user's managed connection. Its run pins native connection generation and an
opaque profile/configuration/policy/credential proof. The source label describes
the configured database, not every native object a statement might access.
Read/Propose keep their existing behavior. An additive execution permission
column preserves the existing run-mode constraint and encrypted lease replay. The
first committed Execute run atomically raises the minimum metadata reader to
V058 so older content inventories cannot retire execution keys or omit execution
blobs from backup. Unused Read/Propose installations keep additive rollback;
coordinated tenant restore preserves the greater reader floor and refuses an
unsupported future reader.

An immutable call ID binds SQL, typed parameters, actor, run and pinned source.
The encrypted journal reserves the shared tool/event quota in the same SQLite
transaction as its request event. Exact replay returns the existing action;
changed input refuses. The provider receives a request/status tool, never an
approval or reconciliation tool. Human approval names the exact fingerprint,
with exact database-label confirmation for production-tagged profiles. Human
denial and run termination revoke unclaimed actions. Native claims serialize
across each chat, including prior unknown outcomes.

Approval starts one bounded detached owner before its atomic claim. That owner
retains the native source, actor, correlation and shutdown lifetime through raw
EOF, ledger observation, encrypted receipt and terminal audit. Pre-dispatch
source/run/policy checks repeat after waits. Supported single reads and ordinary
DML/DDL use the normal execution path with bounded results and no active or
uncertain transaction. Opaque SQL and session/transaction controls require a
separate execution family. Arbitrary connection startup batches are refused;
typed variables and the existing SQL Server session-only SET subset remain
supported. Known native outcomes survive result observation or
delivery failure; absent results never establish rollback. Chat disclosure is
rechecked after encrypted reads. An explicit human acknowledgment permits later
actions after an unknown outcome while preserving uncertainty and never retrying
that statement.

Cleanup, content-key rotation, backups and coordinated tenant restore include
request, source and receipt handles. Active applies protect their journal from
chat deletion; restored/interrupted applies become unknown and cannot resume.
Vim review remains available while the provider waits for a human decision.

**Consequences:** The existing Driver signatures and CI dispatch policy are
unchanged. This is the first execution family, not completed unattended AI.
Typed database/document/extension actions, public-room write grants, broader
revocation/fault evidence, reviewed run delegation and headless/scheduled
lifecycle remain explicit in [the execution plan](PLANS/ai-execution.md).
Native Windows/macOS acceptance remains deferred.

## ADR-189 — Native admission evidence for human AI row apply

**Status:** accepted for human row apply; typed delegated execution remains planned.

**Context:** A requested row-edit kind does not prove a native statement was
admitted. Guarded AI apply previously discarded the transaction/rollback
observations available to direct grid apply, aggregated counts without checked
arithmetic, and described confirmed rollback as an unknown result. Typed agent
execution must not inherit those provenance gaps.

**Decision:** Allocate one source-bound row observation collector per claimed
apply. Both direct grid capture and guarded AI apply retain each statement's
normal execution-owner witness. Only that owner's native admission marks a kind
attempted; preparation, dispatch intent and queued work cannot manufacture it.
Keep those witnesses reachable when an observer deadline drops its apply future.
This retains evidence, not permission to repeat a native statement.

Retain the actual managed transaction ID and observed owned rollback from the
existing source-bound begin/end paths. A managed `TxId` is numeric and witnesses
Sift's transaction handle; it is neither an engine's internal XID nor a proposal
or migration workflow UUID. Expected-plan comparison, source generation checks
and asynchronous authority guards still gate the normal apply path.

AI change-ledger entries cover only natively attempted kinds. Each kind uses
checked conversion and addition into the ledger's signed count representation;
zero remains zero and overflow remains unavailable. An observed owned rollback
is recorded as RolledBack, a known owned commit as Committed, and otherwise
uncertain effects as OutcomeUnknown. A confirmed rollback produces a failed
human proposal receipt explicitly describing rollback. Exact replay observes the
durable receipt without another write or another original ledger entry.

Original actor/source/correlation ownership, encrypted receipts, independent
delivery authorization and sanitized diagnostic codes remain in effect. The
collector is server-internal; no Driver signature, protocol response shape, metadata
schema, dependency or CI dispatch policy changes.

**Consequences:** This supplies row evidence for future typed execution. It does
not implement that action family, unattended authority, migration-group native
witnesses or a promise that native work ended at an observer deadline. Subsequent ADR-192 supplies migration run UUID reconciliation and the native
journal producer described in the [execution plan](PLANS/ai-execution.md).
Native Windows/macOS acceptance remains deferred.

## ADR-190 — Native transaction control observations and HTTP settlement

**Status:** accepted for HTTP begin/end controls; migration integration remains planned.

**Context:** A transaction method's observer deadline can expire while its
existing worker is still awaiting BEGIN/COMMIT/ROLLBACK. Recording that timeout
as the terminal command result loses late acknowledgements and can release HTTP
audit/shutdown ownership before native settlement. Host publication/removal can
also fail after the driver has acknowledged the command.

**Decision:** Carry one server-internal observation through the existing bounded
transaction worker. Record admission, the actual managed handle, source
generation, tracked transaction-state condition and sanitized driver result.
Record acknowledgements before host bookkeeping; retain BEGIN publication and
compensating cleanup separately. Fence a source whose acknowledged end cannot be
removed from host state. No new driver execution path is introduced.

Closing admission and the fallible host end claim share one critical section.
A refusal cannot strand an unissued command in ending state. HTTP controls use
the existing bounded settlement/audit owner, capturing actor/correlation and the
shared shutdown lifetime. Observer timeout closes undispatched admission and
returns a timeout response; caller loss leaves the owner running. The owner
awaits the actual worker and can settle a late acknowledged command successfully
without a duplicate native call or audit. Late BEGIN success requires observed
host publication, not merely a driver acknowledgement.

A command acknowledgement does not prove that previous SQL committed under an
unknown raw/opaque transaction scope. Observations retain that uncertainty rather
than repairing it. Migration conversion must additionally require its captured
generation and known owned scope. Control auditing is distinct from change-ledger
proof of earlier statements.

**Consequences:** Gated tests cover observer timeout, caller loss, session removal,
closed admission, publication refusal and successful/failed cleanup. HTTP tests
retain one original actor/correlation audit and shutdown owner. Native PostgreSQL,
SQL Server and SQLite acceptance checks rollback on the original connection and
commit visibility on an independent connection. Driver signatures, wire DTOs,
metadata schema and CI dispatch policy are unchanged. Native migration journal
production and ledger conversion follow in ADR-192; the inner AI deadline
follow-up is ADR-191. Their design is in the [execution plan](PLANS/ai-execution.md).

## ADR-191 — Retain human AI apply settlement across observer deadlines

**Status:** accepted for human row/migration apply ownership; native journal production remains planned.

**Context:** An inner timeout dropped a claimed human apply's orchestration while
its native worker could still be executing. That could finalize an unknown receipt
and audit before the native outcome, or attempt rollback while a statement still
held its connection gate. Method deadlines also hid late BEGIN/end acknowledgements.
Journal storage cannot repair observations that no surviving owner records.

**Decision:** Move the AI deadline to its HTTP observer and retain the existing
bounded owner through receipt, ledger and audit settlement. Return Applying only
when that request has acquired its durable one-use claim. A preclaim timeout asks
the caller to inspect the receipt; it does not assert that work was never admitted.
Receipt delivery remains separately authorized. Exact replay never sends writes.

Carry an absolute deadline through the actual spawned native workers, atomically
checking admission with statement observations and host transaction end claims.
Caller loss does not remove that deadline. Close further statements, BEGIN and
COMMIT after expiry; owned rollback cleanup remains allowed. Scope exit also
closes admission. No read-only AI abort token is reused for mutation ownership.

Structured owners await issued control workers through terminal observations.
Late BEGIN requires host publication; late end requires native acknowledgement
under known tracked transaction scope. An unknown raw/opaque scope cannot become
owned commit/rollback proof. Cleanup may still issue rollback under that scope,
then fences the source and retains an uncertain owned result. Ordinary HTTP
control acknowledgement semantics remain as recorded in ADR-190.

Row and migration orchestration await admitted statement EOF before cleanup or
subsequent mutations. A statement delivery error remains conservative; the owner
can confirm owned rollback after the native work ends. An already-issued COMMIT
can settle successfully after the HTTP deadline without a duplicate command.
Only an owner with no native mutation admission reports changes not dispatched.

**Consequences:** This is a prerequisite for native migration journal production
and both migration ledger consumers, subsequently implemented in ADR-192. No Driver signature,
wire DTO, metadata schema, new workflow or CI dispatch policy changes. Native
Windows/macOS acceptance stays deferred.

Acceptance passed formatting, strict workspace/all-target Clippy, 8 native-control
unit tests and 128 focused HTTP tests. The combined workspace invocation passed
1,890 tests with 19 default ignored; explicit provider acceptance passed 5 unit
and 4 HTTP tests. Real PostgreSQL, SQL Server and SQLite acceptance passed 3/3.
The existing reduced-memory compiler/profile settings retained assertions and
overflow checks. Aggregate Rust/Cargo CPU and RAM stayed under the hard 10% cap;
no test stack limit was raised. Observation helpers box their parent futures to
keep the integration paths usable on their existing stacks.

## ADR-192 — Native migration journal production and common ledger settlement

**Status:** accepted for native migration journal production and direct/human AI ledger settlement.

**Context:** Migration storage and owned control observations did not themselves
record native migration execution. Direct and AI ledgers still treated a workflow
UUID and workflow status as native transaction evidence. A delivery timeout could
also hide a successful autocommit statement. BEGIN publication cleanup needs its
rollback intent inside the existing worker, before the actual cleanup call.

**Decision:** Freeze one captured connection/source into the existing version-1
journal after its parent Running receipt. Persist each group and statement intent
before any possible native call. Use the same captured entry for BEGIN and SQL,
require known tracked scope for structured BEGIN/COMMIT admission, and await
actual control workers and raw statement EOF before recording native witnesses.
Persist a group terminal witness before admitting the next group. Metadata I/O
runs through blocking workers; no second native execution path is introduced.

An optional server-internal hook in the existing BEGIN worker persists its actual
managed handle and rollback intent before cleanup of an unpublished handle.
Persistence refusal cannot suppress mandatory cleanup, and cannot fabricate a
known durable rollback. Terminal persistence failure fences the source and stops
later native work. Closing/recovering a journal preserves earlier known outcomes;
unresolved intents stay uncertain and are never automatically resent.

Direct migration and human AI owners consume the same revision-bound native
projection. Emit entries only for observed statement attempts, scoped to each
transaction group or nontransactional statement. Transaction fields contain actual
managed TxId values, not workflow UUIDs or engine-internal XIDs. Counts use checked
signed conversion/addition, preserving zero and leaving missing/overflowing counts
unavailable. Only known owned rollback permits a rollback entry. Nontransactional
commit requires successful raw completion and a positive admission-time observation
of known autocommit scope; default absence of a transaction-unknown flag is not proof.
Legacy runs without a journal supply no new native ledger evidence.

The AI owner uses that same snapshot for its receipt and ledger. Full Applied
requires native completion and known commits/autocommit for all frozen work;
an earlier committed group cannot complete untouched later groups. Unknown control
settlement and unresolved statement intents remain uncertain. A late successful
autocommit can confirm Applied despite a failed legacy delivery/workflow receipt;
the message explains the actual workflow error. Exact replay reads the original
receipt without another native dispatch or original ledger entry.

**Consequences:** The legacy MigrationRun DTO, Driver signatures, journal schema,
protocol responses and CI dispatch policy stay unchanged. The separate audited
native-outcomes endpoint retains redacted source, group and statement evidence.
Box the migration owner boundary to avoid embedding its growing state machine
in HTTP/AI caller futures. This supplies native migration provenance for later
typed execution; it does not grant delegated/unattended authority or finish its
full native fault matrix. Native Windows/macOS acceptance stays deferred.

Native acceptance also exposed cardinality observations changing structural
catalog revisions. Revision hashing now excludes only the reserved estimated_rows
observation, which schema diffing already excludes. Returned graph data and its
full content digest retain it; all other fields remain in the conservative
revision fence. The regression checks stable revision across count changes,
visible observation/content changes, and revision advancement after actual DDL.

Acceptance on 2026-11-10 passed formatting, strict workspace/all-target Clippy,
52 focused session tests and 128 focused HTTP tests. The combined workspace run
passed 1,895 tests with 19 default ignored; explicit provider acceptance passed
5 unit and 4 HTTP tests. Real PostgreSQL/SQL Server/SQLite acceptance passed 3/3
with both transactional preferences, preserving SQLite's mandatory transactions.
Three additional PostgreSQL acceptance repetitions passed after the revision fix.
Gated tests cover pending replay, native EOF, publication cleanup, journal refusal,
failed terminal persistence and late autocommit; the deterministic multi-group
producer test preserves an earlier commit and a later rollback without dispatching
the remaining group. Existing reduced-memory compiler/profile settings retain
assertions and overflow checks. Aggregate Rust/Cargo CPU and RAM stayed hard
capped at 10%; no test stack limit or CI dispatch policy was changed.

## ADR-193 — Shared AI mutation admission and truthful unknown acknowledgement

**Status:** Accepted — 2026-11-10.

**Context:** Private SQL Execute and reviewed human database proposals used separate
claim guards. One chat could admit native work in both families, and an interrupted
human apply had no explicit acknowledgement path. Periodic retention also treated
an aged native claim as abandoned even though its retained owner could still run.

**Decision:** Both families check the same chat-wide applying/unknown guard inside
an immediate SQLite claim transaction. V060 storage triggers enforce the actual
claim transitions and protect chats with applying human owners. Exact replay
continues to return the original pending/terminal observation before new admission.
Pending drafts and approvals may coexist; they grant no native dispatch authority.

Expose audited HTTP and SDK status/acknowledgement endpoints for human database
applies. Require current chat/tenant/room access, the original approving principal,
and exact proposal, review, client-request and review-digest bindings. Applying
claims cannot be acknowledged. Record acknowledgement separately from state and
encrypted receipt: an unknown outcome stays unknown and its one-use proposal can
never be resent. Repeated exact acknowledgement is idempotent. First stored human
acknowledgement raises the metadata reader floor to V060. Private review/receipt
bodies remain encrypted; status exposes only redacted claim identity and state.

Vim controls select SQL approvals and unknown database proposals, inspect the
captured claim, then acknowledge that same selected claim. Instance/chat scope and
proposal identity fence asynchronous replies; changing selection cannot acknowledge
a different inspected proposal. The operator must inspect the database independently;
acknowledgement supplies neither native commit proof nor permission to reuse a
fenced connection.

Live maintenance may end an expired model run and revoke unclaimed requests, but
must retain native applying claims and their chats. Only startup/offline recovery,
under the existing exclusive serving/recovery lifecycle, can classify an abandoned
apply as unknown. Time passing is not proof that native work has stopped.

**Consequences:** This prerequisite closes overlapping claim admission without
adding agent mutation authority. Typed database/document/extension execution,
reviewed delegation and unattended lifecycle remain separate work. Existing
receipts and Driver signatures stay unchanged; CI stays manual dispatch only.

Acceptance passed formatting, strict workspace/all-target Clippy and the complete
workspace suite (1,896 passed, zero failed, 19 default ignored). Native PostgreSQL,
SQL Server and SQLite human apply/execution regression acceptance passed 3/3.
The expanded metadata case covers both cross-family admission orders, storage
trigger refusal, replay, unknown blocking, exact acknowledgement, reader-floor
advancement and retained aged owners. Gated HTTP coverage refuses acknowledgement
during retained native work and preserves unknown receipt/replay without native
redispatch; Vim coverage binds acknowledgement to the selected inspected claim.
All Rust/Cargo work stayed in the shared 10% CPU/RAM hard cap, with no swap.
Explicit external-provider acceptance also passed: five unit cases and four HTTP
cases, including retained timeout/caller-loss ownership. Native Windows/macOS
acceptance remains deferred; the typed native failure matrix remains open.

## ADR-194 — Canonical native failure tokens and real-engine settlement evidence

**Status:** Accepted — 2026-11-10.

**Context:** Native multi-group acceptance found a SQL Server duplicate-object
failure whose redacted historical label was `duplicate object`. The V059 journal
correctly refuses spaces in its bounded result-code field. Passing that label as
a code therefore refused a real failure observation, fenced the source, and
prevented confirmation of its required owned rollback. Existing mock errors used
`other` and did not expose the mismatch.

**Decision:** Native observations retain a separate canonical Code discriminator
for journal storage. Derive it from the fixed serde wire variant; handle extensible
Other before conversion so private diagnostic text cannot enter the token. Keep
historical audit/receipt labels unchanged. Do not relax journal validation or
repair earlier unknown observations by inference. Genuine persistence failures
continue to fence the source and preserve uncertainty.

Exercise the existing human apply owners against real PostgreSQL, SQL Server and
SQLite through a test-only Driver decorator. It delegates actual native work,
then can suppress or oversize a one-row update count, or hide an acknowledged
successful native commit. Independent native reads prove physical database state;
host receipts and ledgers must retain only their own evidence. Missing/oversized
counts require observed owned rollback and an unavailable ledger count. A lost
commit acknowledgement retains an unknown receipt/ledger, a fenced source and
one-use replay, including after explicit acknowledgement. Driver counters verify
that receipt inspection, replay and acknowledgement do not execute another native
statement or transaction end. These are observation-boundary faults, not claims
of a physical network outage or actual enormous row populations.

Real-engine journal tests also freeze three transaction groups: an earlier commit,
a later CREATE/INSERT followed by failure and owned rollback, and untouched work.
Use a successful independent catalog query to distinguish table absence from a
failed connection. Retain actual distinct transaction witnesses and inserted counts
while preserving the earlier commit through recovery/replay refusal. The SQL
Server duplicate-object case must persist its canonical token before rollback.
A positive owned rollback acknowledgement permits source reuse. If SQL Server
instead reports [native error 3903](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-3000-to-3999)
(no matching BEGIN for the rollback), retain
an unknown group, fence the source and emit no fictitious rollback entry; an
independent later observation is not the host's missing end acknowledgement.
A separate dispatch-refusal case requires acknowledged owned rollback on all
three engines after the group's successful CREATE and INSERT.

**Consequences:** Driver signatures, protocol schemas, metadata versions and
historical audit labels stay unchanged. This fixes native failure settlement and
supplies evidence required by typed execution. It grants no new provider execution
authority. Typed action requests, document/extension execution, public write grants,
delegation and unattended lifecycle remain separate work; native Windows/macOS
acceptance remains deferred. CI remains manual dispatch only.

Acceptance passed formatting, strict workspace/all-target Clippy and the complete
workspace suite (1,899 passed, zero failed, 19 default ignored). The three expanded
native AI acceptance tests pass on PostgreSQL, SQL Server and SQLite; three native
group tests each pass both dispatch-refusal and engine-error scenarios. The group
checks verify partial-run replay refusal without another native call, preserved
journal revision through recovery and an independent successful catalog read.
Canonical token coverage also verifies fixed wire kinds, private Other redaction
and compatibility of historical labels. Source hashes remain stable through the
checks. Aggregate Rust/Cargo CPU and RAM remain hard capped at 10%, with no swap
and existing assertion/overflow-check settings retained.
The real-file group fixture follows the existing Unix backend guard; these are
Linux observations and do not claim native Windows/macOS acceptance.
Explicit external-provider acceptance also passes: five unit cases and four HTTP
cases. The final guarded Rust snapshot remained unchanged through staging.

## ADR-195 — Typed AI database execution uses the existing native proposal claim

**Status:** accepted for interactive private typed database execution.

**Context:** Private SQL Execute already freezes immutable requests and requires
human approval. Typed row/schema drafts previously stopped at the ordinary human
proposal apply path. A provider needs to request and observe a typed action on
its original Execute source without acquiring approval authority or substituting
a reviewer's connection. A second native apply owner would split cancellation,
transaction evidence, unknown outcomes and replay guarantees.

**Decision:** V061 stores an ID-only execution request referencing the existing
encrypted run source, proposal and frozen native review. Its fingerprint binds
run, initiator, proposal content, review digest, original source and expiry. One
proposal has one request; run/call replay must preserve its proposal. First use
raises the metadata reader floor to V061. Tenant recovery retains these references;
stopping a run or discarding a proposal denies unclaimed requests. Deleting the
profile removes future admission authority while retaining historical receipts.

The audited API and SDK expose request, list, exact review, status, approval,
denial and explicit unknown acknowledgement. Only the private initiating human
can approve, with the exact fingerprint, production label and migration risk
acknowledgements. The provider-facing tool accepts only a same-run proposal ID.
All three desktop provider adapters share that dispatcher; Read/Propose receive
no execution tool or approval route. Vim selection binds source, chat, action and
request nonce, and reuses the existing row/migration preview and human controls.

Consume approval in the same immediate metadata transaction as the original
proposal apply claim and V060 shared AI mutation gate. Storage triggers refuse
ordinary human/older-writer bypass for linked proposals. Duplicates inspect the
exact claim before issuing more native work. A process-local, active-only gate
serializes each proposal's native preflight through claim so concurrent approval
cannot overlap previews on one engine handle; storage remains authoritative.
Same-scope catalog refresh invalidation occurs under its existing fetch gate,
preventing overlapping refreshes from manufacturing stale schema revisions.
Ordinary failed catalog reads retain the existing explicit stale fallback.

Reuse the bounded native apply owner, actor/correlation, actual transaction and
statement observations, journal/ledger, encrypted receipt and shutdown guard.
Native admission also checks the live run, expiry, current chat rights and profile
policy digest, in addition to captured source/generation checks. Stop/deadline
closure cancels an admitted cursor but retains its owner through raw EOF, issued
control acknowledgement and required rollback. Once COMMIT is issued, a late
confirmed acknowledgement retains its actual outcome. Unknown acknowledgement
never proves commit/rollback and never makes the proposal dispatchable again.
Status preserves native outcome after source loss while redacting result bodies;
full source-bound review refuses lost authority.

**Evidence:** Metadata atomic-claim/Stop/replay/unknown/profile-deletion acceptance,
strict workspace/all-target Clippy, real PostgreSQL/SQL Server/SQLite typed row,
count-refusal and lost-commit-acknowledgement acceptance, and native multi-group
migration acceptance pass. The 103 focused HTTP tests pass, including typed Stop,
deadline, caller-loss, policy-change and late-COMMIT settlement plus concurrent
catalog refresh. The exact human-confirmation/stale-reply Vim test passes, and full
workspace validation passes 1,901 tests with zero failures and 19 default ignored.
Formatting and strict Clippy pass. Explicit external-provider regression checks
pass five library cases and four HTTP cases. All 32 checked Rust/SQL/JSON sources
retain their verification hashes through final review. Rust/Cargo validation
retains the aggregate 10% CPU/RAM cap.

**Consequences:** This implements interactive private typed database requests.
Document/extension execution, public writes, reviewed delegation, unattended runs
and native Windows/macOS isolation acceptance remain separate requirements.

## ADR-196 — Reviewed query execution distinguishes room commits from companion evidence

**Status:** accepted for interactive reviewed query/document execution.

**Context:** A query proposal and its human-applied status do not prove that Sift
performed a text edit. Room documents have an authoritative CRDT actor; scratch
tabs belong to a live desktop and cannot acquire server durability merely from
an HTTP acknowledgement. A provider must request an exact edit without choosing
another target or supplying its own approval.

**Decision:** V062 adds an ID/hash-only query action referencing the existing
encrypted proposal and run context. Bind the full original SQL, proposal content,
original room/document or private desktop/tab, actor, source digest and expiry
into its fingerprint. First use raises the reader floor to V062. The public wire
contract remains pure serde. The existing ordinary human-applied proposal path
cannot consume a linked query execution. Public-chat Execute is still refused;
a private conversation may target an authorized shared document while retaining
the initiating actor's own managed database connection.

Audited request/list/review/status/approve/deny/acknowledge/reconcile HTTP and SDK
paths expose the action. Consume exact approval and the shared SQL/database/query
chat mutation gate in one immediate transaction, with storage enforcement against
older/direct writers. Current run, expiry, actor, profile and room write rights
are checked at mutation admission. Stop denies pending approval; a claimed action
never becomes dispatchable again. Query text edits do not execute SQL.

Room replacement uses the normal document actor's validation and limits. Compare
complete original text under its lock, then append the authored CRDT update and
settle the action/proposal/sequence in the same metadata transaction. Import and
broadcast the committed update through the existing collaboration path. A receipt
persistence refusal rolls back the append. Restart preserves committed outcomes;
an action still applying proves its atomic room append did not commit, so recovery
records Refused/NoDispatch. CRDT state remains limited to query text.

Private approval returns one fresh dispatch capability only to the human companion;
metadata retains its digest, and duplicate approval never reissues it. The captured
desktop executor retains the capability and original server client through an
explicit UI reply lane, bounded to eight owners. Scoped starts and approvals use
the originating AI view UUID as desktop identity, preventing local tab-ID reuse
in another window from selecting a different buffer. UI admission independently
checks that identity, instance/chat/action/request nonce, exact reviewed target,
full original text, live profile/database binding, observed run state, Stop and expiry. Normal generated-SQL
replacement preserves undo/dirty tracking and reports actual before/after hashes.
The capability never enters a provider result or UI event.

Expiry atomically closes a queued UI admission. A dropped queued event reports
refusal to its original server. After admission, retain ownership through actual
synchronous editing and reply; missing settlement is unknown. The server records
companion acknowledgement as distinct evidence from a room commit. A missing
scratch acknowledgement expires unknown, and a late exact observation is retained
separately without rewriting that original receipt or authorizing another edit.
Acknowledging inspected uncertainty clears the shared gate, never the one-use
claim. Profile deletion preserves receipt history; recovery and retention handle
pending and issued actions without introducing new plaintext AI-body storage.

All three isolated provider adapters share `sift_execute_query`, accepting only
a same-run proposal ID. The provider waits for status; it cannot approve, supply
production authority, change target or retry uncertainty. Vim review shows the
original and replacement text, with explicit inspect/approve/deny/acknowledge
controls. Action/reply selection remains bound during provider activity.

**Evidence:** Metadata acceptance passes 160 tests (one default ignored), including
atomic append/receipt rollback, concurrent one-use claims, shared admission, Stop,
exact companion acknowledgement, recovery and preserved late evidence. The actual
room actor passes commit, stale text, Stop, changed policy, revocation and update
limits with durable reload. HTTP/SDK acceptance passes five corresponding room/
scratch scenarios plus public Execute refusal and zero native SQL dispatch.
Strict workspace/all-target Clippy and both companion tests pass. The actual
editor verifies one exact replacement and rejection of another tab/window,
changed profile/database binding, stale text/nonce, Stop and duplicate dispatch. The reply lane distinguishes dropped/
expired queued work, admitted uncertainty and settlement retained after expiry.
All three native-engine and migration-group regression tests pass. The captured-
server desktop integration confirms that a dropped UI event records refusal and
leaves the proposal unapplied. Full workspace validation passes 1,909 tests with
zero failures and 19 default ignored. Explicit external-provider regressions pass
five library cases and four HTTP cases. All 37 checked Rust/SQL/JSON files retain
their verified hashes through final review under the persistent 10% CPU/RAM cap.
CI remains manual-dispatch-only.

**Consequences:** This adds interactive reviewed query text execution. It does not
grant extension actions, public write delegation, unattended authority or native
Windows/macOS isolation acceptance; those remain separate requirements.

## ADR-197 — Extension approvals bind the prepared action contract

Status: Accepted

Context: A direct extension approval previously bound the public operation and
context, while governed-tool approvals bound only the tool name and context.
Dispatch subsequently loaded registration again. Replacing a package or changing
an action schema could therefore change what an existing approval authorized.
Input validation also happened after approval consumption.

Decision: Prepare one action from one registration snapshot. Derive its target
from the authorized context, retain its actual invoker and compiled schemas, and
include required contexts, both schemas, timeout/result bounds and installed
archive/manifest/selection revision in the canonical review fingerprint. The
selection revision changes with grants and tenant allowance. Governed tools also
bind their complete published descriptor. Both direct approval producers use the
same prepared review context. Invocation paths validate bounded arguments before
creating or consuming their approvals, then invoke that prepared action. The
standalone approval producer binds the supplied input hash; actual arguments are
validated on the later invocation. Refuse a
prepared action from a replaced registration or a different target before RPC;
never substitute the replacement invoker.

Consequences: Outstanding approvals made with the old binding require fresh
review. Unchanged installed contracts remain stable across reconstruction. No
arguments or result bodies are newly persisted. This establishes review identity;
it does not close revocation races during process readiness/writer waits, retain
invocation ownership after caller loss, or atomically journal approval/dispatch.
Those admission, settlement and recovery requirements remain open in the
extension dispatch settlement contract in `docs/PLANS/ai-execution.md`. No AI
extension execution authority is added. Successful invocation acceptance also
exposed an invalid `started` audit insert against the terminal-only audit schema.
Record one terminal RPC observation through the ordinary audit append API and
remove the unreachable start/finish transition API. Durable pre-dispatch claims
remain the separate journal's responsibility; terminal audit alone cannot recover
an interrupted invocation.

## ADR-198 — Extension RPC observations separate frame entry from response deadlines

Status: Accepted

Context: Extension action receipts need to distinguish an unissued request from a
possibly partial frame and a response observed during deadline cleanup. The shared
supervisor previously killed a generation even when writer admission timed out,
and discarded every response received during cancellation grace.

Decision: Add an observed request path with a callback that runs after acquiring
the process writer. The original deadline bounds writer waiting and the callback;
explicit deadline and process-drain checks run before and after it. A pre-frame
refusal removes only that pending request. Once frame writing is entered, failed
or canceled writes stop the generation because a partial frame cannot be reused.
The observation records frame entry conservatively, an on-time observed response
(including remote errors), transport failure, and any exact late reply observed
during bounded cancellation grace. A ready response seen after the deadline is
retained separately without an unnecessary Cancel. Closed response channels cannot
count as cancellation acknowledgement. Existing request callers retain their
response/error API and timeout result; typed owners may inspect the richer data.

Consequences: Frame entry proves neither full transmission nor native admission.
An extension response proves no database commit, rollback or read purity. The
callback must tolerate cancellation and independently fence detached storage work;
caller-loss ownership, captured actor/source checks, durable claims/receipts and
recovery still belong to the higher-level dispatcher. No new provider execution
authority or database Driver trait change is introduced. Real supervised-process
acceptance covers writer waits, revocation, expiry during synchronous admission,
drain, partial writes, late replies and legacy response/error behavior.

## ADR-199 — Extension actions retain one-use dispatch and redacted RPC receipts

Status: Accepted

Context: A prepared extension contract and supervised writer observations cannot
alone preserve ownership when HTTP callers disappear, prevent authorization replay
from repeating an action, or recover an interrupted request. A Read classification
does not establish purity. Native database receipts cannot describe arbitrary
extension RPC effects.

Decision: Store a versioned extension execution journal in metadata (V063). Atomically
consume any required approval and claim both its identity and an actor/request UUID
against exact operation, input and reviewed-context hashes. Retain a bounded owner
before awaiting that claim. It keeps the original actor, correlation, source,
credential reference, contract, deadline and shutdown lifetime through process
readiness, writer admission, response validation and terminal receipt/audit storage.
Never repeat the RPC on identity replay, renew its deadline or store arguments,
results, credential bytes or private error text in this journal.

At writer admission, re-resolve the original credential and source policy, then
atomically compare metadata authority/source snapshots, opaque credential lifetime,
package selection revision and tenant allowance while recording dispatch intent.
Record actual frame entry separately: durable intent proves neither transmission
nor native admission. Preserve valid result/error acknowledgements and late replies
independently of timeout or output-delivery authorization. Revocation withholds the
body without erasing known RPC observations. Settlement and its one terminal native
invocation audit commit together. Receipt reads are separately audited, owner-only,
tenant-scoped and subject to current target visibility; deletion of a target does
not require an active database connection to inspect an otherwise authorized receipt.

Exclusive current-file startup recovery refuses unissued claims and marks unresolved
dispatch intents unknown. Historical full restore marks every pending claim unknown,
because execution may have followed the snapshot, and removes restored approvals.
Tenant restore preserves the target journal rather than importing snapshot authority
or erasing later observations; selected pending target rows use current-file recovery.
Terminal evidence is immutable and no unknown outcome permits automatic retry.

HTTP/SDK, CLI, standalone MCP and Vim expose bounded redacted receipts, including
lookup by request or approval identity. SDK authentication replay preserves one UUID;
desktop intent UUIDs survive approval/retry. Receipt inspection is independent of an
active query connection. First timely authorized results remain transient responses;
replays return observations only. Clients must preserve request UUIDs across explicit
transport retries; omitting one creates a new intent.

Consequences: An acknowledged RPC proves neither database commit nor read purity,
and a produced body proves no client delivery. Prepared registration and live source
checks complement transactional metadata checks; they cannot transact with an
external process. Queue-time desktop account/endpoint capture remains a separate
requirement before broader delegated execution. This adds no AI extension execution,
public write grant, unattended-run authority or native provider certification.

## ADR-200 — Queued extension work binds its original caller at both ends

Status: Accepted

Context: A retained HTTP owner captures the actor only after a request arrives.
Desktop work waiting in a queue could instead resolve a changed account, endpoint
or native connection. A probe alone cannot close replacement after that probe;
older daemons can ignore new expectation headers on legacy routes.

Decision: Negotiate `extension.caller-scope@1` and capture the selected logical
endpoint, verified instance/daemon identity, principal and authenticated session
before queue admission. Carry that immutable scope with each extension, governed
tool, approval and receipt command and its reply. Refuse enqueue without the
supported authenticated scope. Resolve connection targets to their original native
session/connection IDs in the UI instead of an `active` placeholder after waiting.

The executor checks its original logical endpoint, then uses a bounded SDK capture
that probes identity and copies one credential into a client without a mutable
session provider. Verify principal/session through that copied credential. Expose
only distinct `/v1/scoped/` extension routes on the resulting client. The receiving
boundary requires exactly one of every instance/daemon/principal/session assertion;
compare server identity before routing and the authenticated caller before handler
processing. Record the actual authenticated actor for HTTP audit before comparing
caller assertions, so a mismatching assertion cannot masquerade as another actor
or erase attribution. These are constraints on existing authority, never authority grants.
Missing capability, mismatch and missing routes never fall back to legacy actions.
The captured transport disables HTTP redirects before identity probing, credential
refresh and guarded requests, so redirect responses cannot escape these routes or
carry the captured credential to another endpoint. Ordinary clients retain their
existing transport policy.
Validate approval UUIDs before constructing scoped URLs; caller-supplied identity
text cannot introduce path navigation and escape that route family.

Return replies through the captured UI scope and existing generation guards. Clear
private extension/tool views on scope changes; reject replies from another account,
session, endpoint or daemon. Independently reject pending direct-action results or
approval publication after a captured target context changes. Original retained
HTTP owners and V063 receipts remain authoritative after UI loss or withholding;
inspection is independent of a live native connection and dispatch is never retried
by this mechanism.

Consequences: Interactive session refresh can rotate credentials only while keeping
the reviewed caller session. A captured client never adopts a later session token.
Legacy clients retain their explicit ordinary routes, while protected desktop work
requires the new capability and routes. The assertions do not authenticate an
untrusted network peer or attest database effects. No Driver trait change, new AI
execution authority, unattended-run scope or native platform evidence is implied.

## ADR-201 — Observed RPC Stop retains cancellation ownership

Status: Accepted

Context: Reviewed AI extension execution needs Stop to close new admission while
an already issued request remains owned. Dropping the observer loses cleanup and
late acknowledgement; killing a shared process for every Stop also interrupts
unrelated work without proving rollback.

Decision: Add an explicit Stop future to an observed supervisor request. Existing
callers retain their API through an always-pending Stop. The original request
deadline still bounds writer waiting, authorization and transmission. A ready
Stop wins before fresh admission or polling a new frame writer. Refusal before
frame entry releases the pending slot without damaging a healthy generation.

After frame entry, retain the same owner through bounded targeted cancellation.
A partial request or Cancel frame fences the generation. A fully transmitted
request uses the existing Cancel method and cancellation grace; a missing valid
response also fences the generation. A valid late response remains a separate
observation, never timely success or rollback evidence. Avoid a new Cancel when a
response is already observed; do not start a Cancel after its cleanup deadline.
Preserve the primary Stop/deadline cause even if process cleanup also fails.

Consequences: This transport primitive grants no AI authority and no native-effect
attestation. AI admission, encrypted private results, journal linkage, human review,
unknown acknowledgement and recovery remain the coupled next family. Callers must
retain the observed owner after UI/HTTP loss. Real process acceptance covers Stop
while waiting for a writer, Stop ready immediately after admission, late response,
missing acknowledgement and interrupted large-frame transmission, alongside the
existing supervisor suite and required workspace checks.

## ADR-202 — Owned extension readiness shares original deadline and Stop

Status: Accepted

Context: Targeted RPC cancellation begins at the supervisor request boundary.
Production action invokers also wait for tenant slots, restart backoff and child
startup. Dropping readiness after a generation permit is acquired can release
capacity before the child terminates; bounding Hello alone leaves Welcome writes
outside the startup deadline.

Decision: Add owned cancellable startup and runtime readiness without changing
existing caller APIs. One startup deadline covers Hello reading and Welcome
writing, bounded by the original action deadline where supplied. Stop/deadline
refuse before spawning when already ready; after spawning, every preparation
error retains child ownership through termination before returning. Preserve the
primary interruption/validation cause. Existing startup uses a pending Stop.

Observe Stop/deadline during runtime map, tenant-slot, health and retry waits
before allocating a generation permit. Once acquired, directly await the child
startup owner through cleanup rather than selecting around and dropping it.
Human Stop and original deadline expiry do not spend provider restart budget.
Carry the same Stop future into observed RPC dispatch after readiness. A native
acknowledgement preserves a healthy shared generation; partial/missing frames
retain ADR-201 fencing. Conservative in-process invokers refuse delegated entry
unless they implement this observation contract explicitly.

Consequences: This is startup/RPC ownership, not AI approval or native-effect
attestation. Private AI review, atomic journal linkage, encrypted content, shared
admission, provider tools and Vim still require their coupled implementation.
Native acceptance must cover stalled Hello/Welcome child cleanup, generation
permit lifetime, repeated Stop without quarantine and the actual runtime-to-RPC
handoff. Windows/macOS native acceptance remains deferred.

## ADR-203 — Reviewed AI extension execution links one atomic RPC claim

Status: Accepted — Linux implementation and local acceptance; native Windows/macOS acceptance deferred

Context: AI Execute already has reviewed SQL, typed database and query edits.
Extension actions require independent approval and supervised process ownership;
classifying an action Read does not establish purity or remove shared admission.

Decision: A private active Execute run can request one governed tool and bounded
arguments using an immutable call UUID and its lease. Derive the context/source
from that run, freeze the live descriptor and prepared contract for human review,
and encrypt review/result bodies through existing AI content handles. Store IDs,
hashes and a single immutable V063 execution link in the new V064 AI row.

Human approval binds the exact classification, production/source review and
fingerprint. One immediate transaction checks active review/run and shared chat
admission, creates/consumes any generic approval and claims V063 with the original
actor/request identity, then links the AI review. Retain the native owner before
that transaction is awaited. Writer admission rechecks original run/Stop/expiry
and existing live credential/package/source guards. Keep the original minimum
run/review/action deadline through bounded readiness and RPC cleanup.

Derive terminal AI workflow state from the linked immutable journal; never commit
a second competing native outcome. Missing or mismatching restored evidence and
Stop/deadline uncertainty remain unknown even when a late RPC reply exists.
Explicit owner acknowledgement names the exact fingerprint and execution ID,
releases only the shared gate and never reissues work or rewrites the journal.
Result availability and encrypted disclosure cannot erase RPC facts or renew
approval. No result/argument/credential bytes enter V063 or audit fields.

Extend shared admission predicates and all existing family trigger directions,
retention/cleanup/key rotation and full/tenant recovery inventory. Preserve the
target journal during recovery; restored reviews cannot recreate authority.
Expose audited human request/detail/approve/deny/ack paths, SDK and Vim review on
the captured caller/source, plus fixed request/status tools for all three AI
adapters. Providers receive neither human approval nor unknown-ack authority.

Consequences: An RPC acknowledgement proves neither database commit nor read
purity. Public writes, delegation, unattended lifecycle and native platform
isolation remain separate contracts. Acceptance combines real-process HTTP
ownership/replay/revocation/Stop/late replies, atomic rollback and cross-family
admission, encrypted content/recovery and actual Vim review/caller fencing.

Admission and retention follow only matching actor/request/tenant journal identities.
A known no-frame refusal closes its review and releases admission without an
unknown acknowledgement; a stopped or expired late acknowledgement remains
uncertain. Human acknowledgement cannot alter another owner or recreate missing
journal evidence. Only the first owner retains a result, and current source,
registration and package authority are checked again before private disclosure.
Redacted receipt inspection and exact unknown acknowledgement remain available
when the original native source is gone; approval still requires that source.

Acceptance: Six real extension-process HTTP cases pass, covering captured-caller
concurrent approval/replay, post-retention and in-flight registration revocation,
source revocation, Stop with a late acknowledgement, and invalid late bodies
without losing the primary interruption cause. Seven metadata cases cover atomic
rollback, shared admission, encrypted result loss/cleanup, historical full/tenant
restore, durable pre-frame Stop and mismatching journal acknowledgement. Two actual
Vim interaction cases prove human production/classification approval, caller/source
fencing and exact unknown acknowledgement after connection loss. Metadata and
process HTTP cases also run with JSON key-order preservation enabled; canonical
review hashes remain interoperable with the desktop build. Formatting, strict
workspace/all-target Clippy and 1,942 workspace tests pass (zero failed,
32 default ignored), with all 36 changed Rust/SQL/JSON source hashes unchanged.
All Rust/Cargo validation uses the persistent aggregate 10% CPU/RAM cap.

## ADR-204 — Run delegation and companion lifecycle preserve per-action authority

Status: Proposed — coupled implementation and acceptance pending

Context: Interactive Execute now covers SQL, typed database changes, query edits
and governed extensions. Unattended operation additionally needs explicit human
run authority and a locally authenticated, isolated provider owner independent of
an open chat panel. Existing publication grants disclose data; they authorize no
writes. Existing schedule leases authorize server run admission, not model access.

Decision: Review an immutable, expiring run delegation separately from an individual
action. Bind its initiator, human approver, tenant/chat, original source, policy and
credential revisions, operation families, production choice and cumulative call,
input/output byte and time limits. Keep sensitive policy/action material encrypted.
Model requests keep their existing fixed request shapes and original run lease;
they cannot select an approval, actor, target, grant, credential or wider policy.

Support exact preapproved actions and explicitly reviewed bounded typed rules.
Arbitrary supported SQL is a separate opt-in family, denied by default by instance
and tenant policy; review discloses unbounded affected rows, trigger and external
effects. Parser acceptance, rollback and result-row limits do not contain those
effects. Typed row limits bound submitted edits, not triggered native work. Freeze
extension registration/package/schema/classification and its argument rule. Query
edits bind their exact original room/document or desktop/tab and revision; private
editor execution still requires the original live editor companion capability.

Reuse each existing preparation, human/native review, shared chat admission and
retained settlement owner. In its immediate dispatch transaction, intersect the
current initiator rights/source with the exact active delegation, consume bounded
usage, and claim/link the existing native journal. A replay observes its original
claim and never spends a second grant use or creates another native invocation.
Approval provenance records individual human approval versus run delegation,
including its actual human approver; the model is not a stronger principal.
Stop, expiry, revocation and source/credential/policy replacement close admission.
Already issued work retains truthful native/RPC settlement and independent delivery.

Human run-review APIs use captured-only routes and fixed-caller SDK transport;
there are no ordinary aliases or provider approval tools. Instance delegation
and arbitrary-SQL opt-ins default off independently of tenant opt-ins. Review
scope includes instance/daemon, actual initiator/authentication session, verified
credential record and tenant restriction, and AI policy limits. Presented token
bytes never enter the review fingerprint or content. Resolve native migration/extension and committed room-query
material on the server; private editor text remains an advisory observation that
still needs its original companion at dispatch. Recheck live source and material
before approval. Approval alone neither activates a run nor invokes native work.
Retain mutation owners through metadata settlement and original run cancellation
notification even when an HTTP observer disappears. Inspect/deny/revoke remain
available when Execute is disabled and withdrawal does not decrypt content.
Return the original consumed identity and independently report its grant state;
revocation cannot turn a spent review into a reusable approval. These receiving
APIs are prerequisites for the remaining Vim/CLI, delegated dispatch and companion
lifecycle, rather than proof that unattended execution is complete.
Wire DTOs stay in protocol; concrete JSON codecs live in the pure api-types
crate to bound server handler code generation. The HTTP adapter keeps typed
OpenAPI schemas and Axum content-type/syntax validation, then decodes original
bytes so duplicate fields cannot be silently collapsed.

Native migration review material must be emitted by the same renderer invocation
as the retained plan. Keep the exact typed changes referenced by its forward
statements, with a full-plan content hash, in the process registry. Implicit child
DDL remains visible in the frozen command review; a copied change ID does not
prove a bounded command's complete effects. Capture the original connection
before preview reads, and intersect the review lookup with current managed
principal/tenant/profile/policy, proposal identity, expiry and connection
generation. Validate/apply also refuse a replaced generation. Synthetic native
settlement fixtures may retain plans without review evidence, but those plans
cannot supply delegation review material. Retention supplies evidence for review;
complete native bounded command/object proof remains a separate prerequisite.

Individual human denial is valid only before approval; revocation closes either
an unused review or its original active run grant. Keep a consumed review consumed
so its immutable run linkage cannot be reissued. In one immediate transaction,
close the grant, cancel the running AI run and append its single stopped event.
Do not rewrite issued native action journals or refund reservations. Return the
original revoked run identity on exact retry so a retained server owner can retry
signaling its cancellation token after a lost response. Revocation requires the
original initiator and reviewed fingerprint, but must remain possible without
reading encrypted content, a live database, or enabled/current execution policy.
This metadata transition is a prerequisite; public endpoints must additionally
capture/authenticate the human caller and audit the operation before invocation.

Separate local provider execution from GPUI. A headless or background companion
uses the same Linux sandbox, Sift-only broker, immutable call identities and
bounded streams as the desktop. Provider authentication remains local. Observe
through typed events; UI attachment, detachment or view switches do not create a
provider turn or transfer ownership. Explicit Stop and companion loss remain
visible, and private editor work without a live original editor refuses rather
than fabricating an edit receipt. Capture server/account identity before queued
or headless work and enforce it at the receiving boundary, including setup.

Scheduled entry reuses existing configuration ownership, occurrence identity,
misfire, current-policy and lease checks. An explicitly reviewed schedule template
can create a fresh bounded run delegation for one occurrence; the delegation
cannot outlive that run. Review includes recurrence, expiry and aggregate workload
bounds. Bind the immutable template revision and configured source restrictions;
pin the actual admitted native source and validate required observable identity
constraints before any action. A configured profile alone is never presented as
native database identity attestation. No server-stored model credentials or server
model subprocesses are introduced.

Claim one companion/occurrence/provider-turn owner before startup. Lease loss,
recovery or an expired observer cannot restart a provider turn or retry uncertain
writes. Interrupt historical run/template authority during full and tenant restore;
preserve journals and require exact human acknowledgement of uncertain previous
work before automatic continuation of the same workflow. Native start replay and
schedule admission must not open a fresh chat to evade that guard.

Consequences: Public-room write grants remain a separate explicit authority
contract. This decision does not relax Read/Propose, native isolation, operation
policy, source identity or Driver signatures. Acceptance must combine cumulative
and concurrent budget claims, stale/revoked/expired grants, all action owners,
real isolated provider roundtrips, source-lost receipts, companion/observer loss,
scheduled replay/recovery and actual Vim run/delegation controls. A UI-free runtime
is a prerequisite, not proof that unattended authority exists.

## ADR-205 — Local isolated provider runtime is independent of GPUI

Status: Accepted — Linux implementation and local acceptance; native Windows/macOS acceptance deferred

Context: The three provider adapters, their Sift-only broker, model discovery and
bounded input/output helpers live in the desktop binary and emit GPUI executor
events. Reusing them for a headless companion would otherwise introduce UI
libraries into shared execution code or create a second isolation implementation.

Decision: Move those existing provider implementations into `sift-ai-runtime`, a
local client-side library with no GPUI or workspace-UI dependency. Keep provider
authentication, private-home reconciliation, sandbox arguments, fixed tool surface,
request identities, limits and durable Sift publication behavior. Model discovery
returns local data rather than a UI presentation type. A typed event sink carries
only provider text/activity and redacted execution summaries; its desktop adapter
captures the original `AiViewScope` before execution and maps exhaustively to UI
events. Native query editor work and bounded UI review-request tasks stay in the
desktop, where their original owner and editor evidence exist.

This extraction adds no run delegation, scheduled model turn, automatic approval,
new provider tool or new headless entrypoint. ADR-204 remains the coupled authority
and lifecycle contract. Verify moved behavior using its existing frame, quota,
streaming, broker, credential-home and native Linux isolation cases, together with
an actual desktop adapter scope/disconnection test and workspace acceptance.

Acceptance: The normal dependency graph has no UI, server or metadata crates.
All 13 moved default runtime tests pass; the three installed, signed-in Linux
Codex/Claude Code/OpenCode isolation and Sift-only tool roundtrips pass explicitly.
The desktop adapter test proves immutable view capture, private-text debug
redaction and closed-observer reporting. Formatting, strict workspace/all-target
Clippy and 1,943 workspace tests pass (zero failed, 32 default ignored).
All 19 implementation/manifest snapshots, including removed desktop paths,
remain unchanged through acceptance. Validation retains the aggregate 10% CPU/RAM
Rust/Cargo cap. Provider implementations and existing tests match their former
bodies after the declared type/import substitutions. ADR-204 remains proposed;
this prerequisite creates no unattended authority or headless entrypoint.

## ADR-206 — Database reviews pin the original runtime connection generation

Status: Accepted

Context: A fresh server runtime can reuse numeric session/connection IDs while
connecting to the same configured profile and catalog revision. Those values do
not establish that an unconsumed database review still targets its original
physical handle. A real SQLite replacement-runtime regression reproduced acceptance of a fresh
apply using an old review under those conditions.

Decision: The trusted review producer captures and binds its authorized connection
before catalog/preview work, refuses transparent source replacement, rechecks that
capture before storing the review, and includes its execution generation in the
signed encrypted review body. Fresh apply requires that generation to match the
authorized captured connection before new catalog/claim/dispatch. The generation
is produced by the server and cannot be chosen by a model or approval request.

Old encrypted review bodies remain readable with the optional field omitted from
serialization, preserving their original digests. Missing proof requires a fresh
review before new work. Exact already-claimed requests observe their original
durable receipts before source checks; they never dispatch again on a replacement
handle. Current actor/tenant/profile/policy and production checks continue to apply.

Consequences: This proves runtime handle identity, not native database/role
attestation, bounded effects, or completed managed-source/companion ownership.
Automatic setup still needs a captured, bounded source owner retained through
settlement; it cannot treat source generation as a resource lease or reconnect
permission. ADR-204 remains proposed and the wider inventory remains partial.

ADR-206 acceptance: Formatting, focused real SQLite regression, strict workspace/all-target Clippy and 1,971 workspace tests pass (zero failed, 32 default ignored) under the aggregate 10% CPU/RAM cap. All 7 implementation snapshots match accepted source. The real replacement-runtime SQLite case proves stale review refusal and preserved original-receipt replay. Legacy encrypted bodies remain readable; their missing proof cannot authorize fresh dispatch.

## ADR-207: Server-owned automatic AI database review sources

Status: Accepted

Automatic review setup is a scoped-only operation. Validate actual authenticated
principal/session against the captured instance/daemon/caller and check proposal,
publication, tenant, profile and preview authority before creating a session.
Reuse an explicitly supplied ordinary source through the existing review path.
A new automatic request reserves a bounded server-owned source and records its
nonce durably before native connection setup. Replay observes the original
preparation/review or refuses a missing owner; it never opens a replacement.
Attempt records contain identities only and follow proposal retention.

The source registry holds at most 32 sources and expires unused reviews after
five minutes. Each source has a weak link to its original SessionStore. Session
markers remain weak and distinguish an expired managed source from an ordinary
manual source. Captured connection entries and pending connection opens retain
the strong owner through detached native work, late open cleanup and settlement.
Expiry releases only the registry reference. Last-owner cleanup directly awaits
physical connection close before closing the empty session and releasing its slot.

General session/cursor routes cannot access managed sources. Review/apply aliases
require the exact original caller, including actual authenticated session; asserted
headers grant no authority. Captured SessionStore authority prevents body-based
query/transfer routes from acquiring these sources.
Already-claimed apply replay remains observable before live source validation.
No reconnect or fresh credentials are permitted for an absent original owner.
Native database/role attestation, room editor ownership and public unattended
delegation remain separate requirements.

ADR-207 acceptance: Formatting, four native lifecycle regressions, real SQLite review/apply/replay acceptance, both desktop regressions, strict workspace/all-target Clippy and 1,976 workspace tests pass (zero failed, 32 ignored) under the aggregate 10% CPU/RAM cap. All 21 implementation snapshots match the accepted sources. Paused-clock expiry, observer timeout/native EOF, late open cleanup and existing revocation close preserve source admission; real SQLite proves exact review binding, ordinary-path refusal, restart nonce refusal and single apply/replay. This does not establish native database/role attestation or room/companion/delegation ownership.

## ADR-208: Server-owned human room query proposal commits

Status: Accepted

Legacy room proposal application edits the client CRDT before capturing its HTTP
acknowledgement owner. View loss can discard synchronization state before a caller
is captured. Human room approval will instead use a scoped-only server commit,
with actual original caller validation before editing. The existing acknowledgement
endpoint keeps its semantics and never acquires a new editing side effect.

The retained bounded owner loads the immutable proposal and original run text,
checks exact content hash/base revision, then authors through the normal room
DocumentActor under its mutex. Validate full original text, current room write
permission, principal/session and shared AI mutation admission. The CRDT append,
proposal/event settlement and a durable commit receipt share one SQLite Immediate
transaction. Publish the normal committed document update only after persistence.
No SQL is executed against a database and no local optimistic replacement occurs.

Receipts retain full before/after hashes, original actor and document sequence
independently of document log compaction. Exact replay observes the receipt before
looking at mutable current text; changed approval or another actor cannot dispatch
again. Preserve receipt history under coordinated tenant recovery and proposal
retention. Capture credentials before dispatch, retain the original owner after
view/HTTP loss, and revalidate current caller before native admission. Native
private-editor companions, delegation and unattended recovery remain separate.

ADR-208 acceptance: Formatting, three room integration tests, the native scoped/no-optimistic-edit UI regression, strict workspace/all-target Clippy and 1,980 workspace tests pass (zero failed, 32 ignored) under the aggregate 10% CPU/RAM cap. All 22 implementation/fixture snapshots match. Real HTTP refuses absent/mismatched callers and changed approval; receipts replay after log compaction without overwriting later text. Native stale text, revoked permission/session, deadline, non-SQL document and late transaction failure leave no partial update or receipt. Original observer loss retains commit ownership. Moving HTTP-heavy tests to their own integration binary kept the cap unchanged after a server unit-test compiler OOM.
