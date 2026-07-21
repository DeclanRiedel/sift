# IDE Parity And Provider Extensibility

Status: **forward-looking design input.** This note records product gaps and
constraints for incomplete phases only. It does not reopen completed phases or
claim that the designs below are locked ADRs.

The component ownership boundary is developed separately in
`docs/PLANS/core-plugin-boundary.md`.

The comparison baseline is JetBrains' current DataGrip feature overview:
<https://www.jetbrains.com/datagrip/features/>. The target is not imitation for
its own sake. Sift should combine serious database-IDE depth with Zed-like
collaboration, remote operation, and responsiveness while keeping the server as
the product.

## API finding

The current public API is a strong database-runtime substrate: authentication,
tenancy, sessions, connections, transactions, query streaming, cancellation,
cursors, schema introspection, search, completion, DDL, plans, structured row
edits, import/export, history, audit, rooms, and WebSockets are present.

It is not yet a complete database-IDE API. Forward work must cover:

- SDK parity for existing routes, including session lookup, connection listing,
  persistent room clients, and a non-buffering export consumer;
- cursor-based pagination for potentially large list and audit surfaces;
- generated typed OpenAPI plus generated or mechanically parity-checked SDKs;
- a real client/server version and capability handshake;
- reconnect discovery for sessions, queries, cursors, and shared room state;
- complete principal, tenant-member, room, and document administration;
- stable revision/precondition semantics for concurrent mutations.

Route/method parity testing reduces one class of OpenAPI drift, but the
hand-authored specification remains a second source of truth. Protocol types,
router registration, OpenAPI, and SDK reachability should ultimately derive
from one typed contract.

## DataGrip-class product gaps

Some DataGrip features are client presentation and need no special server
behavior: multi-cursor editing, themes, keymaps, localization, panel layout,
and result-grid virtualization. Sift's server contract must supply the data and
actions without dictating those UI choices.

The following are server-owned product semantics because thin and remote
clients must behave identically:

- normalized catalog dependency graph and navigation;
- relationship diagrams backed by stable object identities;
- schema snapshot comparison, migration generation, preview, and audited apply;
- table and query-result comparison with keys, tolerances, and bounded paging;
- dialect-aware parsing, formatting, diagnostics, quick fixes, usages, and
  refactoring;
- richer completion for aliases, CTEs, temporary objects, and objects created
  in the active document;
- general object-change DDL/DML generation and the remaining fidelity gaps;
- extensible export formats beyond CSV/TSV/JSON, without loading arbitrary
  extension code into the server process;
- governed AI explain/generate/edit operations with explicit context and the
  Phase F authorization evaluator;
- offline DDL sources, run configurations, ordered multi-script execution, and
  durable execution records.

## Workspace ownership decision

Files, local history, VCS integration, offline DDL sources, and run
configurations expose a load-bearing choice. If clients remain thin and
stateless, these are server-owned workspace resources. A remote client cannot
assume that its filesystem is the server's filesystem, and a locally useful
feature cannot silently disappear when connecting to a hosted instance.

The preferred model is:

1. Sift documents and run configurations are durable server resources.
2. A workspace may bind those resources to a repository through a server-side
   VCS adapter when the deployment permits filesystem access.
3. A desktop client may offer local import/export and native Git UI, but those
   conveniences do not become the only representation of product state.
4. VCS credentials use secret handles and scoped helpers; they never enter
   SQLite, protocol payloads, or audit logs.

An ADR must lock server-owned versus hybrid workspace semantics before a GUI
depends on either behavior.

## Database-provider strategy

### What exists today

PostgreSQL and SQL Server are first-party native Rust drivers. They implement a
small object-safe core `Driver` trait and engine extension traits. This gives
excellent control over async streaming, cancellation, schema fidelity, error
classification, pooling, and engine-specific fast paths.

The current boundary is intentionally closed: `Engine` is a two-variant enum,
the registry is keyed by it, connection specs are a closed union, and optional
features use `as_pg`/`as_mssql` downcasts. Adding more downcasts and enum
variants for every provider would make the server and public protocol the
bottleneck for third-party drivers.

### Recommended support tiers

1. **First-party native drivers.** Important providers receive custom Rust
   implementations in-tree. They offer the best latency, cancellation,
   introspection, bulk paths, diagnostics, and release confidence.
2. **Driver RPC plugins.** Other providers run out of process and implement a
   language-neutral, versioned Sift Driver Protocol over local stdio or a local
   socket. A plugin may be written in Rust, Go, Python, .NET, Java, or anything
   else; Sift inherits none of those runtimes as a core dependency.
3. **Optional compatibility bridges.** An ODBC bridge plugin can expose
   installed ODBC drivers. A JDBC bridge plugin can expose JDBC providers for
   users who explicitly install Java. Neither bridge ships in or is required by
   the Sift server. Bridges trade fidelity and deployment simplicity for broad
   reach and are not treated as equivalent to certified native drivers.

ODBC is not a dependency-free universal answer. It avoids Java but depends on a
platform driver manager, native vendor drivers, DSN/configuration behavior, and
often blocking APIs. JDBC has a larger provider ecosystem but requires a JVM
and a bridge process. Both flatten provider-specific schema, type, cancellation,
and bulk capabilities unless Sift defines its own richer contract above them.

### Driver Protocol design

The RPC protocol should preserve the existing reliable core while replacing
closed engine downcasts with discovery:

- protocol handshake with compatible version ranges;
- stable namespaced `provider_id`, separate display name and dialect id;
- manifest containing plugin version, executable, supported platforms,
  connection-configuration JSON Schema, secret-field declarations, and
  capability descriptors;
- opaque connection, transaction, query, and cursor handles scoped to one
  plugin process;
- open, ping, schema, begin, commit, rollback, execute, cancel, and close;
- framed streaming pages with explicit backpressure and size limits;
- structured errors, retryability, warnings, native type metadata, and
  correlation ids;
- optional capability families for savepoints, bulk transfer, notifications,
  process control, explain plans, database switching, schema invalidation, and
  provider-native operations;
- crash containment, deadlines, cancellation, health checks, restart policy,
  resource limits, and no in-process dynamic-library loading by default;
- conformance fixtures and certification levels so “loads” is not confused
  with “full IDE support.”

The wire encoding is less important than the semantics. A length-delimited
serde format over stdio is adequate initially and avoids committing to a large
runtime. The design should permit a more efficient encoding later through
handshake negotiation without changing the logical protocol.

### Engine identity and capability discovery

The public protocol needs an extensible provider identity before external
drivers can work. Do not simply change every `Engine` match into a free-form
string. Preserve well-known built-in engine/dialect identities for exhaustive
core behavior, and introduce a validated namespaced provider id plus a declared
capability set for runtime dispatch.

Clients ask the server what a provider supports. They do not infer support from
its name. Missing capabilities produce `UnsupportedForEngine`; they never
silently degrade a destructive or correctness-sensitive operation.

### Secrets and trust

Plugin manifests declare credential fields, but stored values remain opaque
`SecretStore` handles. The server resolves and passes the minimum credential
material only to the admitted driver process at connection time. Secret bytes
must never appear in manifests, metadata, logs, crash reports, or audit
operations.

Third-party plugins are code executed by the instance operator. The extension
system therefore needs install provenance, signatures/checksums, explicit
permissions, disabled-by-default network/filesystem capabilities where the
platform permits enforcement, update policy, and an operator-visible trust
state. Marketplace convenience must not imply sandbox guarantees the runtime
cannot actually enforce.

## General extension system

Driver plugins are one contribution type within a broader extension host. The
same manifest/version/permission/lifecycle foundation should support:

- database providers and tunnel adapters;
- connection hooks and credential brokers;
- export/import formats;
- SQL dialect, formatter, analyzer, and completion packs;
- commands and server-side operations;
- agent/MCP context providers and governed tools;
- client contributions such as panels or renderers through a separately
  versioned, declarative client contract.

Extensions do not bypass product invariants. Every user-visible action maps to
an audited core or namespaced extension `Operation`; authorization, rate and
resource admission, secret handling, timeout, and cancellation remain owned by
the server.

## Decisions to lock in future ADRs

- Provider identity and capability model replacing closed dispatch at the
  external boundary.
- Driver RPC transport, framing, compatibility, and backpressure.
- Plugin manifest, permissions, installation, trust, and update model.
- Whether process isolation is mandatory for all third-party server plugins.
- Namespaced extension operations and their audit/policy representation.
- Server-owned or hybrid workspace and VCS topology.
- Shared SQL semantic service and dialect-pack boundary.
- Certification tiers for providers and extensions.
