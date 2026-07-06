# Plan — Collab-Native Pivot

Status: proposed, urgent. Owner: TBD. Sibling doc: `docs/PLANS/metadata-store.md` (auth/tenant/secret foundation remains valid).

## 1. Decision

**Sift is "Zed for databases," not "Navicat-with-a-server."** Real-time cowork — shared cursors, shared editor buffer, shared query execution, follow mode, and eventual voice/video — is a **defining** feature, not a bolt-on.

Consequence: every design decision made from this point forward must assume multi-attach rooms are the default. Single-client behavior is the degenerate case (room of size 1), not the base case.

**Explicitly deferred**, do not design for these yet:
- Hosted service ownership / monetization / SaaS shape.
- Billing, quotas, seat licensing.
- Any decision about who runs "the" sift server (self-host vs sift.dev).

Rationale: those decisions constrain infra and require product/business input. Core infrastructure — CRDT documents, room membership, presence, shared execution, human auth, a client — must land first and be sound. Ownership choices are cheap to make later; foundation choices are not.

## 2. What just landed and why it matters right now

Commits `ecb4ff9` (metadata store foundation) and `9d77bfa` (API-token lookup keys) shipped the auth/tenant scaffolding described in `docs/PLANS/metadata-store.md`. Migrations V001–V005 are now in `crates/metadata/migrations/`. Nothing on top of this scaffolding is built yet — the HTTP surface still uses the pre-metadata inline-spec model.

**This is the last good moment to bake collab primitives in without a rewrite.** Every week that server handlers, protocol types, and `SessionStore` semantics pile up assuming single-client-per-session, the pivot gets more expensive.

Three specific shapes in the shipped code that are silently single-user:

| Location | Current shape | Problem for collab |
|---|---|---|
| `crates/metadata/migrations/V003__workspaces.sql:5` | `workspace.principal_id NOT NULL` | A workspace is owned by one principal. Rooms are tenant-scoped and multi-attach; membership is a separate relation. |
| `crates/metadata/migrations/V003__workspaces.sql:24` | `tab.body_text TEXT` | SQL text as a plain string. Concurrent editing needs a CRDT payload, not a `TEXT` blob. |
| `crates/server/src/session.rs` (whole file) | `Session` = 1 WS-client. No membership. `ResultSetStream` uses `mpsc::Receiver` (single consumer). | Rooms need N attachments and broadcast fanout. |

None of these are wrong today, but they will lock in wrong assumptions if left uncorrected before the next 500 lines of code land on top.

## 3. Non-goals for the pivot Phase 1

Keep scope honest — the pivot doesn't mean "build everything Zed has at once":

- **No voice/video yet.** LiveKit / WebRTC is a distinct workstream after text collab works. Wire protocol should not preclude it; runtime should not depend on it.
- **No follow-mode UI polish.** Server broadcasts viewport state; client rendering of "following Alice" is deferred until a real desktop client exists.
- **No CRDT undo history sync.** Local undo per-client is fine for Phase 1; shared undo is a Phase 2 refinement.
- **No offline editing / merge on reconnect.** Room requires an active session. Reconnect within a short window resumes; long disconnect = rejoin fresh.
- **No hosted service.** Everything runs local or self-hosted.

## 4. New primitives to add now (before more code lands)

### 4.1 Rooms replace single-owner workspaces

`workspace` in V003 is renamed conceptually to `room` and gains proper membership. Because the metadata store is only two commits old and unused, migrating now is cheap.

Add **V006__rooms.sql**:

```
-- Rename workspace → room, drop principal_id, add creator_id
CREATE TABLE room (
    id INTEGER PRIMARY KEY,
    tenant_id INTEGER NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('personal', 'shared')),
    created_by INTEGER NOT NULL REFERENCES principal(id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE room_member (
    room_id INTEGER NOT NULL REFERENCES room(id) ON DELETE CASCADE,
    principal_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('owner', 'editor', 'viewer')),
    joined_at TEXT NOT NULL,
    PRIMARY KEY (room_id, principal_id)
);

CREATE INDEX idx_room_tenant ON room(tenant_id);
CREATE INDEX idx_room_member_principal ON room_member(principal_id);
```

`kind = 'personal'` rooms auto-attach only their creator and are hidden from the tenant. This preserves the single-user UX: opening a fresh sift on your laptop still gets you a private space with zero setup.

`kind = 'shared'` rooms are visible to all tenant members with a `room_member` row.

Drop the existing `workspace` / `session_snapshot` / `tab` tables in the same migration. **Do this now**, before anything reads or writes them. The 60 LOC of migration code is cheaper than the code that will pile on top of the wrong shape.

### 4.2 Documents are CRDT payloads, not TEXT

Replace `tab.body_text TEXT` with a CRDT-backed document primitive.

Add **V007__documents.sql**:

```
CREATE TABLE document (
    id INTEGER PRIMARY KEY,
    room_id INTEGER NOT NULL REFERENCES room(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,                        -- 'sql' | 'result_view' | 'notebook' | ...
    title TEXT NOT NULL,
    crdt_type TEXT NOT NULL CHECK (crdt_type IN ('loro', 'automerge')),
    crdt_state BLOB NOT NULL,                  -- serialized CRDT doc; opaque to sift
    position INTEGER NOT NULL,                 -- ordering within the room
    connection_profile_id INTEGER REFERENCES connection_profile(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_document_room_position ON document(room_id, position);
```

The CRDT itself lives in a new `sift-doc` crate (§5). The metadata store treats `crdt_state` as opaque bytes.

**Choice: Loro over Automerge.** Reasons:
- Actively developed, Rust-native, no wasm indirection.
- Better performance on rich text (relevant for query editors with syntax highlights that may want annotation ranges).
- Excellent snapshot/incremental-update story.
- Automerge is fine but has a larger footprint and its Rust integration is less ergonomic. Revisit if Loro proves unstable.

### 4.3 Room attachments (live sessions) and presence

`session_snapshot` in V003 conflates two ideas: "session was open" and "state to restore." Split them.

Add **V008__attachments.sql**:

```
CREATE TABLE room_attachment (
    id INTEGER PRIMARY KEY,
    room_id INTEGER NOT NULL REFERENCES room(id) ON DELETE CASCADE,
    principal_id INTEGER NOT NULL REFERENCES principal(id) ON DELETE CASCADE,
    client_id TEXT NOT NULL,                   -- opaque per-ws-connection UUID
    attached_at TEXT NOT NULL,
    detached_at TEXT                           -- NULL while attached
);

CREATE INDEX idx_room_attachment_active ON room_attachment(room_id) WHERE detached_at IS NULL;
```

Presence itself (cursor, selection, follow target) is **in-memory only** — DashMap in the server. Never persisted; too high-frequency and useless after disconnect.

### 4.4 Query history: attribute to actor, not to owner

Current `query_history` in V004 (per-principal) is still correct — history is private. But now history rows need a `room_id` so "what ran in this room" is queryable for shared debugging.

Add **V009__history_room.sql**:

```
ALTER TABLE query_history ADD COLUMN room_id INTEGER REFERENCES room(id) ON DELETE SET NULL;
CREATE INDEX idx_query_history_room ON query_history(room_id, started_at DESC);
```

## 5. New crate: `sift-doc`

Isolates the CRDT layer so the rest of the codebase never imports `loro` directly.

```
crates/doc/
  Cargo.toml
  src/
    lib.rs           -- Document, DocumentOp, DocumentSnapshot
    loro_backend.rs
    text.rs          -- text-doc helpers (SQL body content)
    ops.rs           -- server-side op validation / serialization
```

Public surface (sketch):

```rust
pub struct Document { /* Loro doc handle */ }

impl Document {
    pub fn new_text() -> Self;
    pub fn from_snapshot(bytes: &[u8]) -> Result<Self>;
    pub fn snapshot(&self) -> Vec<u8>;

    pub fn apply(&mut self, op: DocumentOp) -> Result<AppliedOp>;
    pub fn text(&self) -> String;               -- flat read for executing SQL
    pub fn version(&self) -> DocVersion;
}

pub enum DocumentOp {
    TextInsert { pos: usize, text: String, actor: ActorId, lamport: u64 },
    TextDelete { pos: usize, len: usize, actor: ActorId, lamport: u64 },
    // richer ops when they're needed
}
```

The server never edits text as a String; it applies `DocumentOp`s and re-reads `.text()` when it needs to execute the SQL. This keeps the CRDT the single source of truth.

## 6. Protocol additions (`sift-protocol`)

### 6.1 Room lifecycle

```
Operation:
  CreateRoom { tenant_id, name, kind }
  ListRooms { tenant_id }
  JoinRoom { room_id }
  LeaveRoom { room_id }
  InviteMember { room_id, principal_id, role }
  RemoveMember { room_id, principal_id }

  CreateDocument { room_id, kind, title, connection_profile_id? }
  ListDocuments { room_id }
  DeleteDocument { room_id, document_id }
```

### 6.2 WebSocket protocol: two message classes

Presence and document ops are **high-frequency**; result pages are **backpressured**. Mixing them on the current ACK-gated single-inflight channel is wrong.

Split `WsClientMessage`/`WsServerMessage` into two classes:

```
// Existing class — backpressured, ACK-gated, single-inflight per stream
Execute, Ack, Cancel, Started, Page (query results)

// New class — priority, no ACK, coalesced client-side
Cursor { document_id, position, selection }
DocOp { document_id, op: DocumentOp }
Presence { principal_id, cursor?, selection?, following? }
FollowRequest { target_principal_id }
Attach { room_id, client_id }
Detach

// Server broadcasts
DocOpBroadcast { document_id, op, from: principal_id }
PresenceBroadcast { room_id, principal_id, cursor?, ... }
MemberJoined { room_id, principal_id }
MemberLeft { room_id, principal_id }
```

The transport can be one WebSocket with a `class: "priority" | "stream"` field on each message, or two WebSockets per client. **Recommendation: one WS, class field.** Two WS means two auth handshakes and two reconnect stories.

### 6.3 Result broadcast

When a room member runs a query, the result stream broadcasts to the whole room. Change `ResultSetStream`'s internal channel from `mpsc::Receiver<Page>` to `tokio::sync::broadcast::Receiver<Page>` (fan-out to N attached WSs). Each attached client independently ACKs its own page consumption.

Design detail: **the initiator's ACK gates the driver**. Slow observers do not slow the DB — they drop pages (with a `Lagged` marker) and get a "resync from cursor" op. This is how Zed handles slow followers.

## 7. Server changes (`sift-server`)

### 7.1 `SessionStore` becomes `RoomStore`

- `Session` (current: 1 WS per session) is renamed `Attachment`; it's per-WS-connection.
- New `Room` struct holds `attachments: DashMap<ClientId, Attachment>`, `documents: DashMap<DocumentId, Document>`, `presence: DashMap<PrincipalId, PresenceState>`.
- Result streams are broadcast within a room.
- `SessionStore::execute_stream` becomes `RoomStore::execute_in_room(room_id, initiator, document_id, params)`. Every attached client sees the result.

### 7.2 Priority channel

Add a second per-WS message pump for the priority class. Priority messages bypass the ACK gate. Coalescing: rapid cursor updates from one client collapse to the latest before broadcast (server-side debounce, ~30ms).

### 7.3 CRDT persistence

Documents are persisted on a debounced timer (~500ms after last op) as snapshots. On room open, load latest snapshot; apply any newer ops (kept in a bounded in-memory ring for crash recovery).

## 8. Revised milestone ordering

Replaces M1–M10 in `docs/PLANS/metadata-store.md`. **The metadata store as shipped stays** — auth/tenant/secret work is retained. The rest reorders around collab.

| # | Deliverable | Why now |
|---|---|---|
| **C1** | Migrations V006–V009 — `room` / `room_member` / `document` / `room_attachment` + drop `workspace`/`session_snapshot`/`tab` | Do this **before** anything reads or writes the old tables. Cheap because nothing consumes them yet. |
| **C2** | `sift-doc` crate with Loro backend + text doc + snapshot/apply | CRDT primitive must exist before any protocol op references documents. |
| **C3** | Protocol split: priority class vs stream class; add `Cursor`/`DocOp`/`Presence`/`Attach`/`Detach` variants | Wire shape locks quickly; do it before HTTP handlers get written against the old shape. |
| **C4** | `RoomStore` replaces `SessionStore`; broadcast-backed result streams | Server orchestration is the last thing you want to refactor after handlers exist. |
| **C5** | Auth middleware (loopback bypass, API-token via metadata) — from existing plan M3 | Independent of collab; unblocks everything. |
| **C6** | Room + document HTTP routes + operation-log wiring | Turns C1–C4 into a usable surface. |
| **C7** | Keypair auth — from existing plan M4 | Needed for the desktop client hitting a remote server. |
| **C8** | Minimal desktop client shell (GPUI or web-first — decide separately) with: room list, document editor, run query, see other cursors | You cannot validate collab without a client. This shortens the feedback loop from "quarters" to "weeks." Any UI, even ugly. |
| **C9** | Presence coalescing + follow mode server broadcast | Refinement on top of C4/C8. |
| **C10** | Query history with `room_id` (V009 wiring) + shared history view in client | Small; unlocks a "what did the team run last night" view. |
| **C11** | OIDC (from existing plan M9) — deferred until the first non-solo deployment | Not blocking. |
| **C12** | Voice/video — separate track, no schedule commitment | Distinct workstream. Client-side WebRTC data channels + optional LiveKit later. |

**C1–C4 are the "before more code lands" bloc** — the batch that has to happen before HTTP handlers or client work resumes on the old shape. Roughly 2–3 weeks of focused work, and it protects everything downstream.

**C5–C7 finish auth.** **C8 is the pivot payoff** — first real collab demo.

## 9. Which piece of the earlier gap analysis this addresses

| Gap called out earlier | Addressed by |
|---|---|
| CRDT document model | C1 (schema), C2 (crate), C3 (protocol) |
| Room / multi-attach session | C1 (schema), C4 (server) |
| Presence protocol | C3 (protocol), C4 (server), C9 (refinement) |
| Follow mode / shared viewport | C3 (protocol), C9 |
| Persistent buffer state | C1 + C7.3 (CRDT persistence) |
| Auth for humans | C5 (API tokens), C7 (keypair), C11 (OIDC) |
| A desktop client | C8 |
| Voice/video | C12 (deferred, not designed against) |
| Ownership of collab server infra | **Explicitly deferred.** Not addressed by any milestone. |

## 10. Risks and open questions

- **Loro version stability.** It's actively developed but pre-1.0. Isolating it in `sift-doc` means the blast radius of a swap-to-Automerge is one crate.
- **Broadcast channel overflow.** Fast producer + slow consumer → dropped pages. Design detail: initiator ACKs gate the driver, observers get `Lagged` + resync-from-cursor. Confirmable in C4.
- **Coalescing rules for presence.** ~30ms server-side debounce per (principal, doc). Tune with a real client (C8).
- **Migrating away from `workspace`/`session_snapshot`/`tab` costs nothing today, everything in a month.** Prioritize C1.
- **CRDT undo across clients.** Not attempted in Phase 1. Local undo per-client only.
- **Which client platform first — GPUI desktop or web?** Web-first shortens the feedback loop (any browser can join a room), but GPUI is the eventual target. Recommendation: **web-first for C8**, GPUI reused once web proves the protocol. Sanity-check with client-owner before committing.
- **`room.kind='personal'` UX.** Personal rooms preserve the Navicat-like local flow. Make sure they don't show up in tenant room lists — enforce at query, not just at membership.
- **Room-scoped connections?** Should a connection be tenant-scoped (current) or room-scoped (Zed-style project-scoped)? Recommend tenant-scoped, referenced from documents. Deferrable to a later refinement.

## 11. What this locks in

After C1–C4 ship:
- The **wire protocol** commits to room-and-document-shaped operations. Non-breaking additions still possible; the single-session-per-WS assumption is gone.
- The **schema** commits to rooms, members, CRDT-blob documents, room attachments. Old workspace/tab shapes disappear.
- The **server orchestration** commits to broadcast-first result streams. Fanout is a first-class concern, not a retrofit.

None of this precludes anything from the deferred list (voice, follow polish, offline, hosted). All of it precludes a lot of future rework.
