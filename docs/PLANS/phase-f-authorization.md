# Phase F — Authorization, Tenancy, And Limits

Status: implemented and graduated on 2026-07-21. ADR-020 is authoritative when
this plan and the decision record differ.

## Outcome

Phase F turns Phase E's authentication and resource-ownership floor into one
enforceable policy path for every database operation. A hosted user can act
only when tenant role, optional room role, and connection policy all allow it.
The same path provides capability discovery, rate admission, tenant resource
admission, and later governance for shared rooms and automation.

Personal-loopback remains the trusted local exception: no visible login is
required, direct connection specs work, and general tenant/rate limits are
unlimited by default. The server still resolves an internal bootstrap
principal and tenant, and hard result, cursor, timeout, cancellation, and
driver-isolation safeguards remain enabled.

## Locked contracts

### Authorization order

For a database-facing action, the server resolves and checks:

1. authenticated or trusted-local principal;
2. tenant membership and tenant-role baseline;
3. room membership and role when a room is in context;
4. permission to use the connection profile;
5. operation allowlist/blocklist;
6. read-only and schema restrictions for SQL/object operations;
7. principal and tenant rate admission;
8. tenant live-resource admission;
9. bounded driver dispatch.

Denial at any layer stops dispatch. Blocklists override allowlists, and
administrative roles do not bypass database policy. Checks that do not apply to
an operation are absent rather than implicitly denied. Policy denial is audited
as the failed typed `Operation`, with no SQL, bind, or secret leakage.

### Runtime provenance

Managed connections store principal id, tenant id, profile id, and policy
revision. Transactions, queries, cursors, result buffers, and spill files
inherit this provenance. Trusted-local direct connections use a distinct
marker and cannot be created through a network/shared route.

Access removal, profile deletion/disablement, credential revocation, and an
explicit admin disconnect cancel and close descendants immediately. Ordinary
policy edits increment the revision: in-flight work already admitted may
finish, but the next operation re-evaluates current policy.

### Rate limiting

Hierarchical token buckets are keyed by principal/route-class and
tenant/route-class. Both reservations succeed or neither is charged. Classes
are control/metadata, interactive read, query admission, heavy transfer, and
stream bytes. Monotonic refill, configurable burst/cost, lazy creation, and
idle eviction keep the limiter smooth and bounded.

HTTP admission denial is 429 with `Code::RateLimited` and ceiling-rounded
`Retry-After`. WebSockets receive the same stable code. Stream bytes are paced
with bounded, cancel-safe backpressure after headers have started. Phase E's
login/refresh abuse limiter remains separate.

### Tenant resources

Effective limits cover:

- durable connection-profile count;
- open sessions;
- open managed connections;
- concurrent driver queries;
- open cursors;
- retained in-memory and spilled result bytes.

Configuration supplies defaults and operator ceilings. Instance admins may set
durable per-tenant overrides within those ceilings; tenant owners/admins may
inspect effective limits and usage. `None` is unlimited and zero denies new
admission. Lowering a limit below live usage blocks new admission and lets
existing work drain.

Live resources use reservation guards. Ownership transfers to a successful
runtime resource and releases on all terminal paths. Result bytes are charged
until their response body, buffered page, or spill file is dropped; delivered
bytes and query history are not retained usage. Durable profile quota is
checked in the same SQLite transaction as profile creation.

### Namespace isolation

Personal saved queries are owner-only. Tenant-shared saved queries are readable
by tenant members and mutable only by tenant owner/admin. Documents require
room membership; viewer reads, while editor/owner mutates. Every saved-query or
document profile reference must resolve inside the same tenant and be usable by
the caller. Metadata queries apply tenant/owner predicates in SQLite rather
than filtering cross-tenant rows after retrieval.

## Ordered implementation slices

Each slice is a focused commit with its own tests. Later slices may not bypass
an earlier boundary for convenience.

### F2 — Protocol surface

Add pure-serde policy, schema-selector, effective-limit, and usage types. Add
stable rate/resource error codes and HTTP/WebSocket mappings. Define `None`
versus empty-list semantics in schema tests. Add typed audited operations for
policy and limit administration.

### F3 — Metadata persistence

Migrate connection policies, minimum tenant role, and monotonic policy
revision. Add tenant limit overrides with instance-admin mutation APIs and
transactional durable profile-count admission. Keep secret bytes behind
`SecretStore` and validate all enum/list data at the metadata boundary.

### F4 — Central evaluator

Introduce one server-internal authorization service and the conservative role
matrix from ADR-020. Replace handler-local permission decisions where the new
service applies. Make `ListAvailableOperations` call the same evaluator and add
exhaustive `OperationKind` coverage tests.

### F5 — Provenance and connection entry

Attach managed provenance to connections and descendants. Require profiles for
personal-network and all team modes; retain raw specs only for
personal-loopback. Authorize before credential resolution or driver open.
Prevent room/profile/connection tenant mismatches.

### F6 — Operation and SQL policy

Map database-facing operations to profile policy before driver dispatch. Add
engine-aware restricted-SQL classification and schema-reference extraction.
Fail closed for unknown/ambiguous SQL only when restrictions require it; avoid
parser cost for unrestricted profiles. Filter schema, search, completion, DDL,
and structured edit/import paths through the same policy.

### F7 — Revision and revocation

Re-evaluate policy revisions before each operation. Build indexes from
tenant/profile/principal to active runtime descendants. Implement immediate
cancel, rollback, cursor cleanup, close, and WebSocket notification for hard
revocation events, while ordinary edits apply after in-flight work.

### F8 — Rate admission

Add classified principal + tenant token buckets, configuration validation,
idle eviction, HTTP headers, WebSocket errors, bounded stream pacing, audit,
and deterministic controlled-time tests. Trusted-local exemption is the
default but can be enabled explicitly for testing.

### F9 — Tenant accounting

Implement reservation guards and effective-limit caching for profiles,
sessions, connections, queries, cursors, and retained bytes. Integrate every
cleanup path, including timeout, cancellation, cursor eviction, spill reaping,
disconnect, and graceful shutdown. Expose an authorized usage snapshot and
internal metric hooks; leave Prometheus/OTLP export to Phase J.

### F10 — Namespace closure

Move saved-query and document visibility predicates into metadata operations,
remove request-path use of cross-tenant lookup helpers, and validate every
profile reference against tenant plus effective permission. Cover FTS queries
so search cannot leak another principal's SQL or names.

### F11 — Public consumers

Add client-SDK methods and OpenAPI entries for policy, effective capabilities,
limits, usage, errors, and retry metadata. Preserve pure-serde protocol and
keep UI dependencies outside shared crates.

### F12 — Graduation

Run role/operation matrices, both SQL dialect restriction corpora, cross-tenant
ID tests, revocation tests, rate-burst/refill tests, quota races, cleanup/fault
tests, cursor spill accounting, namespace search tests, and all four deployment
trust combinations. Finish with fmt, workspace clippy with warnings denied,
workspace tests, and cargo-deny.

## Deferred without reopening ADR-020

- Per-principal connection ACL exceptions and reusable custom policy roles.
- Horizontal replicas and shared rate/resource coordination.
- A central identity or collaboration relay.
- Prometheus and OpenTelemetry exporters.
- Result replication; Phase G shares result references only.
