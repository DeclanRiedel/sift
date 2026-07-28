# Server-Side Build List — Everything Before The GUI

> Status: **code-grounded work-management checklist.** Every open item below
> reflects a real gap verified against the code. This is the single ordered
> backlog for all server-side work that must land before the product GUI.
>
> Companion to `docs/DECISIONS.md` (ADRs) and `docs/legacy/ZED_LESSONS.md`
> (rationale for stolen ideas). Items marked `[x]` are verified-present in
> code; `[ ]` are verified-absent or stubbed.
>
> Format: `- [status] [Design|Implement] <area>: <goal>`. **Design** = lock a
> decision (ADR/crate/contract); **Implement** = build against a locked design.

## Current state

- **Phases A, B, C are complete** — driver & type completeness (trait locked by
  ADR-017), the server reliability layer (timeouts, graceful shutdown, audit,
  correlation ids, secret backends, result caps), and the performance layer
  (cursor registry + spill/resume, schema cache, pool pre-warm, compression).
  Their per-item detail lived here previously; it is now recorded in the git
  history and the ADRs, not re-listed.
- **Phase D is complete.** Landed: autocomplete endpoint (`sift-completion`
  crate + `POST .../complete`), DDL generation (`server/src/ddl.rs`; remaining
  gaps tracked in `docs/PLANS/ddl-gaps.md`), the export pipeline
  (`server/src/export.rs`, CSV/TSV/JSONL/JSON-array, streamed and routed through
  the cursor registry), saved-query library (full CRUD + FTS + RBAC), inline
  edits, schema/data search, execution plans, transaction state, process
  control, contextual capabilities, and CSV import.
- **Phase D readiness was re-audited on 2026-07-20.** Runtime correctness,
  public operation coverage, OpenAPI/SDK reachability, failure auditing, and
  priority-one DDL fidelity were polished. The explicitly listed v1 and DDL
  gaps are accepted follow-ups, not Phase E prerequisites. See
  `docs/PLANS/phase-d-readiness.md`.
- **Phase G is complete.** Room execution consumes the streaming cursor path
  and publishes opaque, transient result references; current members
  independently page immutable result pages. Presence is leased and carries
  stable Loro selection anchors, lag recovery is explicit, and the reference
  SDK includes client-side follow-mode projection.
- **Phase H is complete.** Direct SSH bootstrap, the persistent authenticated
  proxy daemon, pre-release protocol range negotiation, lifecycle modes, signed
  periodic update staging, readiness-gated activation/rollback, and release
  CI are implemented. See `docs/PLANS/phase-h-remote-development.md`.
- **Phase I is complete.** Provider-neutral protocol v1, strict signed
  packages, supervised tenant-scoped Driver RPC, governed automation/MCP,
  connection-pipeline contracts, lifecycle management, hostile conformance,
  and public operational artifacts are implemented. ODBC/JDBC bridges remain
  explicitly deferred. See `docs/PLANS/phase-i-extensibility.md` and
  `docs/EXTENSIONS.md`.

---

## Phase D — Headless product features

Goal: the server side of every daily-driver and power-user IDE feature, so a
GUI later is just rendering.

- [x] [Design] Inline-edit → DML generation (ADR-023). `docs/PLANS/inline-edit-dml.md`.
- [x] [Implement] Inline-edit → DML. `protocol/src/edit.rs`,
      `server/src/edit.rs` (PK/unique-index identity, parameterized
      INSERT/UPDATE/DELETE, engine-quoted, RETURNING/OUTPUT keys),
      `SessionStore::{preview_edits,apply_edits}` (transactional apply,
      optimistic `affected_rows==1` conflict → `Code::EditConflict`), routes
      `POST .../edits/{preview,apply}`. Tests: `edit::tests` (9),
      `tests/edits.rs` (4). **v1 gaps:** generated/computed columns not yet
      excluded from INSERT (blocked on `ddl-gaps.md` default_expr work);
      optional dry-run conflict count not implemented.
- [x] [Design] Transactions panel contract (ADR-026): server exposes open-tx state
      per connection, savepoint lifecycle (Phase A savepoint Operation variants
      exist), commit/rollback preview. `docs/PLANS/transactions-panel.md`.
- [x] [Implement] Transactions panel server state. Session-scoped list and
      commit/rollback preview routes, tracked savepoint lifecycle, audited
      `Operation` variants, OpenAPI schemas, and client SDK methods.
- [x] [Design] Schema search + data search (ADR-024). `docs/PLANS/schema-data-search.md`.
- [x] [Implement] Schema + data search. `completion/src/fuzzy.rs`
      (subsequence matcher + scoring), `protocol/src/search.rs`,
      `server/src/search.rs` (per-connection `SearchIndex` from shallow schema +
      one bulk column catalog query, fuzzy `rank`, bounded data-search SQL),
      `SessionStore::{search_schema,search_data}` (TTL-cached index; data
      fan-out through `execute_http` with per-table/table-count caps + `LIKE`
      escaping), routes `POST .../search/{schema,data}`. Tests: `fuzzy::tests`
      (6), `search::tests` (6), `tests/search.rs` (3). **v1 gaps:** index built
      lazily+cached (background post-connect pre-warm and DDL invalidation
      deferred — always reports `Ready`); data fan-out is sequential (bounded
      concurrency deferred); engine-native FTS not wired (LIKE only);
      numeric/date columns not searched.
- [x] [Design] Execution plans (typed `PlanNode` tree, ADR-025). `docs/PLANS/execution-plans.md`.
- [x] [Implement] Execution plans. `protocol/src/plan.rs` (`PlanNode`,
      `ExplainRequest/Response`) + `Operation::Explain`; `server/src/plan.rs`
      parses PG `EXPLAIN (FORMAT JSON)` (serde_json) and MSSQL showplan XML
      (`roxmltree`) into a common-core node + `extra` map + raw blob; ANALYZE of
      a non-read statement runs in a rolled-back tx. Route `POST .../explain`.
      Tests: `plan::tests` (3), `tests/explain.rs` (4). **v1 gap:** MSSQL
      `analyze=true` returns `UnsupportedForEngine` (STATISTICS XML multi-result
      capture not wired); PG analyze is full.
- [x] [Design] Process list + kill (ADR-027): PG `pg_stat_activity` +
      `pg_terminate_backend`, MSSQL `sys.dm_exec_requests` + `KILL`.
      `docs/PLANS/process-control.md`.
- [x] [Implement] Process list + kill. Normalized cross-engine process model,
      bounded catalog queries, guarded termination route, audit variants,
      OpenAPI schemas, and client SDK methods.
- [x] [Design] Command-palette server surface (ADR-028): enumerate available
      `OperationKind`s for a server-derived capability context at
      `GET /v1/operations/available`; preserve `/v1/operations` as the replay
      log. `docs/PLANS/operation-capabilities.md`.
- [x] [Implement] Contextual capability query. Exhaustive `OperationKind`,
      server-derived session/connection/transaction evaluation with disabled
      reasons and destructive flags, OpenAPI schema, audit entry, and SDK.
- [x] [Design] CSV import → table (ADR-029): server-side validation and type
      inference, optional create, atomic abort or duplicate-skip policy; PG
      `COPY FROM STDIN` and SQL Server bulk fast paths.
      `docs/PLANS/csv-import.md`.
- [x] [Implement] CSV import. Validated 64 MiB-bounded parser, deterministic
      type inference, optional engine-quoted table creation, atomic abort and
      duplicate-skip modes, both engine ingest extensions, audited route,
      OpenAPI schemas, and client SDK method.

## Phase E — Hosted auth & identity

Goal: take auth from "bearer token + loopback bypass" to "hosted mode with
real identity," without breaking local-first (ADR-006, ADR-010).

- [x] [Design] ADR-030: instance-owned closed registration with provider-neutral
      principals. Deployment policy (`personal | team`) is separate from
      transport (`loopback | network | ssh-proxy`). Password and per-instance
      GitHub OAuth credentials are equivalent auth methods; OIDC is deferred.
      `docs/PLANS/hosted-identity.md`.
- [x] [Design] Auth-code + state + S256 PKCE; 15-minute opaque access tokens +
      30-day rotating refresh families with replay revocation; native bearer +
      secure web cookie; WebSocket auth leases; personal tenant on first
      principal creation and explicit invite/accept for teams.
- [x] [Implement] Authentication floor: central fail-closed middleware and
      principal/room ownership across sessions, connections, transactions,
      cursors, queries, and WebSockets. This minimum authorization moves from
      Phase F because hosted identity is unsafe without it.
- [x] [Implement] Instance admin bootstrap and closed registration: create,
      disable, link, and revoke password/GitHub identities; GitHub allowlist;
      personal-tenant creation and team invitation lifecycle.
- [x] [Implement] Username/password login using Argon2id verifiers behind
      `SecretStore`; auth-specific throttling; session-token
      issue/refresh/revoke with rotating refresh tokens and replay detection.
- [x] [Implement] Per-instance GitHub OAuth login route pair, allowlist
      enforcement, immutable GitHub-id binding, and profile synchronization.
- [x] [Implement] Keypair auth. Ed25519 registration/revocation and bounded,
      one-use challenges now issue the standard opaque Sift session shape.
- [x] [Implement] Policy/transport guarantee: loopback bypass exists only for
      `personal + loopback`; every network transport requires explicit auth;
      team mode fails closed on unsafe configuration. Future SSH proxy auth
      uses an instance-bound capability rather than broadening loopback trust.
- [x] [Implement] Principal profile sync (display name, optional email, avatar
      from GitHub on login); expose via `/v1/auth/whoami`; native SDK token
      rotation and cookie/CSRF + WebSocket reauthentication surfaces.

## Phase F — Authorization, tenancy & limits

Goal: deepen Phase E's principal/room ownership floor into configurable
tenant and connection policy, general API limits, and tenant-resource
enforcement.

- [x] [Design] ADR-020: deny-wins intersection of tenant role, room role, and
      connection-profile policy through one server evaluator. Personal +
      loopback stays login-optional and permits raw connection specs; every
      shared/network path requires authentication and managed profiles.
- [x] [Design] Hierarchical token-bucket rate limiting: principal + tenant,
      classified as control, interactive, query, heavy transfer, and streamed
      bytes. HTTP denial is 429 + `Retry-After` + `Code::RateLimited`; WebSocket
      operations use the same code. Trusted personal-loopback is exempt by
      default. Phase E retains separate login/refresh abuse throttling.
- [x] [Design] Tenant isolation: configuration defaults + operator-bounded
      per-tenant overrides for profiles, sessions, connections, concurrent
      queries, cursors, and retained result bytes. RAII admission guards;
      `Code::TenantResourceExhausted`; trusted personal-loopback unlimited by
      default. Detailed build contract: `docs/PLANS/phase-f-authorization.md`.
- [x] [Implement] Protocol policy/usage contracts and stable
      `RateLimited`/`TenantResourceExhausted` errors.
- [x] [Implement] Metadata migration for profile policy revisions and
      instance-admin-managed tenant limit overrides.
- [x] [Implement] Central authorization evaluator and conservative tenant/room
      role matrix; capability discovery consumes the same evaluator.
- [x] [Implement] Runtime provenance and connection-entry closure: managed
      connections carry principal/tenant/profile/revision; raw specs are
      personal-loopback only.
- [x] [Implement] Connection-profile permissions: `read_only`,
      `allowed_ops`/`blocked_ops`, `allowed_schemas`; enforced in the
      dispatcher before routing to the driver.
- [x] [Implement] Policy-revision checks and hybrid revocation of active
      connections, transactions, queries, and cursors.
- [x] [Implement] Rate-limit middleware keyed by principal + tenant;
      configurable per route class, with bounded stream-byte pacing.
- [x] [Implement] Tenant resource accounting for profiles, sessions,
      connections, concurrent queries, open cursors, and retained result
      bytes; admin usage API and internal metric hooks.
- [x] [Implement] Saved-query + document namespace isolation per
      tenant/principal.
- [x] [Implement] SDK/OpenAPI surfaces for policy, effective capabilities,
      limits, usage, structured errors, and retry metadata.
- [x] [Graduate] Role, policy, SQL-classification, revocation, rate,
      quota-race, cleanup, and trusted-local integration matrices; graduate
      Phase F only with all workspace gates green.

## Phase G — Collaboration depth

Goal: graduate the room runtime from "foundation" to a real multiplayer SQL
session. CRDT only for query text; everything else server-authoritative.

- [x] [Design] ADR-014 (candidate): lock collaboration scope — shared SQL
      editor via CRDT, ephemeral presence, shared session/connection state
      via broadcast; explicitly exclude result replication beyond
      references. **Written** (`docs/DECISIONS.md` ADR-014); CRDT state
      outside the SQL text is explicitly out of scope.
- [x] [Design] CRDT backend choice for `sift-doc`. **Resolved: Loro**
      (ADR-014). `sift-doc` is now a real Loro CRDT — the `CrdtKind`/
      `Automerge` selector and positional-op contract were removed. Server
      validates, durably sequences, and rebroadcasts native Loro updates;
      all Loro CPU runs in a per-document blocking actor.
- [x] [Design] Late-join protocol: snapshot + ops-since. **Implemented** —
      version-vector `DocumentSync`, chunked snapshot/update transfer, a
      bounded per-document op-log (`list_document_updates_since`,
      `next_update_seq`), and `compact()` with covered-row truncation
      (`crates/server/src/document_actor.rs`, `crates/metadata/src/lib.rs`).
- [x] [Design] Presence vs durable separation: presence is ephemeral and
      fire-and-forget; document text is durable CRDT. Today presence rides
      the same `broadcast::channel(1024)` as document ops
      (`room_runtime.rs:84`). Drafted in
      `docs/PLANS/presence-durable-separation.md` (ADR-035): two per-room
      broadcast lanes; durable-lane lag emits the (previously dead)
      `ResyncRequired` and the client re-runs `DocumentSync`.
- [x] [Design] Shared-connection ownership: a connection opened in a room
      is server-owned; members attach and run ops through it with role
      gating from ADR-020 (editor+ can run only operations also permitted by
      tenant/profile policy; viewer observes result references). Drafted in
      `docs/PLANS/shared-connection-ownership.md` (ADR-036): room binds one
      connection profile (binder's credentials, revocable); execute resolves
      the bound profile and runs `authorize()` with the submitter's scope
      (intersection gating — already implemented in `authorization.rs`).
- [x] [Implement] Real CRDT in `sift-doc`; snapshot + op-log persistence in
      metadata; deterministic merge across peers. Done: Loro replica
      (`crates/doc`), per-document blocking actor + `DocumentRegistry`, durable
      `crdt_state` snapshot + `document_update` op-log in metadata; Loro
      provides deterministic cross-peer merge (ADR-014).
- [x] [Implement] Late-join snapshot + ops-since over the room WS; bounded
      op log with background compaction. Done: version-vector `DocumentSync`,
      chunked snapshot/update transfer, `list_document_updates_since`,
      `next_update_seq`, `should_compact`/`compact` with covered-row
      truncation. (Compaction runs inline on write via `should_compact`, not a
      separate background task.)
- [x] [Implement] Ephemeral presence channel distinct from the durable
      doc-op channel; not persisted. Done (ADR-035): per-room
      `presence_events` (cap 256) + `doc_events` (cap 1024) in
      `room_runtime.rs`; `handle_room_ws` selects both lanes — presence lag
      heals with a snapshot, doc lag emits `ResyncRequired`
      (`runtime_epoch` + `event_seq` now wired). Fixes silent loss of
      committed CRDT ops on a lagging peer.
- [x] [Implement] Shared room connection with role gating; result-reference
      broadcast. Room→connection **binding** (nullable
      `room.bound_connection_profile_id` + `bound_connection_by`, migrations
      V020/V021, owner-gated bind/unbind + `PUT/DELETE
      /v1/metadata/rooms/:id/connection`) **and routing** — a bound room opens
      one server-owned connection under the binder's provenance
      (`SessionStore::execute_room_query`, a hidden binder-owned session +
      managed connection, serialized by a per-room async mutex), room execute
      is authorized by the submitter's room-role × the bound profile policy
      before routing, and an unbound room is hard-rejected. The connection is
      torn down on unbind/rebind and when the room empties (self-healing via
      lazy reopen). Room execution consumes the streaming cursor path, retains
      immutable pages behind an opaque `RoomResultId`, spills overflow with a
      process-random ChaCha20-Poly1305 key, and broadcasts only the sanitized
      result reference (never SQL or rows). Current viewers can list/get/page
      results independently. The E2E matrix covers viewer paging and
      profile-policy denial of an editor.
- [x] [Implement] Observer lag recovery + follow mode. Presence heartbeats use
      a 30-second lease; presence updates carry active-document and Loro-stable
      selection anchors. Durable-lane lag emits `ResyncRequired`, ephemeral
      lag refreshes presence and shared results are rediscovered over HTTP.
      `sift-client-sdk::FollowMode` projects presence/result events and exposes
      `NeedsRecovery` without putting follow state in the CRDT.

## Phase H — Remote development & distribution

Goal: a sift server can run remote while a thin client renders locally.
Because sift is already server-first, this is mostly bootstrap + version
handshake.

- [x] [Design] ADR-021: direct SSH-tunneled remote topology (Zed
      model) using Phase E's instance-bound proxy capability. A hosted
      collaboration relay is a separate future topology, not required for
      initial remote support. See
      `docs/PLANS/phase-h-remote-development.md`.
- [x] [Design] Remote bootstrap (SSH control-master, binary fetch/upload,
      version check, daemon spawn/reconnect, capability handoff over the
      authenticated channel); reconnect + state survival on SSH drop. The
      proxy establishes an instance-bound principal context and never inherits
      personal-loopback bypass (ADR-020/030).
- [x] [Design] Version handshake. ADR-016 locks the range negotiation and
      response-validation contract.
- [x] [Design] Background updater (release channel + signature
      verification); single-binary distribution modes (in-process / daemon
      / container). ADR-015 locks manifest trust, staging, activation, and
      rollback.
- [x] [Implement] Remote bootstrap client helper; proxy-mode daemon; port-
      forward analogue; periodic background updater; launcher-owned verified
      activation/rollback; `--mode` distribution modes; CI release pipeline.

## Phase I — Extensibility

Goal: a strong, versioned plugin system for database providers, SQL tooling,
automation, and connection-time hooks without forking or destabilizing the
server. The decision-complete contract is
`docs/PLANS/phase-i-extensibility.md`; earlier inputs remain in
`docs/PLANS/ide-parity-and-provider-extensibility.md` and
`docs/PLANS/core-plugin-boundary.md`.

- [x] [Design] ADR-022: built-ins remain native behind a provider-neutral
      registry; third-party providers use supervised Driver RPC v1 over
      length-prefixed JSON stdio. ODBC/JDBC and automatic bridge discovery are
      deferred.
- [x] [Design] Provider identity and discovery: immutable namespaced provider,
      dialect, extension, and contribution ids; protocol-v1 descriptors;
      JSON-schema configuration; explicit versioned capability families.
- [x] [Design] Driver RPC v1: identity/version handshake, generation-scoped
      handles, 16 MiB hard frame ceiling, host byte-credit backpressure,
      structured errors, deadlines, cancel/kill, restart/quarantine, and a
      hostile conformance corpus.
- [x] [Design] ADR-031: strict manifest v1, content-addressed packages,
      provenance/signatures, operator grants, honest isolation labels,
      declarative-first contributions, lifecycle/update/rollback, and
      forbidden plugin access.
- [x] [Design] Core/bundle/plugin boundary and contribution points. Phase I
      activates providers, connection pipeline contributions, commands/tools,
      MCP, and discovery; Phase K/L own semantic/workspace contracts.
- [x] [Design] Namespaced extension operations and storage. Manifest-locked
      classifications consume Phase F policy, audit, rate, quota, timeout,
      cancellation, and approval; uninstall retains data until explicit purge.
- [x] [Design] MCP governance. `sift mcp` uses a normal authenticated session
      and explicit tool descriptors; writes/destructive/admin actions require
      narrowly bound approval by default.
- [x] [Design] Deterministic connection pipeline for hooks, credential brokers,
      and tunnel leases with reverse cleanup and no secret arguments/env/logs.
- [x] [Design] Declarative client contribution descriptors only; no arbitrary
      extension JavaScript or raw routes.
- [x] [Implement I0–I3] Contract crates, protocol v1, package registry,
      supervisor, provider-neutral registry, and built-in adapters.
- [x] [Implement I4–I6] Driver RPC host/SDK + conformance provider,
      namespaced operations/storage, and connection-pipeline fixtures.
- [x] [Implement I7–I8] Governed command/tool registry, approval records,
      `sift mcp`, management APIs, and declarative client discovery.
- [x] [Implement I9] Fault/security matrices, compatibility/certification
      artifacts, and operational documentation.
- [x] [Graduate] A plugin crash, timeout, protocol violation, secret-handling
      failure, or incompatible version cannot freeze or compromise the server;
      provider capability and compatibility matrices are public API artifacts.

## Phase J — Operations polish

Goal: establish the operational and public-contract foundation for a real
release. Packaging is finalized after the selected Phase K/L v1 scope lands.

- [ ] [Design] Metrics surface (`/v1/metrics` Prometheus); OpenTelemetry
      export; server-side migrations policy (`sift migrate` subcommand vs
      startup gate — today refinery runs eagerly on startup,
      `metadata/src/lib.rs:80`); backup/restore ops; query plan capture +
      retrieval; scheduler. Prometheus/OTLP export consumes Phase F's
      resource counters and rate-limit events.
- [ ] [Design] Release + packaging (musl/static Linux, macOS, Windows;
      per-channel artifacts; signature material for the Phase H updater).
- [ ] [Implement] Prometheus metrics endpoint; OTLP trace export; `sift
    migrate` subcommand + startup gate with pre-release CI matrix;
      backup/restore driver methods + Operations; plan capture wired into
      `execute`; scheduler runtime.
- [ ] [Implement] **OpenAPI generation from typed schemas** to replace the
      hand-authored JSON at `http.rs:655-978`. The hand-authored map already
      drifts from routes. Single source of truth = `utoipa` annotations or
      route-level schema extraction; add a drift test. (Can land earlier —
      the drifting hand-authored map is a documentation-contract hazard.)
- [ ] [Implement] Public contract closure: SDK methods for every supported
      route, streaming export consumption, cursor-based pagination for large
      collections, persistent room clients, reconnect discovery, mutation
      revisions/preconditions, and automated router/OpenAPI/SDK parity checks.

## Phase K — SQL intelligence & database modeling

Goal: add the semantic database-IDE layer that is absent from the runtime API,
without moving product behavior into one privileged client. Detailed gap
inventory: `docs/PLANS/ide-parity-and-provider-extensibility.md`.

- [ ] [Design] ADR candidate: shared dialect-aware SQL syntax and semantic
      service powering formatting, diagnostics, completion, statement
      selection, usages, refactoring, quick fixes, and governed AI context.
- [ ] [Design] Catalog identity and dependency graph across tables, views,
      routines, triggers, types, constraints, and referenced columns; define
      invalidation and partial-introspection behavior.
- [ ] [Design] Schema diff/migration contract: durable snapshots, normalized
      changes, dependency ordering, engine-aware generated SQL, preview,
      destructive warnings, transactional limits, and audited apply.
- [ ] [Design] Table/result comparison: key selection, duplicate handling,
      type-aware tolerances, bounded paging, cancellation, and optional patch
      generation.
- [ ] [Design] Diagram projection from the catalog graph. Layout and visual
      editing remain client concerns; graph truth and mutations remain server
      operations.
- [ ] [Implement] SQL parse/semantic services; formatter; diagnostics and
      quick fixes; richer completion; find usages/refactoring; catalog graph;
      diagrams API; schema diff/migration preview+apply; data/result compare.
- [ ] [Graduate] PostgreSQL and SQL Server semantic/diff corpora, destructive
      migration safety matrix, large-schema latency budgets, and public
      Operation/OpenAPI/SDK coverage.

## Phase L — Workspaces, VCS & execution automation

Goal: support DataGrip-class files, offline DDL sources, history, VCS, and run
configurations without abandoning thin clients or breaking remote topology.

- [ ] [Design] ADR candidate: server-owned versus hybrid workspace topology.
      Local conveniences may use client files, but hosted/remote product state
      must have a server-authoritative representation.
- [ ] [Design] Durable SQL files/documents, folders, revisions, local-history
      semantics, offline DDL sources, and mapping between DDL models and live
      connections.
- [ ] [Design] VCS adapter boundary: repository binding, status/diff/commit
      operations, remote-server filesystem rules, scoped credentials through
      `SecretStore`, and collaboration conflict behavior.
- [ ] [Design] Run configurations: ordered scripts, target connections/schemas,
      variables and secret references, transaction/error policies, pre-tasks,
      scheduling handoff, logs, cancellation, and audited reruns.
- [ ] [Design] Extensible import/export recipes including HTML, Markdown,
      spreadsheet, and operator-installed formatter plugins. Untrusted
      formatters use the Phase I extension boundary rather than in-process
      execution.
- [ ] [Implement] Workspace/history API; DDL-source model; VCS adapter and Git
      implementation; run-configuration executor; recipe-based import/export;
      SDK/OpenAPI and remote integration.
- [ ] [Graduate] Local, SSH-remote, and network-hosted workspace matrices;
      repository credential redaction; concurrent edit/VCS conflict tests;
      deterministic multi-script execution and recovery tests.

---

## Sequencing & dependency notes

- **Phases D and E are complete.** Hosted password/GitHub identity, closed
  registration, keypair sessions, invitations, ownership enforcement, and
  renewable WebSocket leases are implemented and release-gated.
- **Phase G's first deliverable is replacing `sift-doc` with a real CRDT.**
  Everything else in G (late-join, presence split, follow mode) depends on it.
- **Phase G shared execution depends on F's managed-connection provenance and
  central evaluator.** Room roles narrow tenant/profile permission; they never
  grant around it.
- **Phase H depends on E's instance-bound proxy capability + a real version
  handshake.** The one-way header today is not a handshake. It does not
  require a central identity or collaboration relay, and it cannot reuse the
  personal-loopback bypass.
- **Phase I is mostly orthogonal** but governance consumes F's evaluator and
  `OperationKind` policy rather than defining parallel permissions. Its driver
  protocol and plugin manifest are prerequisites for third-party providers,
  dialect packs, exporters, and agent context extensions in K/L.
- **Phase J's OpenAPI item can land earlier** — the hand-authored map is
  already drifting. Its metrics exporter consumes F's in-memory resource
  counters; F does not introduce a competing Prometheus surface.
- **Phase K consumes C's schema cache, D's editing/search/DDL surfaces, and I's
  dialect-pack boundary.** It must expose semantic operations through the
  server rather than creating desktop-only product behavior.
- **Phase L depends on H's remote topology and G's durable document model.** A
  hosted workspace cannot assume the client's filesystem is locally mounted;
  VCS and automation also consume I's plugin/permission model.
- **The release-packaging portion of Phase J is finalized after K/L.** Contract
  generation and observability may land earlier, but a feature-complete release
  artifact includes the later IDE/workspace surfaces selected for v1.

## ADR candidates this list implies

| #       | Candidate                                                             | Origin  | Status                                                         |
| ------- | --------------------------------------------------------------------- | ------- | -------------------------------------------------------------- |
| ADR-011 | server-side cursor registry (cap + LRA eviction + spill/resume)       | Phase C | written                                                        |
| ADR-012 | schema cache with TTL + engine-specific invalidators                  | Phase C | written                                                        |
| ADR-013 | driver isolation                                                      | Phase B | written; both engines meet the containment boundary            |
| ADR-014 | collaboration scope (CRDT text only)                                  | Phase G | written; Loro single backend, CRDT limited to the SQL text     |
| ADR-015 | signed background updater                                             | Phase H | written; verified staging and restart activation by mode       |
| ADR-016 | protocol versioning + semver stability                                | Phase B | written; two-sided range handshake, monotonic integer version  |
| ADR-017 | driver trait shape                                                    | Phase A | written; Phase A trait lock                                    |
| ADR-018 | graceful shutdown contract                                            | Phase B | written                                                        |
| ADR-019 | audit durability                                                      | Phase B | written                                                        |
| ADR-020 | authorization model                                                   | Phase F | written                                                        |
| ADR-021 | remote topology                                                       | Phase H | written; direct SSH bootstrap + persistent proxy daemon        |
| ADR-022 | driver extensibility                                                  | Phase I | written; provider ids + supervised JSON/stdio Driver RPC v1    |
| ADR-023 | inline-edit conflict & row-identity model                             | Phase D | drafted in `docs/PLANS/inline-edit-dml.md`                     |
| ADR-024 | search architecture (progressive schema index + bounded data fan-out) | Phase D | drafted in `docs/PLANS/schema-data-search.md`                  |
| ADR-025 | execution-plan model (typed PlanNode + XML dep + ANALYZE-rollback)    | Phase D | drafted in `docs/PLANS/execution-plans.md`                     |
| ADR-026 | server-owned transaction panel state                                  | Phase D | written                                                        |
| ADR-027 | bounded database process control                                      | Phase D | written                                                        |
| ADR-028 | server-derived operation capabilities                                 | Phase D | written                                                        |
| ADR-029 | normalized CSV import                                                 | Phase D | written                                                        |
| ADR-030 | instance-owned closed registration + hosted identity                  | Phase E | written                                                        |
| ADR-031 | plugin manifest, isolation, permissions, and lifecycle                | Phase I | written; declarative-first packages + core-governed operations |
| ADR-032 | SQL semantic service and dialect-pack boundary                        | Phase K | not written                                                    |
| ADR-033 | catalog graph, schema diff, and migration safety                      | Phase K | not written                                                    |
| ADR-034 | server-owned or hybrid workspace and VCS topology                     | Phase L | not written                                                    |
| ADR-035 | room lane separation + CRDT-safe lag recovery                         | Phase G | implemented; `docs/PLANS/presence-durable-separation.md`       |
| ADR-036 | room-owned connection binding + submitter-scoped authorization        | Phase G | implemented; `docs/PLANS/shared-connection-ownership.md`       |
| ADR-037 | room-owned system session + submitter-scoped pre-authorization        | Phase G | implemented; `docs/PLANS/shared-room-connection-routing.md` |

## Reference: what is being stolen, and what is not

Stealing (with attribution):

- **Zed** — process discipline (→ driver isolation ADR-013), restart model
  (→ metadata + room snapshots), action system with capability checks
  (→ Phase D capability query), background updater (Phase H), CRDT-only-
  for-text (Phase G), progressive post-paint indexing (Phase C schema
  cache), late-join = snapshot + ops-since (Phase G), GitHub OAuth
  `read:user` flow (Phase E), remote SSH bootstrap + proxy-mode daemon
  reconnect (Phase H).
- **dbflux** — Driver RPC Protocol for out-of-process drivers (Phase I),
  MCP server + governance/approval layer (Phase I), SSH/SOCKS5/HTTP/SSM
  tunnel profiles (Phase I), connection hooks (Phase I), audit redaction +
  query fingerprinting + centralized error correlation id (Phase B).

Not copying (per ZED_LESSONS §5):

- CRDTs for results/schema/sessions — those stay server-authoritative.
- Local-first file ownership — sift's source of truth is the user DB, not
  a client-owned file (ADR-002).
- Treating result grids as editable buffers — they need server-side
  cursors, virtualization hints, and backpressure.
- Replicating result data to peers — share a reference, not the rows.
