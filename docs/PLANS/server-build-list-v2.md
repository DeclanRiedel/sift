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
- **Still dead schema:** `principal_key` and `keypair_challenge` (V001) — wire
  in Phase E or drop.

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
tenant and connection policy. Per-connection permissions, general API limits,
and tenant-resource enforcement remain absent.

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
- [ ] [Implement] Metadata migration for profile policy revisions and
      instance-admin-managed tenant limit overrides.
- [ ] [Implement] Central authorization evaluator and conservative tenant/room
      role matrix; capability discovery consumes the same evaluator.
- [ ] [Implement] Runtime provenance and connection-entry closure: managed
      connections carry principal/tenant/profile/revision; raw specs are
      personal-loopback only.
- [ ] [Implement] Connection-profile permissions: `read_only`,
      `allowed_ops`/`blocked_ops`, `allowed_schemas`; enforced in the
      dispatcher before routing to the driver.
- [ ] [Implement] Policy-revision checks and hybrid revocation of active
      connections, transactions, queries, and cursors.
- [ ] [Implement] Rate-limit middleware keyed by principal + tenant;
      configurable per route class, with bounded stream-byte pacing.
- [ ] [Implement] Tenant resource accounting for profiles, sessions,
      connections, concurrent queries, open cursors, and retained result
      bytes; admin usage API and internal metric hooks.
- [ ] [Implement] Saved-query + document namespace isolation per
      tenant/principal.
- [ ] [Implement] SDK/OpenAPI surfaces plus role, policy, SQL-classification,
      revocation, rate, quota-race, cleanup, and trusted-local integration
      matrices; graduate Phase F only with all workspace gates green.

## Phase G — Collaboration depth

Goal: graduate the room runtime from "foundation" to a real multiplayer SQL
session. CRDT only for query text; everything else server-authoritative.

- [ ] [Design] ADR-014 (candidate): lock collaboration scope — shared SQL
      editor via CRDT, ephemeral presence, shared session/connection state
      via broadcast; explicitly exclude result replication beyond
      references.
- [ ] [Design] CRDT backend choice for `sift-doc`. **Today `sift-doc` is
      not a CRDT** (`crates/doc/src/lib.rs:79-98`) — it is a UTF-8 byte
      buffer with destructive `apply()`, no op-log, no merge, no pluggable
      backend. The `CrdtKind::{Loro,Automerge}` tag is a label, never
      dispatched on. Picking + wiring a real backend (Automerge vs Loro vs
      Yjs) is the core Phase G deliverable.
- [ ] [Design] Late-join protocol: snapshot + ops-since. Today only full
      snapshots are persisted (`metadata/src/lib.rs:744-759`); there is no
      bounded op-log and no compaction.
- [ ] [Design] Presence vs durable separation: presence is ephemeral and
      fire-and-forget; document text is durable CRDT. Today presence rides
      the same `broadcast::channel(1024)` as document ops
      (`room_runtime.rs:84`).
- [ ] [Design] Shared-connection ownership: a connection opened in a room
      is server-owned; members attach and run ops through it with role
      gating from ADR-020 (editor+ can run only operations also permitted by
      tenant/profile policy; viewer observes result references).
- [ ] [Implement] Real CRDT in `sift-doc`; snapshot + op-log persistence in
      metadata; deterministic merge across peers.
- [ ] [Implement] Late-join snapshot + ops-since over the room WS; bounded
      op log with background compaction.
- [ ] [Implement] Ephemeral presence channel distinct from the durable
      doc-op channel; not persisted.
- [ ] [Implement] Shared room connection with role gating; result-reference
      broadcast (today the room emits a `RoomQueryResult` *summary*
      (`http.rs:1731-1738`), not a cursor reference peers can page from).
- [ ] [Implement] Observer lag recovery + follow mode.

## Phase H — Remote development & distribution

Goal: a sift server can run remote while a thin client renders locally.
Because sift is already server-first, this is mostly bootstrap + version
handshake.

- [ ] [Design] ADR-021 (candidate): direct SSH-tunneled remote topology (Zed
      model) using Phase E's instance-bound proxy capability. A hosted
      collaboration relay is a separate future topology, not required for
      initial remote support.
- [ ] [Design] Remote bootstrap (SSH control-master, binary fetch/upload,
      version check, daemon spawn/reconnect, capability handoff over the
      authenticated channel); reconnect + state survival on SSH drop. The
      proxy establishes an instance-bound principal context and never inherits
      personal-loopback bypass (ADR-020/030).
- [ ] [Design] Version handshake. The client-sdk never sends or inspects
      `X-Sift-Protocol-Version` today (`client-sdk/src/lib.rs` never
      imports `PROTOCOL_VERSION`); the server emits it one-way. Both sides
      need a real handshake once remote mode exists.
- [ ] [Design] Background updater (release channel + signature
      verification); single-binary distribution modes (in-process / daemon
      / container).
- [ ] [Implement] Remote bootstrap client helper; proxy-mode daemon; port-
      forward analogue; background updater; `--mode` distribution modes;
      CI release pipeline.

## Phase I — Extensibility

Goal: third-party drivers, AI/automation consumers, and connection-time
hooks without forking the server.

- [ ] [Design] ADR-022 (candidate): driver extensibility — in-tree drivers
      first-class; third-party drivers register over a local RPC protocol
      implementing the `Driver` trait shape.
- [ ] [Design] Driver RPC Protocol contract (wire encoding, capability
      advertisement, streaming `Page` frames, cancel cross-call); the RPC
      proxy must satisfy driver-isolation (ADR-013).
- [ ] [Design] MCP server surface (`sift mcp`): every `Operation` is a
      tool; results are protocol types.
- [ ] [Design] MCP governance layer (operation classification, per-
      connection policy, approval flow for write/destructive ops); it consumes
      the Phase F evaluator and must not create a second authorization model.
- [ ] [Design] Connection hooks (`PreConnect`/`PostConnect`/etc); tunneling
      for user DBs (SSH/SOCKS5/HTTP CONNECT/SSM); plugin/extension loading.
- [ ] [Implement] Driver RPC host; `sift mcp` subcommand; governance
      middleware; connection hooks; tunnel profiles; extension loader.

## Phase J — Operations polish

Goal: the last mile before a real release.

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
  `OperationKind` policy rather than defining parallel permissions.
- **Phase J's OpenAPI item can land earlier** — the hand-authored map is
  already drifting. Its metrics exporter consumes F's in-memory resource
  counters; F does not introduce a competing Prometheus surface.

## ADR candidates this list implies

| # | Candidate | Origin | Status |
| --- | --- | --- | --- |
| ADR-011 | server-side cursor registry (cap + LRA eviction + spill/resume) | Phase C | written |
| ADR-012 | schema cache with TTL + engine-specific invalidators | Phase C | written |
| ADR-013 | driver isolation | Phase B | written; both engines meet the containment boundary |
| ADR-014 | collaboration scope (CRDT text only) | Phase G | not written |
| ADR-016 | protocol versioning + semver stability | Phase B | written; pin-or-proceed negotiation, monotonic integer version |
| ADR-017 | driver trait shape | Phase A | written; Phase A trait lock |
| ADR-018 | graceful shutdown contract | Phase B | written |
| ADR-019 | audit durability | Phase B | written |
| ADR-020 | authorization model | Phase F | written |
| ADR-021 | remote topology | Phase H | not written |
| ADR-022 | driver extensibility | Phase I | not written |
| ADR-023 | inline-edit conflict & row-identity model | Phase D | drafted in `docs/PLANS/inline-edit-dml.md` |
| ADR-024 | search architecture (progressive schema index + bounded data fan-out) | Phase D | drafted in `docs/PLANS/schema-data-search.md` |
| ADR-025 | execution-plan model (typed PlanNode + XML dep + ANALYZE-rollback) | Phase D | drafted in `docs/PLANS/execution-plans.md` |
| ADR-026 | server-owned transaction panel state | Phase D | written |
| ADR-027 | bounded database process control | Phase D | written |
| ADR-028 | server-derived operation capabilities | Phase D | written |
| ADR-029 | normalized CSV import | Phase D | written |
| ADR-030 | instance-owned closed registration + hosted identity | Phase E | written |

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
