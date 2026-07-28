# Phase I — Extensibility Design

Status: **implemented and graduated on 2026-07-28.**

This document is the implementation contract for Phase I. The load-bearing
choices are graduated as ADR-022 and ADR-031 in `docs/DECISIONS.md`.

## Goal

Sift should accept independently released database providers, connection
mechanisms, tools, and automation without moving server invariants into plugins
or making an extension failure a server failure.

Phase I builds the extension foundation, a language-neutral driver RPC, and
governed automation. It does **not** make every database or desktop feature
automatically work merely because a compatibility driver can connect.

ODBC and JDBC bridges, DSN discovery, JVM management, and generic fidelity
claims are explicitly deferred. The first external provider is a purpose-built
conformance fixture, not an ODBC or JDBC adapter.

## Zed lessons, adapted for a server product

The reference is Zed's current extension model:

- a repository/package has a small, versioned `extension.toml`;
- most contributions are declarative and require no procedural code;
- procedural extensions use a narrow versioned host API;
- capabilities are explicit and can be restricted by the user/operator;
- local development extensions can override an installed release;
- publication is reviewable and tied to immutable source/package versions.

References:

- [Zed extension overview](https://zed.dev/docs/extensions)
- [Zed extension development and manifest](https://zed.dev/docs/extensions/developing-extensions)
- [Zed extension capabilities](https://zed.dev/docs/extensions/capabilities)
- [Zed extension API/WIT source](https://github.com/zed-industries/zed/tree/main/crates/extension_api)

Sift adopts the manifest, declarative-first, capability, development override,
and reviewed-registry patterns. It diverges where its workload requires it:

- Sift extensions execute beside a long-lived server, not inside a desktop UI.
- Database drivers own network connections and streams, so third-party drivers
  use supervised processes and a dedicated RPC rather than an editor-oriented
  WebAssembly callback API.
- Extensions are installed and granted by the instance operator. Tenant users
  may select from allowed contributions but cannot install server code.
- Sift's authorization, audit, quotas, cancellation, secrets, and durable
  product state remain core-owned.
- A process boundary contains crashes and wedges but is not advertised as an
  operating-system sandbox. Unrestricted native plugins remain operator-trusted
  code unless a future platform sandbox enforces otherwise.

## Scope

### Implement in Phase I

1. Extension package and manifest v1, validation, local installation, signed
   package provenance, development overrides, enable/disable, and inspection.
2. Extension registry and supervisor with generation-scoped processes, health,
   deadlines, bounded logs, restart backoff, quarantine, and atomic upgrade.
3. Extensible provider identity and capability discovery in public protocol v1.
4. Driver RPC v1 over stdio, a host adapter, SDK/schema artifacts, and an
   external conformance provider.
5. Namespaced extension operations, central policy classification, audit, and
   quota/timeout/cancellation integration.
6. Namespaced plugin storage and explicit retain/purge uninstall behavior.
7. Connection-pipeline contracts for tunnels, credential brokers, and hooks,
   plus conformance fixtures.
8. Governed command/tool contributions and `sift mcp`.
9. Declarative client contribution descriptors in the public API. Rendering
   waits for a product client; arbitrary plugin JavaScript is not introduced.

### Define now, activate in later phases

- SQL grammar, formatter, analyzer, completion, refactor, and schema-diff
  contribution descriptors. Phase K owns their semantic contracts.
- Workspace and VCS adapter descriptors. Phase L owns their state topology.
- Themes, icon themes, and purely local presentation. The future client owns
  their rendering and may follow Zed more directly.
- A public marketplace service or registry index. Phase I supports local
  packages with package-level signatures and never makes server startup depend
  on a registry.

### Explicitly deferred

- ODBC and JDBC bridge implementations;
- automatic DSN, driver-manager, JAR, classpath, or JVM discovery;
- claiming IDE-level support from connection success alone;
- loading third-party dynamic libraries into `sift-server`;
- arbitrary plugin HTTP routes, WebSocket routes, UI code, or SQL against core
  metadata;
- transparent live migration of open connections between plugin versions;
- a cross-platform mandatory OS sandbox. Honest trust labeling ships first.

## Architectural rings

### Mandatory core

Core remains responsible for:

- identity, tenancy, rooms, authorization, capability narrowing, and approvals;
- operation classification, audit, redaction, correlation, history, and policy;
- secret handles and scoped secret delivery;
- sessions, connections, transactions, queries, cursors, result paging, and
  shared-result ownership;
- timeouts, cancellation, rate limits, quotas, retained-byte accounting, and
  shutdown;
- extension installation state, grants, lifecycle, storage quotas, and
  revocation;
- public HTTP/WebSocket/OpenAPI contracts and plugin RPC contracts;
- validation of every plugin input/output and canonical provider-neutral data.

Plugins never become alternate implementations of these concerns.

### First-party bundle

PostgreSQL and SQL Server remain native in-process Rust drivers in Phase I.
They register through the new provider registry but are not forced through RPC.
This preserves their latency and avoids destabilizing two proven drivers while
the external protocol is being established.

Baseline CSV/TSV/JSON, direct SSH remote bootstrap, and current completion/DDL
implementations remain bundled. Architectural ownership does not require
repackaging all first-party code as plugins in this phase.

### Optional extensions

Third-party server code is out of process. Declarative contributions need no
process. A package may contain multiple contribution declarations but at most
one server executable per target in manifest v1; the supervisor multiplexes
that executable's contributions.

Data-bearing executable contributions run in a separate process generation per
tenant. Database providers, tunnels, brokers, hooks, commands, tools, and agent
context never share one plugin process across tenants. Package validation and
instance-administration work may use an instance-scoped generation that
receives no tenant data or credentials. A manifest cannot request broader
process scope.

## Identity model

### Extension and contribution identifiers

Identifiers are immutable after publication.

- `ExtensionId`: `publisher/name`
- `ProviderId`: `publisher/provider`
- `DialectId`: `publisher/dialect`
- `ContributionId`: `<extension-id>/<kind>/<local-name>`

Each segment is 1–63 ASCII lowercase characters, starts and ends with an
alphanumeric character, and otherwise permits `-`, `_`, and `.`. The complete
identifier is at most 191 bytes. The `sift/*` publisher namespace is reserved
for first-party components.

Display names are mutable localized text and are never dispatch keys.

### Built-in compatibility

Public protocol v1 introduces provider-neutral identities and capabilities.
The pre-release `Engine::{Postgres, SqlServer}` wire shape is replaced rather
than retained as a compatibility layer; there are no external users requiring
a historical codec.

Built-ins map as follows:

| Existing engine | Provider id | Dialect id |
| --- | --- | --- |
| PostgreSQL | `sift/postgres` | `sift/postgresql` |
| SQL Server | `sift/sql-server` | `sift/tsql` |

New public shapes use:

```text
ProviderRef {
    provider_id,
    dialect_id,
    provider_version,
}
```

A connection profile stores `provider_id`, provider configuration JSON, and
opaque credential handles. The server validates configuration against the
provider's declared JSON Schema before persistence. Secret-shaped fields are
not permitted in provider configuration; credential fields have a separate
schema and remain in `SecretStore`.

Schemas use JSON Schema Draft 2020-12, are bounded to 256 KiB and a configured
validation-depth ceiling, and may reference only files declared inside the same
package. Remote `$ref` resolution is forbidden. Credential schemas mark fields
with `x-sift-secret = true`; ordinary settings/config schemas reject that
annotation and secret-shaped field names.

All protocol v1 clients use provider-neutral shapes. No external provider
pretends to be PostgreSQL or SQL Server.

### Capability discovery

Clients render from descriptors returned by the server. They do not infer
features from provider or dialect names.

Capability ids are versioned strings such as:

- `driver.core@1`
- `driver.transactions@1`
- `driver.savepoints@1`
- `driver.schema.shallow@1`
- `driver.schema.deep@1`
- `driver.cancel@1`
- `driver.bulk@1`
- `driver.notifications@1`
- `driver.process-control@1`
- `driver.explain@1`

Each capability carries a typed limits/configuration object. Unknown
capabilities are ignored by old clients. Missing capabilities produce
`UnsupportedForEngine`; Sift never silently emulates destructive or
correctness-sensitive behavior.

Provider quality labels are separate from capabilities:

- **Compatible** — handshake, manifest, health, and failure conformance.
- **Query capable** — connect, ping, execute, page, cancel, close, values.
- **Transactional** — begin/commit/rollback and failure-state conformance.
- **IDE capable** — deep schema plus the declared IDE capability corpus.
- **Sift certified** — maintained fixtures, security review, release matrix,
  performance budget, and signed provenance.

Only Sift assigns certification. A manifest may declare capabilities but cannot
self-assign a certification level.

## Extension manifest v1

The package root contains `sift-extension.toml`. Unknown fields are rejected in
schema v1 so misspelled security or contribution declarations fail closed.
Package and compatibility versions are strict SemVer without an implicit
leading `v`.

Required package metadata:

```toml
schema_version = 1
id = "acme/example"
name = "Example"
version = "1.2.3"
authors = ["Acme"]
description = "Example Sift provider"
license = "Apache-2.0"
repository = "https://example.invalid/acme/example"
minimum_sift_version = "0.2.0"

[compatibility]
public_protocol = { minimum = 3, maximum = 3 }
extension_rpc = { minimum = 1, maximum = 1 }
driver_rpc = { minimum = 1, maximum = 1 }
```

The manifest also declares:

- target artifacts with OS, architecture, SHA-256, byte length, and executable
  relative path;
- contributions keyed by immutable local id;
- requested host capabilities and whether each is required or optional;
- provider configuration and credential JSON Schema paths;
- lifecycle mode (`lazy` or `eager`), readiness deadline, and idle policy;
- package data files with hashes;
- optional homepage, support, and source provenance.

Paths are relative, normalized, cannot escape the package, and cannot be
symlinks at install time. Executables and data are installed content-addressed
under an operator-owned extension state directory.

Target artifacts must be directly executable, self-contained programs. Core
does not invoke a shell, discover an interpreter, or install a language
runtime. Authors may use any implementation language if the published target
artifact carries its own runtime requirements without making them Sift
dependencies.

Representative contribution and grant syntax:

```toml
[[capabilities]]
kind = "database.connect"
required = true

[[capabilities]]
kind = "storage.kv"
required = false
max_bytes = 1048576

[[contributions.database_provider]]
id = "example-db"
provider_id = "acme/example-db"
dialect_id = "acme/example-sql"
config_schema = "schemas/provider-config.json"
credential_schema = "schemas/provider-credentials.json"
capabilities = ["driver.core@1", "driver.cancel@1"]

[[artifacts]]
target = "linux-x86_64"
path = "bin/example-provider"
sha256 = "<64 lowercase hex characters>"
byte_length = 123456
```

Extension settings are instance- or tenant-scoped schema-validated JSON.
Secret fields are forbidden in settings schemas and must use the declared
credential/secret-handle path.

### Package archive

The distributable format is a ZIP archive named `*.sift-extension`. Phase I
accepts stored and deflated entries only. It rejects absolute paths, parent
traversal, duplicate normalized paths, case-fold collisions, symlinks,
hardlinks, devices, more than 4,096 entries, and configured compressed or
expanded byte ceilings.

The archive contains only:

- `sift-extension.toml`;
- `sift-extension.lock`, RFC 8785 JSON Canonicalization Scheme (JCS) bytes
  containing the manifest digest and listing `sift-extension.toml` plus every
  payload file's normalized path, SHA-256, and byte length (only the lock and
  signature files are excluded from the list);
- `sift-extension.sig`, an optional unpadded-base64url Ed25519 signature over
  the exact lock bytes;
- the declared artifacts and data files.

Verified provenance requires a signature from an operator-trusted publisher
key. First-party publisher keys may be embedded. Local unsigned installation
requires an explicit admin action and records the exact archive SHA-256.
Extraction happens into a private staging directory, every file is rehashed,
and an atomic rename selects the content-addressed install. For verified
packages, the raw lock signature is checked before the manifest or payload is
trusted or activated. Validation also rejects lock bytes that do not exactly
match the RFC 8785 serialization of their parsed value; verifiers do not
silently rewrite signed input. The manifest entry's digest must equal the
separate `manifest_sha256` field. Undeclared archive entries are rejected.

Package files and database selection cannot share one transaction, so install
uses an explicit recovery protocol. The host fully writes and syncs a private
staging directory, atomically renames it on the same filesystem to an immutable
archive-digest directory, and only then selects that digest in an immediate
SQLite transaction. Startup reconciliation removes abandoned staging
directories, tolerates unreferenced immutable packages until garbage
collection, and quarantines a selected record whose immutable directory is
missing or fails revalidation. Activation never points at staging bytes.

### Contribution kinds reserved by v1

- `database_provider`
- `tunnel_provider`
- `credential_broker`
- `connection_hook`
- `import_format`
- `export_format`
- `dialect_pack`
- `command`
- `governed_tool`
- `agent_context`
- `client_panel`

Reservation means the manifest can identify and inspect the contribution. A
kind is invocable only when its host contract is implemented and negotiated.

Manifest v1 has no executable extension-to-extension dependency graph. A
declarative contribution may name a provider or dialect supplied elsewhere and
is reported inactive when that target is absent. Plugin code cannot import,
start, or call another plugin directly; composition goes through a typed core
contract. Only one installed extension may own a given provider or contribution
id, and collisions fail installation.

### Provenance and development mode

Install records distinguish:

- `bundled`: shipped and signed by Sift;
- `verified`: package signature chains to an operator-trusted publisher key;
- `local`: checksum-pinned local archive, unsigned;
- `development`: live local path override.

Development overrides follow Zed's useful workflow but are restricted to
personal/local instances by default. Hosted development overrides require an
explicit instance-admin setting, are shown as unverified, and cannot be enabled
silently by tenants.

Package signature verification covers the exact manifest plus a canonical file
hash list. Registry metadata is not a substitute for package verification.
Phase I has no implicit dependency resolver and never downloads an undeclared
runtime. A future signed index may point to exact package hashes, but package
signature and operator policy still authorize installation.

Publisher keys are public trust records scoped to an exact publisher namespace,
with fingerprint, validity window, and revocation state. Trust changes are
instance-admin-only and audited. A package signed solely by a newly revoked key
is disabled/quarantined on trust-policy reload unless an explicit emergency
override pins its exact archive digest. Reusing one extension id/version with a
different digest is always rejected. Downgrade is allowed only through explicit
rollback to a previously verified installed digest.

## Operator configuration

Extension configuration has explicit safe defaults:

- extension hosting is enabled but no optional package is installed or enabled;
- unsigned local packages are denied until an instance-admin policy allows
  them;
- development paths are denied on team deployments until separately enabled;
- the extension state directory is operator configured/defaulted locally and
  private;
- publisher keys and package origins are never guessed;
- process, frame, package, storage, log, restart, and migration ceilings have
  server hard maxima;
- tenant availability defaults to denied until an instance admin allows a
  contribution.

Manifest settings are not environment-variable passthrough. Core supplies only
the validated settings fields and host facts defined by Extension RPC.

## Host capabilities and grants

Requested capabilities are inert until an instance administrator grants them.
Effective grants are the intersection of manifest request, instance policy,
tenant allowlist, and per-operation admission.

Optional extensions install disabled. Enabling fails if a required requested
capability is not granted; missing optional grants remove that host service
from `welcome`. Revoking a grant stops the process, invalidates its handles, and
requires a fresh generation/handshake before remaining contributions can run.

Initial capability vocabulary:

- `database.connect`: connect only to the admitted profile endpoint;
- `secret.receive`: receive named credential fields for one admitted call;
- `network.connect`: connect to an operator-scoped host/port pattern;
- `network.listen.loopback`: create a host-approved loopback tunnel endpoint;
- `filesystem.data`: access the extension's own quota-bound data directory;
- `filesystem.read`: read explicitly configured paths;
- `filesystem.write`: write explicitly configured paths;
- `process.spawn`: spawn an explicitly matched command/argument pattern;
- `http.fetch`: use a host-mediated HTTPS request restricted by origin/path;
- `storage.kv`: use namespaced opaque plugin storage;
- `operation.invoke`: invoke an enumerated core operation through dispatch;
- `event.publish`: publish an enumerated sanitized event type;
- `tool.register`: expose declared commands/tools.

Native executables may technically possess ambient OS capabilities that Sift
cannot enforce portably. Their runtime record therefore has an `isolation`
field:

- `host_enforced`
- `platform_sandboxed`
- `process_only`

Phase I guarantees `process_only` everywhere and may report stronger platform
enforcement where it is actually active. The UI/API must not describe
`process_only` as sandboxed. Enabling a `process_only` executable requires an
explicit operator trust decision; host grants constrain host-mediated services
but cannot truthfully prevent that executable from using ambient OS access.

## Extension RPC v1 and Driver RPC v1

Extension RPC is the common process envelope. Driver RPC is one versioned
method family carried inside it. Tunnel, broker, hook, command, and tool
contracts use the same lifecycle, framing, identity, deadline, and capability
rules without pretending to be database drivers.

### Transport and framing

- local child-process stdio only;
- stdin/stdout reserved for RPC; stderr is captured as bounded diagnostic text;
- four-byte unsigned big-endian frame length followed by UTF-8 JSON;
- negotiated maximum frame size, default 8 MiB and hard ceiling 16 MiB;
- no plugin-selected socket, listener, shell command, or inherited secret env;
- malformed length, invalid JSON, unknown mandatory message, or output beyond
  limits is a protocol violation and terminates that process generation.

The supervisor executes the exact verified artifact without a shell, clears
the inherited environment, uses a private working/temp directory, and supplies
host facts through `welcome`. No `.env`, bearer token, `HOME`, credential, or
ambient Sift configuration is inherited.

JSON is selected for v1 because it is language-neutral, inspectable, fixture
friendly, and adequate behind bounded pages. A future encoding requires a new
negotiated RPC version; v1 does not include a speculative encoding switch.

Default supervisor limits are:

| Limit | Default |
| --- | ---: |
| Handshake/readiness | 10 seconds |
| Heartbeat interval | 5 seconds |
| Missed heartbeats before unhealthy | 3 |
| Cancel grace before kill | 2 seconds |
| Restart budget | 5 failures per rolling 10 minutes |
| Restart backoff | 250 ms exponential, capped at 30 seconds, plus jitter |
| Structured/stderr log input | 64 KiB/s per generation |
| Retained diagnostic ring | 1 MiB per generation |
| Concurrent generations per extension and tenant | 1 active + 1 upgrade candidate |
| Concurrent tenant generations per extension | 32 |
| Concurrent extension generations per instance | 256 |
| Idle lazy-generation lifetime | 5 minutes |
| Single storage value | 1 MiB |
| Storage migration working set | 64 MiB |

Operators may lower these values. Raising them is bounded by server hard
ceilings so a manifest cannot request unbounded startup, cancellation, logs, or
restarts. Tenant generations start lazily, idle generations are evicted only
when they own no live handles, and admission fails with a stable resource error
when a limit cannot be satisfied. Upgrade candidates consume the explicitly
reserved second per-tenant slot rather than bypassing instance ceilings.

Extension RPC message kinds are `hello`, `welcome`, `request`, `response`,
`stream`, `credit`, `cancel`, `heartbeat`, `log`, and `shutdown`. Unknown
optional fields are ignored; unknown message kinds or mandatory fields are
protocol violations. Host-to-plugin and plugin-to-host requests have distinct
id namespaces so capability-mediated host service calls cannot collide.

### Handshake

The plugin must send `hello` before any other frame:

```text
hello {
  extension_rpc_range,
  method_family_ranges,
  extension_id,
  extension_version,
  manifest_sha256,
  process_nonce,
  contributions,
}
```

The host validates that identity and contribution discovery exactly match the
installed manifest, then returns:

```text
welcome {
  selected_extension_rpc_version,
  selected_method_family_versions,
  process_generation,
  granted_capabilities,
  limits,
  heartbeat_interval,
}
```

No compatible version, identity mismatch, or manifest mismatch fails startup.
The executable declares a maximum concurrent request count in `hello`; the host
may lower it. Requests beyond the negotiated concurrency remain queued in core
and continue to consume normal admission limits.

### Host service calls

An executable can call only versioned host services covered by its effective
grant:

- namespaced storage get/put/delete/compare-and-swap;
- bounded HTTPS fetch;
- approved process spawn;
- sanitized configuration lookup;
- core operation invocation;
- sanitized event publication;
- structured logging.

The host validates the grant and the current operation context on every call.
Host services do not return arbitrary principal, metadata, environment, or
secret data. Secret delivery is a dedicated one-call response initiated by the
core connection pipeline, not a general lookup API.

To prevent re-entrant policy and deadlock surprises, a plugin may not invoke
the same operation/action currently dispatching it. Nested core operations have
a maximum depth of one in v1 and receive a child correlation id.

### Message model

Every request carries:

- monotonically unique request id within the process generation;
- contribution id;
- method;
- typed payload;
- correlation id;
- absolute deadline;
- admitted tenant/room context reduced to what the plugin needs;
- optional stream id.

Request ids, stream ids, and opaque 128-bit handles are fixed-width lowercase
hex strings on the JSON wire, never JSON numbers. The host is authoritative for
deadline enforcement; the absolute deadline sent to a plugin is advisory and
cannot extend the host timer.

Responses are `ok`, structured `error`, or stream start. Driver errors preserve
stable `Code`, retryability, provider-native code, sanitized message, and
warnings. Unknown error codes map to `Internal` without discarding the native
diagnostic from protected operator logs.

Handles are opaque random 128-bit values scoped to a contribution and process
generation. Connection, transaction, query, and cursor handles never cross
generations or tenants and are never accepted from a different provider.

### Driver RPC core methods

Driver RPC v1 requires:

- `open`
- `ping`
- `schema`
- `begin`
- `commit`
- `rollback`
- `execute`
- `cancel`
- `close`

Optional methods are callable only when their capability family is negotiated.
Provider-native escape hatches are not exposed directly to clients in v1; they
must graduate as typed capability families and audited operations.

### Streaming and backpressure

`execute` returns a query handle and stream id. The host grants byte credit.
The provider may send `NextResult`, `Rows`, `Done`, or `Error` frames only while
credit is available. Credit is charged by encoded frame bytes before receipt
and replenished after the core cursor registry accepts the page.

Data-stream credit is independent from a small bounded control-frame allowance,
so cancellation, terminal errors, heartbeats, and credit updates cannot
deadlock behind result data. Initial data credit is at least one negotiated
maximum legal result frame. A sender computes charge from the complete encoded
length-prefixed frame; exceeding either credit pool is a protocol violation.

The provider must split pages to fit the negotiated frame and row/byte limits.
A single value that cannot fit returns `ResultTooLarge`; it is not fragmented
into an unbounded side channel.

The host remains the sole owner of public cursors, spill files, room result
references, and client pacing. Plugin stream ids are internal.

### Deadlines, cancellation, crash, and restart

- every request has a host deadline;
- deadline first sends RPC `cancel`, then kills the process after a short grace
  period if work does not stop;
- cancellation has a bounded acknowledgement but success does not imply the
  database connection is reusable;
- the provider reports connection disposition as `reusable`, `invalidated`, or
  `unknown`; the host treats `unknown` as invalidated;
- process exit invalidates every generation-scoped handle;
- idempotent discovery/health work may retry once on a fresh generation;
- open transactions, writes, and query execution are never automatically
  replayed;
- restart uses exponential backoff with jitter and a fixed attempt budget;
- repeated failure quarantines the extension until operator action or an
  explicitly configured cooldown.

No RPC future runs inline in an Axum handler. The adapter preserves ADR-013's
spawn, timeout, cancel, and containment boundary.

## Code and persistence ownership

Phase I introduces these boundaries:

- `sift-protocol`: public provider, extension management, operation, approval,
  and declarative client types only; it remains pure serde/schemars.
- `sift-extension-protocol`: pure serde types for strict manifest v1,
  Extension RPC v1, Driver RPC v1, capabilities, and golden fixtures; no Tokio,
  process, filesystem, network, or server dependency.
- `sift-plugin-host`: server-internal package validation/install orchestration,
  grants, supervisor, framing, generations, storage facade, and RPC adapters.
- `sift-driver-api`: the locked built-in `Driver` trait remains unchanged.
- `sift-server`: a new provider-neutral internal registry stores
  `Arc<dyn DatabaseProvider>`. `BuiltinProviderAdapter` wraps the existing
  `Driver`; `RpcProvider` uses `sift-plugin-host`.

The provider-neutral internal trait uses `ProviderId` and validated provider
configuration rather than `EngineConnectionSpec`. It preserves the logical
ADR-017 verbs and server-owned handles. This is an adapter boundary, not an
unreviewed mutation of the locked built-in trait.

Provider/contribution registry mutation is serialized, builds a complete
validated immutable snapshot, and atomically swaps that snapshot for readers.
Hot query dispatch never holds the installation lock. A selected connection
captures its provider generation so a later registry swap cannot reroute an
existing handle.

Metadata migrations add core-owned tables for:

- installed extension versions and selected version;
- exact manifest/hash/provenance and lifecycle state;
- instance grants and tenant allowlists;
- contribution/provider indexes;
- extension storage blobs with revision and byte accounting;
- storage schema/migration state;
- approval records and consumption;
- orphaned-data retention/purge state.

Process ids, live handles, plaintext secrets, and raw package executable bytes
do not enter SQLite. Generation health is runtime state; only bounded terminal
diagnostics and quarantine metadata are durable. Plugin secrets remain in
`SecretStore` under namespaced opaque handles.

Extension storage defaults to `(extension_id, tenant_id)` scope. Explicit
instance-scoped storage is available only to instance-administration
contributions and cannot be opened by a tenant process. Quotas and backup/purge
accounting apply independently to each namespace.

## Public protocol transition

Phase I publishes application protocol version 1. The earlier numeric version
and engine-specific shapes were pre-release implementation details with no
users, so the server does not retain or advertise them.

- protocol-v1 provider-neutral DTOs carry provider refs, capability
  descriptors, and schema-validated configuration;
- internal state always uses provider ids;
- clients never receive an `Engine` value as their capability source.

The handshake selects one version for the whole connection as required by
ADR-016. The initial published server range is `[1, 1]`.

## Connection pipeline

The core owns a deterministic connection state machine:

1. validate profile and provider configuration;
2. run `pre_resolve` hooks without secrets;
3. ask the selected credential broker for named credentials, if configured;
4. establish the selected tunnel and receive a loopback endpoint lease;
5. construct the final provider request;
6. deliver only the selected credential fields to the provider;
7. call driver `open`;
8. run `post_connect` hooks with sanitized server information;
9. publish the core-owned connection handle.

Cleanup runs in reverse order. Tunnel and broker leases are generation-scoped
and never stored as plaintext metadata.

Hook ordering is explicit `(priority, extension_id, contribution_id)`. Equal
priority is deterministic, not install-order dependent. Hooks return validated
patches to an allowlisted configuration surface; they do not mutate profiles or
call drivers directly. A required hook failure aborts the connection. Optional
hook failure is surfaced as a warning and audited.

Credential brokers return secret bytes over the private RPC response for the
single operation. The server stores only broker configuration and opaque
handles. Secret bytes never enter environment variables, arguments, logs,
crash reports, operation payloads, or plugin storage.

### Connection contribution contracts

Credential broker `resolve` receives the broker's non-secret configuration,
required credential field names, admitted principal/profile context, and opaque
broker secret handles. It returns a map of secret bytes plus optional expiry.
Core re-resolves expiring credentials on a later connection attempt; it never
persists the returned bytes.

Tunnel provider `open_tunnel` receives the admitted final database endpoint,
non-secret tunnel configuration, and separately scoped tunnel credentials. It
returns only a loopback TCP endpoint, opaque lease id, and optional expiry.
Core probes the endpoint, tracks the lease, and calls `close_tunnel` during
reverse cleanup. The provider cannot rewrite the logical database identity or
return a non-loopback listener in v1.

Hook stages are:

- `pre_resolve`
- `post_resolve`
- `pre_connect`
- `post_connect`
- `connection_failed`
- `pre_close`
- `post_close`

Hooks receive stage-specific sanitized data. Only `pre_resolve` and
`pre_connect` may return patches, and their schemas enumerate patchable fields.
No hook sees credential bytes. Observation stages cannot alter success/failure.

The Phase H SSH remote-server topology is separate from a database tunnel
provider. It is not reimplemented as a Phase I plugin.

## Namespaced operations

Protocol v1 includes:

```text
ExtensionOperation {
    extension_id,
    contribution_id,
    action,
    classification,
    target_kind,
    target_id?,
    sanitized_arguments,
}
```

`action` and `target_kind` use the same validated segment grammar as ids.
`sanitized_arguments` is schema-validated, size-bounded JSON with manifest
fields marked `secret`, `sql_text`, `row_data`, or `audit_safe`. Only
`audit_safe` projections reach durable audit.

Each action declares exactly one core classification:

- `read`
- `execute_read`
- `write`
- `destructive`
- `administrative`

The Phase F evaluator maps the classification to existing tenant, room, and
connection policy. An extension cannot choose a weaker classification at
runtime. Installation rejects duplicate actions or actions with an unknown
classification.

Extension actions always pass through core dispatch. There are no raw plugin
routes and no plugin-created audit rows.

## Storage, migrations, and uninstall

Core provides:

- quota-bound key/value and opaque-blob storage scoped by extension id;
- compare-and-swap revision semantics;
- manifest-declared storage schema version;
- a bounded migration RPC that writes a staged copy before the new version
  becomes active;
- export/import of opaque extension data for backup;
- separate `SecretStore` handles for plugin-owned secrets.

Extensions cannot execute SQL against metadata SQLite or see another
extension's namespace.

Keys, values, namespace totals, and migration working sets have independent
hard ceilings. Values above the single-value ceiling are rejected before a
SQLite write transaction begins. Upgrade staging is copy-on-write: unchanged
values reference immutable content-addressed blobs, while a staged namespace
records only changed keys and tombstones. Blob reference counts and the active
namespace pointer change in the same immediate transaction. Startup
reconciliation deletes unreachable blobs only after proving that no active,
staged, rollback, or orphaned namespace references them.

Disabling stops invocation but retains package and data. Uninstall removes the
package and grants but retains data as orphaned state. Purge is a separate
explicit administrative operation that removes extension data and secret
handles after showing impact. Rollback never asks old code to read a storage
version it did not declare compatible.

Storage is generation-versioned during upgrade. Core snapshots the old
namespace, the new executable migrates into a staged namespace, and package +
storage pointers switch in one metadata transaction only after migration and
health pass. Failure discards staged data. The previous package/storage pair is
retained for rollback until the configured retention gate passes.

Provider configuration schemas carry their own integer schema version. If an
upgrade no longer accepts persisted profiles, the extension must provide a
bounded staged configuration migration. Core validates every migrated profile
against the new schema and switches them in the same activation transaction.
Credential handles/bytes are never passed to configuration migration.

## Lifecycle and upgrades

State progression:

```text
installed -> disabled -> starting -> ready -> degraded
                         |          |        |
                         +----------+--------+-> quarantined
disabled/quarantined -> uninstall -> orphaned data -> explicit purge
```

Contribution discovery comes from the validated manifest and does not require
starting code. Executables default to lazy startup. Eager startup is allowed
for operator-selected infrastructure contributions.

Upgrade installs content-addressed bytes beside the current version and
validates the manifest/signature. Core starts the candidate for handshake and
stateless health, blocks new work for that extension, and drains the old
generation by the operator's deadline. Remaining queries are canceled and
connections closed; there is no live connection migration. Core then stages
storage migration, health-checks against staged state, and atomically selects
the new package/storage pair before stopping the old generation. Failed
activation leaves the old pair selected and reopens admission.

## Commands, tools, and MCP

### Command/tool contributions

A command declares:

- immutable id, display metadata, and JSON input/output schemas;
- operation classification;
- required context (instance, tenant, room, profile, connection, document);
- timeout and result-size ceiling;
- whether it is interactive, schedulable, or MCP-exposable;
- audit-safe argument/result projections.

Tools invoke the same core dispatch used by HTTP/WebSocket clients. They do not
receive an implicit administrator identity.

### `sift mcp`

`sift mcp` is a stdio MCP server backed by a normal authenticated Sift client
session. It lists only tools currently available to that principal and context.
Core operations are exposed from explicit tool descriptors, not by blindly
serializing every `Operation` variant.

Because stdin/stdout belong to MCP, credentials cannot arrive through MCP
frames or command arguments. The proxy obtains its Sift session through the
normal SDK token provider backed by an OS secret backend or a permission-checked
token file. Hosted refresh follows the existing auth session flow. Local
trusted bypass is available only when the target server independently verifies
the loopback transport; `sift mcp` does not assert trusted-local status.

Order of admission:

1. authenticate principal;
2. resolve tenant/room/connection context;
3. evaluate Phase F authorization and connection policy;
4. apply rate/quota/deadline admission;
5. classify approval requirement;
6. obtain or verify a narrowly bound approval;
7. execute through normal operation dispatch;
8. sanitize result and record audit.

Approval defaults:

- reads: policy-controlled, no extra prompt by default;
- query execution classified read-only by the SQL policy: configurable;
- writes: explicit per-call approval by default;
- destructive/administrative: explicit per-call approval and never
  auto-approved by an extension.

An approval is bound to principal, operation/action id, target context, input
fingerprint, and short expiry. It cannot be replayed for changed SQL, another
connection, or another tenant.

Approval records are one-use, expire after five minutes by default, and have a
hard maximum lifetime of fifteen minutes.

When approval is required, the tool call returns a structured
`approval_required` result containing an opaque request id. Approval occurs
through the normal authenticated Sift API/client surface; retry consumes the
one-use approval. The plugin and MCP client never mark their own request
approved.

MCP resources and prompts may expose sanitized schema/document context through
the same authorization and byte budgets. Credentials, secret handles, raw
audit internals, and unrestricted result sets are never resources.

Phase I makes Sift an MCP server; it does not run arbitrary third-party MCP
servers as a privileged shortcut. Agent context and governed tool contributions
use Extension RPC and the same policy path.

## Declarative client contributions

Phase I defines descriptors for future thin clients:

- command palette items;
- context-menu actions;
- read-only detail panels;
- schema-driven forms;
- bounded tables and key/value views;
- links to core resources and operation results.

Descriptors reference registered operations and protocol fields. They cannot
execute arbitrary JavaScript, issue network requests, read local files, retain
credentials, or mutate server state outside an operation. A client may decline
an unsupported descriptor without affecting server behavior.

## Extension management API

Instance administrators can:

- inspect installed packages, locally validated candidates, and exact
  provenance;
- validate/install a local package;
- enable or disable an extension;
- grant or revoke requested capabilities;
- allow contributions per tenant;
- inspect generations, health, bounded logs, and quarantine reason;
- activate/rollback an update;
- uninstall, restore orphaned data, or explicitly purge;
- inspect provider capabilities and certification evidence.

Tenant administrators may only select contributions allowed by the instance.
Ordinary users see capability descriptors relevant to their authorized
context.

Every mutation is a typed audited operation. Package bytes are never accepted
from a URL the server invents or guesses.

The protocol-v1 HTTP surface remains under the stable `/v1` route prefix:

- `GET /v1/providers`
- `GET /v1/extensions`
- `GET /v1/extensions/{publisher}/{name}`
- `POST /v1/extensions/validate`
- `POST /v1/extensions/install`
- `PUT /v1/extensions/{publisher}/{name}/selection`
- `PUT /v1/extensions/{publisher}/{name}/grants`
- `PUT /v1/extensions/{publisher}/{name}/tenants/{tenant_id}`
- `POST /v1/extensions/{publisher}/{name}/rollback`
- `DELETE /v1/extensions/{publisher}/{name}`
- `POST /v1/extensions/{publisher}/{name}/purge`
- `GET /v1/extensions/{publisher}/{name}/diagnostics`
- `POST /v1/extension-actions/invoke` (ids are typed body fields)
- `POST /v1/operation-approvals`
- `POST /v1/operation-approvals/{approval_id}/approve`

Install/validate stream a bounded archive to private staging rather than
buffering it in an Axum handler. Mutation resources carry revisions and require
an expected revision/`If-Match`; retries cannot duplicate activation, grants,
purge, or approval. Diagnostics are bounded and redacted. The OpenAPI and
client-SDK coverage manifests include every route before Phase I graduation.

Protocol v1 includes typed `ManageExtension`, `InvokeExtension`, and
`ApproveOperation` operation variants. HTTP transport auditing remains in
addition to, not instead of, these semantic operations.

## Conformance and graduation

### Manifest/package corpus

- path traversal, symlink, duplicate id, hash, signature, schema, target, and
  compatibility rejection;
- requested versus granted capability intersection;
- dev override and hosted restriction;
- atomic install, update rollback, uninstall retention, and purge.

### RPC fault corpus

- fragmented reads and writes;
- oversized, malformed, out-of-order, duplicate, and unknown frames;
- identity/manifest mismatch;
- credit violation and output flood;
- slow handshake, missed heartbeat, request timeout, ignored cancellation;
- crash during open, query, transaction, stream, and close;
- stale/cross-generation/cross-provider handle rejection;
- restart budget and quarantine;
- secret-shaped stderr/log/error redaction.

### Provider corpus

- value and typed-null round trips;
- multi-result ordering and terminal frames;
- backpressure under a slow consumer;
- transaction and cancellation disposition;
- shallow/deep schema fidelity;
- stable error mapping and native diagnostic protection.

### Governance corpus

- every extension action has one classification;
- deny wins across instance, tenant, room, connection, and approval policy;
- no extension action bypasses rate, quota, timeout, cancellation, or audit;
- approval binding and replay rejection;
- MCP lists only currently authorized tools;
- plugin crash cannot wedge HTTP readiness or unrelated providers.

Phase I graduates only when both built-in providers pass through the new
provider-neutral registry, an external process conformance provider passes the
RPC corpus, and the full repository gates remain green.

## Ordered implementation milestones

1. **I0 — Contract crates.** Add pure-serde extension/manifest/provider/RPC
   types and golden/schema fixtures. Publish protocol v1.
2. **I1 — Package registry.** Validate/install content-addressed packages,
   provenance, grants, dev overrides, inspection, and metadata migrations.
3. **I2 — Supervisor.** Process generations, stdio framing, health, deadlines,
   logs, restart/quarantine, update/drain/rollback.
4. **I3 — Provider-neutral core.** Provider ids/capabilities/config schemas,
   dynamic registry, and built-in adapters.
5. **I4 — Driver RPC.** Host adapter, SDK/schema, credit streaming, handle
   safety, cancellation, and external conformance provider.
6. **I5 — Operations and storage.** Namespaced actions, policy mapping, audit
   projection, quota-bound storage, migration, uninstall/purge.
7. **I6 — Connection pipeline.** Tunnel/broker/hook contracts and fault
   fixtures; no ODBC/JDBC bridge.
8. **I7 — Governed automation.** Command/tool registry, approval records, and
   `sift mcp`.
9. **I8 — Declarative discovery.** Management/capability APIs and client
   descriptors; later-phase contribution kinds remain non-invocable.
10. **I9 — Graduation.** Fault/security matrices, public compatibility and
    certification artifacts, operational docs, and ADR conformance review.

## Decisions intentionally left reversible

- a future Wasmtime component host for pure tooling contributions;
- a future protobuf/MessagePack driver RPC version;
- a hosted marketplace implementation;
- stronger per-platform sandbox mechanisms;
- ODBC/JDBC bridges;
- whether a proven first-party external provider later moves in process.

None of these alter the v1 identity, manifest, operation, grant, lifecycle, or
logical driver semantics defined here.
