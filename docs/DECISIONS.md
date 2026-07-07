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

**Consequences.** New product actions should add protocol-visible operation
shape instead of disappearing into ad hoc handler logic.

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
