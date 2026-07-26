# Design — Shared-connection ownership (Phase G)

> Status: **drafted.** Phase G collaboration item G5. A connection used inside a
> room becomes **room-owned**: the room binds exactly one connection profile,
> every member's room-scoped execute runs server-side through that bound
> connection, and per-op authorization is the **intersection** of the
> submitter's room role and the ADR-020 policy. Composes over the existing
> `authorize()` evaluator, cursor registry, and execute path — **no new
> `Driver` method** (ADR-017 lock preserved).
>
> Implies **ADR-036 (candidate): room-owned connection binding + submitter-
> scoped authorization.** Weight on README **Goal #4 (Zed-class snappiness)**:
> members join a live shared session and run against it immediately, without
> re-entering credentials, while viewers observe results without holding any
> connection of their own.

## Where we are

- Room-scoped execute already checks `RoomPermission::Write` and resolves a
  **per-principal** connection profile the client passes in:
  `get_connection_profile_for_principal(profile, auth.principal_id)`
  (`http.rs:2828-2831`). Each member runs through *their own* profile — there
  is no room-owned connection.
- The room row has **no** bound-connection field (`schema.rs:358-366`).
- Authorization intersection **already exists**: `authorize(scope, op)`
  composes `tenant_role`, `room_role`, and the profile's `connection_policy`
  (`authorization.rs:59-115`). `ExecuteQuery` is a connection operation, so a
  room `Viewer` is already denied with `RoomEditorRequired`
  (`authorization.rs:102-103`). The gating logic is done; the missing piece is
  connection *ownership*, not gating.
- The room broadcast carries a `RoomQueryResult` **summary** only
  (`http.rs:2889-2908`) — row count + status, not a reference peers can page.

## Decisions locked (this session)

1. **Room binds one connection (server-owned).** The room row gains
   `bound_connection_profile_id: Option<ConnectionProfileId>`. All room-scoped
   executes route through it server-side, regardless of who submits; a
   client-supplied profile is ignored for room-scoped execute.
2. **Credentials = the binder's profile (v1).** Binding an owner's connection
   profile lends its DB credentials to the room for the binding's lifetime.
   Unbind revokes. A dedicated room service credential is deferred.
3. **Role gating = intersection (ceiling × floor).** Effective permission for
   a room execute = room role allows it **and** ADR-020 policy allows it —
   evaluated by the existing `authorize()` with the submitter's scope. No
   second authorization model.

## Model

### Binding (metadata)

- `Room` + `bound_connection_profile_id: Option<ConnectionProfileId>` (nullable
  FK; SQLite migration adds the column, default `NULL`).
- Two operations, both **room `Owner`**-gated and requiring that the binder can
  access the target profile (`get_connection_profile_for_principal`):
  - `BindRoomConnection { room_id, connection_profile_id }`
  - `UnbindRoomConnection { room_id }`
- Both are typed `Operation`s (audited like every other), and the bound
  profile must belong to the room's tenant (reject cross-tenant binds).

### Execute path (`execute_metadata_context` + execute)

When `room_id` is present:

1. Ignore any client-supplied `connection_profile_id`; resolve the room's
   `bound_connection_profile_id`. If unbound → `RoomConnectionUnbound` error
   ("bind a connection to this room before running queries").
2. Build the **submitter's** `AuthorizationScope`: `tenant_role` from tenant
   membership, `room_role` from room membership, `connection_policy` from the
   **bound** profile.
3. `authorize(scope, OperationKind::ExecuteQuery)` — the existing evaluator
   enforces the intersection. Viewer → `RoomEditorRequired`; editor whose op is
   blocked by the bound profile's policy → `OperationBlocked`/`TenantRoleTooLow`.
4. Run against the bound profile's connection (binder's credentials). The
   submitter never needs their own profile for the shared session.

> **Landed vs deferred.** `execute_http` selects the live driver connection by
> `ExecuteRequestHttp.connection` (a `ConnectionId`), not by
> `connection_profile_id` (which is attribution/authz provenance only). So the
> increment that landed with ADR-036 is: the room→connection **binding**
> (metadata + owner-gated bind/unbind + audit) and **attribution override** —
> when a room is bound, room-scoped execute attributes history to the bound
> profile. The **hard `RoomConnectionUnbound` rejection (step 1) and the actual
> routing of the query through a server-held room connection (step 4) land
> together in G9** ("shared room connection"), because a reject without routing
> would block members while queries still ran on their own connections — a
> guardrail divorced from the security property. Steps 2–3 already hold: the
> intersection evaluator (`authorize()`) is live and `RoomPermission::Write`
> already denies viewers at the room-permission gate.

### Attribution

Audit and history attribute the **submitting principal**, never the binder —
`RoomQueryResult.actor_principal_id` and `NewQueryHistory.principal_id` already
carry the submitter (`http.rs:2866, 2901`). The binder is recorded once, at
bind time, on the `BindRoomConnection` operation.

### Viewers observe result references

A room `Viewer` cannot execute (step 3 denies it) but observes results. The
result broadcast graduates from summary to a **pageable reference**:

- On a successful room execute, the server registers the result in the existing
  server-side **cursor registry** and broadcasts a reference —
  `{ cursor_id, row_count, page_size, server_version }` — on the **ephemeral
  presence lane** (per G4 / ADR-035; a lost reference is recoverable by
  re-query, and results are outside the ADR-014 CRDT scope).
- Peers (including viewers) page the shared result through the normal
  cursor-fetch endpoint under their **own** room role — read-only paging is
  allowed for viewers; this is the intended "viewer observes result
  references." Fan-out result *replication* remains explicitly out of scope
  (ADR-014): peers page the one server-held cursor, they do not receive copies.

> The full pageable-reference wiring is Phase G implement item G9; this design
> fixes its **shape** (registry cursor id on the ephemeral lane, submitter-
> scoped paging) so the implement step has no open contract questions.

## Non-goals

- **No dedicated room service credential.** v1 lends the binder's profile;
  managed per-room credentials + rotation are a later phase.
- **No cross-room / multi-connection rooms.** Exactly one bound connection per
  room.
- **No new authorization model.** Room execute consumes the Phase F evaluator;
  this design adds ownership + a resolution rule, not policy.
- **No result replication.** Reference-and-page only (ADR-014).

## Tests

- Bind requires room `Owner`; editor/viewer bind attempts denied.
- Cross-tenant profile bind rejected.
- Room-scoped execute with no bound connection → `RoomConnectionUnbound`.
- Room-scoped execute ignores a conflicting client `connection_profile_id` and
  uses the bound one.
- Editor executes through the bound (binder's) connection; history attributes
  the **editor**, not the binder.
- Viewer execute denied (`RoomEditorRequired`); viewer *can* page the broadcast
  result cursor.
- An op blocked by the bound profile's ADR-020 policy is denied even for an
  editor (intersection holds).
- Unbind blocks subsequent room execute; an in-flight cursor already opened
  survives to completion.
