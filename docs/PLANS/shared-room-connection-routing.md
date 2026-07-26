# Design — Shared room connection routing (Phase G, G9)

> Status: **drafted.** Routes room-scoped query execution through a single
> server-owned connection the room opens from its bound profile (ADR-036),
> instead of each member's own connection. Completes the G9 "shared room
> connection with role gating" implement item. Composes over the existing
> session/connection/cursor/transaction/execute machinery — **no new `Driver`
> method** (ADR-017 lock preserved).
>
> Implies **ADR-037 (candidate): room-owned system session + submitter-scoped
> pre-authorization.** Weight on README **Goal #4**: members join a live shared
> SQL session and run against it immediately, with the server — not each
> client — owning the database connection.

## Decisions locked (this session)

1. **Topology = room-owned system session.** The room lazily owns a hidden
   `SessionId` holding one connection opened from the bound profile.
   Room-scoped execute resolves to `(room_session, room_conn)` and reuses
   `execute_http`, the cursor registry, transactions, resource accounting, and
   the schema cache unchanged. No parallel connection registry.
2. **Concurrency = serialize on a single connection.** One DB connection per
   room; concurrent member queries queue behind a per-room async lock (a DB
   connection runs one statement at a time regardless). A per-room pool is a
   later optimization.

## The authorization problem (and resolution)

Routing naively would authorize the wrong principal. `execute_http` →
`authorize_connection_operation` builds the `AuthorizationScope` from the
connection's **managed provenance** — i.e. the **binder** — with
`room_role: None` (`session.rs:417-423`). So a room execute run on the
room-owned connection would be authorized as the binder, **ignoring the
submitting member's viewer/editor role**. That defeats ADR-036's whole point.

**Resolution — two authorization layers:**

- **Submitter layer (new), before routing.** In the room-execute pre-check,
  build the *submitter's* scope
  `{ tenant_role: submitter, room_role: submitter's room role,
  connection_policy: bound profile's policy }` and run
  `authorize(scope, ExecuteQuery)` + `sql_policy::enforce(...)`. This is the
  ADR-036 intersection: viewer → `RoomEditorRequired`; an editor whose SQL/op
  is blocked by the bound profile's policy → denied. Reuses the existing
  evaluator; no second authorization model.
- **Connection-owner layer (existing), during routing.** `execute_http` on the
  room session still runs `authorize_connection_operation`, which authorizes as
  the binder — always passes for a validly bound connection, and keeps the
  policy-revision refresh + `sql_policy` enforcement the managed path already
  provides.

Audit/history attribution stays the **submitter** (already the case:
`RoomQueryResult.actor_principal_id`, `NewQueryHistory.principal_id`).

## Lifecycle

- **Binder identity.** Opening a managed connection needs the binder principal
  (provenance = `{principal_id, tenant_id, profile_id, policy_revision}`). The
  room now records `bound_connection_by` alongside `bound_connection_profile_id`
  (migration V021; set in `bind_room_connection`). *(Landed as the foundational
  slice of this design.)*
- **Lazy open.** On the first room-scoped execute after a bind, the room opens
  its system session (owned by the binder principal) and a managed connection
  from the bound profile via `resolve_connection_spec` +
  `open_managed_connection`. Cached as `room_id → RoomConnection { session,
  conn, engine, lock }`.
- **Routing.** `execute_query` detects a bound room, acquires the per-room
  lock, rewrites the target to `(room_session, room_conn)`, and calls
  `execute_http`. The client's `session`/`req.connection` are ignored for
  room-scoped execute.
- **`RoomConnectionUnbound`.** With routing in place, a room-scoped execute on
  an unbound room is rejected (the soft attribution-only behavior from the
  ADR-036 binding increment becomes a hard reject).
- **Teardown.** Close the room connection + system session on unbind, on room
  emptiness (last subscriber leaves), and on credential/membership revocation
  (the existing `managed_connections` reverse index already drives hard
  revocation cleanup).

## Non-goals

- **No per-room pool.** Single serialized connection (decision 2).
- **No result replication.** The pageable result-reference broadcast (viewer
  observes results) is the remaining G9 slice, designed in
  `shared-connection-ownership.md`; it is independent of routing.
- **No cross-room connection sharing.** One connection per room.

## Build slices

1. **Binder recorded** — landed. `bound_connection_by` column (V021) + bind
   wiring + `Room` field.
2. **Room connection manager** — landed. `room_connections: DashMap<i64,
   Arc<tokio::sync::Mutex<Option<RoomConn>>>>` on `SessionStore`; lazy open
   (`open_session_with_owner` + `open_managed_connection` from
   `resolve_connection_spec`); `close_room_connection` on bind (rebind) and
   unbind. The async mutex also serializes member queries (decision 2).
3. **Submitter pre-authorization** — landed. `room_submitter_scope` builds the
   submitter's scope; `authorize(ExecuteQuery)` + `sql_policy::enforce` run in
   the room-execute pre-check before routing.
4. **Routing + `RoomConnectionUnbound`** — landed. `execute_query` routes a
   bound room to `execute_room_query`; an unbound room is hard-rejected.
5. **Tests** — `MockDriver` integration: bound room routes end-to-end (200 +
   room-attributed history); unbound rejected (400); submitter-role gating —
   viewer denied (403), editor routes (200).

## Remaining

- **Policy-blocked E2E coverage.** Viewer-denied is now covered end-to-end; an
  end-to-end test for a policy-blocked editor (read-only profile rejecting a
  write) is a follow-up (the `sql_policy::enforce` path is already unit-proven).
- **Pageable result reference.** The viewer-observes-results broadcast
  (`shared-connection-ownership.md`) is the last independent G9 slice — and
  bigger than a broadcast. Investigated during G9: room queries run through the
  **HTTP** execute path, where `drain_stream_inner` always returns
  `has_more: false` and **errors `ResultTooLarge`** past the cap — it never
  spills a pageable cursor. `/v1/cursors/:id/pages` also authorizes by
  `session_owner == requester`. So the real prerequisite is routing room
  execution through the **streaming/cursor** path (WS execute + spill); only
  then do the broadcast (`RoomQueryResult { cursor_id, has_more }`) and a
  room-scoped cursor-authz relaxation (admit any member of the cursor's room,
  read-only) deliver it. Full corrected design in
  `shared-connection-ownership.md`. Gate on streaming room execution.

## Landed teardown

The room connection is closed on unbind/rebind and when a room empties: after a
room WebSocket handler returns, if `RoomRuntime::is_active` reports the room was
evicted (last subscription dropped), `close_room_connection` runs. Because open
is lazy, a race with a rejoin self-heals (the next execute reopens). Membership
revocation flows through the same path — a revoked member's WS ends, and if it
was the last, the connection closes.
