# Sift Core And Plugin Boundary

Status: **planning note for incomplete phases.** This is input to the Phase I
extension ADRs, not a locked decision and not a reason to reopen completed
phases. Companion:
`docs/PLANS/ide-parity-and-provider-extensibility.md`.

## Purpose

Sift needs a strong extension system without turning the server into an empty
shell or allowing plugins to bypass its security and collaboration model. The
boundary should be understood as three rings rather than a binary choice:

1. **Trusted mandatory core** — invariants and product state that every Sift
   instance must implement identically.
2. **First-party bundle** — components maintained and shipped by Sift but kept
   behind the same capability-oriented boundaries used by extensions.
3. **Optional plugins** — provider-, deployment-, workflow-, or vendor-specific
   contributions installed by an instance operator.

“Bundled” does not mean “architectural core.” PostgreSQL can work out of the
box while still being a driver behind a provider contract.

## Boundary test

A feature belongs in mandatory core when one or more of these are true:

- it enforces a security, isolation, audit, durability, or resource invariant;
- it owns shared state that all clients and collaborators must observe
  consistently;
- it defines the stable public API or plugin compatibility contract;
- a plugin failure must not be able to change its semantics;
- every useful installation requires it;
- it coordinates multiple contribution types or providers.

A feature belongs behind a plugin boundary when one or more are true:

- it speaks a vendor/provider protocol or depends on a vendor runtime;
- it is optional, high-churn, organization-specific, or independently
  releasable;
- it requires filesystem, network, process, cloud, or credential access that
  not every deployment should grant;
- there are multiple competing implementations or policy choices;
- omitting it should reduce capability, not corrupt core product state.

When both apply, core owns policy, orchestration, canonical data, and
validation; the plugin supplies the provider-specific mechanism.

## Trusted mandatory core

### Identity, policy, and safety

Core owns:

- authentication sessions, principals, tenants, rooms, memberships, and roles;
- authorization evaluation and capability narrowing;
- operation audit envelopes, correlation ids, redaction, and history;
- `SecretStore` handles, credential admission, and secret-delivery policy;
- rate limiting, quotas, retained-byte accounting, and resource ownership;
- timeout, cancellation, graceful shutdown, and driver/plugin isolation;
- plugin trust state, permissions, lifecycle supervision, and revocation.

These are never replaceable by plugins. A plugin may request an admitted
capability; it cannot provide a second authorization, audit, secret, or quota
system.

### Server-owned product state

Core owns:

- sessions and connection/query/transaction/cursor lifecycles;
- query result paging, spill/resume, backpressure, and result references;
- rooms, CRDT document durability, presence boundaries, late joining, and
  collaboration recovery;
- saved queries, query history, workspaces, document revisions, and run records;
- canonical connection profiles and opaque references to credentials;
- scheduler state and execution admission;
- canonical catalog snapshots, dependency identities, schema-diff plans, and
  migration approval/apply records.

Plugins may calculate or execute part of these workflows, but the authoritative
state and lifecycle stay in the server.

### Public contracts and orchestration

Core owns:

- pure-serde public protocol types and stable error semantics;
- HTTP/WebSocket routing, OpenAPI generation, SDK contract, and version
  negotiation;
- provider/plugin manifests, contribution discovery, compatibility checks, and
  the local RPC host;
- provider-neutral values, rows/pages, schema common model, typed plans, and
  capability vocabulary;
- generic execute, transaction, import/export, schema, diff, workspace, VCS,
  automation, and agent-governance orchestration;
- validation and size limits on every plugin input and output;
- a namespaced audited `Operation` representation for extension actions.

Plugins do not register raw unaudited server routes. Extension calls enter
through core-owned dispatch so authentication, policy, rate, quota, audit,
timeout, cancellation, protocol-version, and correlation middleware always
apply.

### Extensible frameworks that remain core

The following frameworks belong in core even though their implementations can
be contributed:

- database-provider registry and Driver RPC supervisor;
- SQL syntax/semantic service interfaces and dialect-pack registry;
- import/export streaming framework and recipe registry;
- tunnel, connection-hook, and credential-broker contracts;
- VCS and workspace adapter contracts;
- command/action registry and capability discovery;
- MCP/agent governance, context budgeting, approvals, and audited tool dispatch;
- declarative client-contribution schema.

## First-party bundled components

The normal Sift distribution should be useful without downloading plugins. The
first-party bundle should initially contain:

- native PostgreSQL and SQL Server drivers;
- PostgreSQL and T-SQL dialect, completion, formatter, analyzer, DDL, schema
  diff, and plan adapters as those frameworks mature;
- baseline CSV/TSV/JSON import/export formats;
- the direct SSH remote bootstrap required by Phase H;
- standard Prometheus and OpenTelemetry exposure;
- a Git workspace adapter if Phase L selects Git for v1;
- the reference client SDK and official client.

These components may receive stronger compatibility and performance guarantees
than third-party plugins, but they should consume public or clearly versioned
internal capability contracts wherever practical. Provider-specific code must
not leak back into authorization, sessions, rooms, or generic query lifecycles.

The default package can bundle these components statically or as signed sibling
executables. Packaging is separate from architectural ownership.

## Explicitly deferred to plugins

### Database connectivity

- database providers beyond the explicitly selected first-party set;
- ODBC and JDBC compatibility bridges;
- vendor-specific authentication helpers and cloud database discovery;
- provider-specific bulk, notification, replication, or administrative tools
  not promoted into a stable generic capability;
- organization-internal database proxies and custom wire protocols.

High-demand providers may later graduate into the first-party bundle without
moving into trusted core.

### Connections and infrastructure

- SOCKS5, HTTP CONNECT, SSM, cloud bastion, VPN, and organization-specific
  tunnel implementations beyond the selected first-party remote path;
- credential brokers, vault adapters, cloud identity token exchanges, and
  pre/post-connect hooks;
- certificate acquisition/rotation integrations;
- provider-specific connection diagnostics.

Core still owns connection admission, secret handles, process supervision,
timeouts, and audit.

### SQL and database tooling

- dialect packs beyond first-party PostgreSQL and T-SQL;
- vendor-specific formatter, analyzer, quick-fix, refactor, DDL, plan, schema
  diff, and catalog enrichments;
- specialized diagram projections and database-object renderers;
- custom import/export formats and transformation recipes;
- domain-specific data viewers, masking, generators, and comparison rules.

Core owns the semantic/result contracts and safety checks; plugins return typed
contributions rather than mutating core metadata directly.

### Automation, AI, and user workflow

- model-vendor clients and organization-specific AI gateways;
- agent context sources and domain tools;
- custom commands, task types, approval integrations, and notification sinks;
- CI/CD, ticketing, chat, and deployment integrations;
- optional client panels, themes, renderers, and workflow-specific UI.

MCP exposure, authorization, context limits, secret filtering, write approval,
and audit remain core. No AI plugin receives unrestricted schema, results, or
credentials merely because it is installed.

### VCS and workspaces

- VCS implementations other than the selected first-party v1 adapter;
- hosted forge integrations, code-review workflows, and issue linkage;
- repository templates and organization-specific project conventions;
- remote storage and synchronization backends.

Core owns workspace identities, revisions, conflicts, collaboration semantics,
and credential-handle policy.

## Things plugins must never do

A plugin must not:

- read or write the metadata SQLite database directly;
- enumerate or resolve arbitrary `SecretStore` entries;
- mint principals, roles, sessions, capabilities, or audit records;
- bypass the central authorization evaluator or claim trusted-local status;
- retain credentials after the admitted operation or include them in errors;
- create untracked queries, cursors, transactions, files, or child processes;
- expose unauthenticated network listeners or raw HTTP routes through Sift;
- write directly to another plugin's state or handles;
- emit unbounded frames, logs, result buffers, or retry loops;
- silently emulate an unsupported destructive capability;
- make core availability depend on a marketplace, external registry, JVM,
  ODBC manager, cloud account, or proprietary runtime.

The operator may deliberately run a trusted plugin with broad OS access, but
Sift must report that trust honestly rather than describing process isolation
as a sandbox when the platform does not enforce one.

## Plugin-facing capability APIs

Plugins receive narrow interfaces instead of references to server internals:

- lifecycle and health channel;
- admitted operation context with principal/tenant/room ids and correlation id,
  but only the policy facts required for the call;
- scoped credential delivery for one connection attempt;
- bounded request/response streams with cancellation;
- namespaced durable plugin storage with quotas and no access to core tables;
- structured logging with core redaction and rate control;
- optional network/filesystem/process grants declared in the manifest;
- core-mediated event publication to rooms, operations, or client
  contributions.

Plugin-owned durable data should be namespaced, schema-versioned, quota-bound,
exportable, and removable when the plugin is uninstalled. Core metadata stores
only the plugin identity, state version, opaque data, and lifecycle records; it
does not learn provider secrets or plugin-specific relational schemas.

## Capability ownership examples

| Capability | Core owns | Bundled/plugin implementation owns |
| --- | --- | --- |
| Execute SQL | admission, lifecycle, paging, audit, quota | wire protocol and value decoding |
| Cancel query | deadline and cancellation orchestration | provider-native cancel mechanism |
| Introspect schema | canonical model, cache, visibility policy | provider catalog queries and enrichments |
| Format/analyze SQL | request contract, limits, shared document operation | dialect grammar and rules |
| Schema migration | normalized plan, approval, audit, apply lifecycle | dialect rendering and provider constraints |
| Import/export | bounded streaming and policy | file/record encoding or provider fast path |
| Tunnel connection | profile, secret handles, admission, supervision | transport mechanism |
| AI tool call | context policy, approval, audit, result limits | model/provider request and optional tool logic |
| VCS action | workspace identity, permissions, operation record | Git or other VCS mechanism |
| Scheduled run | durable schedule and admitted execution | optional trigger/task contribution |

## Packaging and availability

The project should publish a capability matrix rather than imply that every
plugin has first-party fidelity:

- **Core compatible** — handshake and safety conformance only.
- **Query capable** — connect, execute, page, cancel, transactions, and types.
- **IDE capable** — deep schema, completion/dialect metadata, DDL, plans, edits,
  import/export, and invalidation.
- **Sift certified** — maintained test corpus, fault isolation, performance,
  security review, and supported release matrix.

A minimal Sift installation remains operable and administrable with no optional
plugin installed. The standard distribution bundles the selected first-party
providers and formats. Hosted operators decide which additional plugins are
available to their tenants; ordinary users cannot install server code unless an
explicit future policy grants that ability.

## Phase I decisions to lock

1. Exact mandatory core crate/process boundary.
2. Which first-party components are static crates versus supervised sibling
   processes.
3. Plugin manifest, contribution ids, permissions, and trust states.
4. Namespaced extension `Operation` and policy model.
5. Plugin storage, migration, backup, and uninstall semantics.
6. Capability and certification levels.
7. Whether any plugin class may run in-process; default answer is no for
   third-party server plugins.
8. Which components ship in the standard distribution without becoming core.

