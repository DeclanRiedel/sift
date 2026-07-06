# Plan — Headless Collab Infrastructure

Status: implemented for the foundation slice. Keep this as the record of what
the headless collaboration infrastructure now provides.

## What Are We Building?

Sift is a server-first, collaboration-native database IDE substrate: "Zed for
databases." The product core is the headless server, not a UI process. Clients
are thin renderers over a shared protocol.

The first durable unit is a **room**:

```
Tenant
  Principal
  ConnectionProfile
  Room
    RoomMember
    Document       -- CRDT snapshot bytes, opaque to metadata
    Attachment     -- live client attachment record
    QueryHistory   -- actor-attributed, optionally room-scoped
```

Single-user local mode is a room with one member. Multi-user collaboration is
the same model with more attachments and members.

## Current Foundation

Implemented:

- `sift-metadata` crate with embedded SQLite/refinery migrations.
- Identity schema: tenant, principal, membership, API token, principal key,
  keypair challenge.
- Connection profile schema and store methods with secrets outside SQLite.
- API token lookup keys: tokens are `sift_<lookup>_<secret>`, Argon2-verified.
- Room/document infrastructure:
  - `room`
  - `room_member`
  - `document`
  - `room_attachment`
  - `query_history.room_id`
- Metadata APIs for creating/listing rooms, room members, opaque document
  snapshots, active room attachments, room-scoped history, and
  principal-scoped history.
- Existing Phase 0 server remains intact: sessions, inline connection specs,
  HTTP execute, WebSocket execute/listen, operation log, OpenAPI, SDK.
- `sift-server` constructs the local metadata store at startup, bootstraps the
  local tenant/principal, and exposes a headless metadata/auth HTTP surface.
- Metadata-backed API tokens can authenticate metadata routes; local loopback
  mode resolves to the bootstrapped local principal. Tenant-scoped tokens only
  authorize the tenant they were issued for.
- Headless HTTP routes now cover tenant listing, room create/list/delete,
  room member join/leave/add/remove/list, document create/list/update/delete,
  connection profile create/list/delete, per-user credential set,
  open-session-connection-from-profile, token issue/list/revoke, room-scoped
  history, and principal-scoped history.
- Room HTTP routes enforce room roles:
  - viewers can read room members, documents, and room-scoped history;
  - editors can create/update/delete documents and attach query history
    context;
  - owners can manage room membership and delete rooms.
- HTTP execute accepts optional `room_id` and `connection_profile_id` metadata
  context. When supplied, the server validates room/profile access and records
  actor-attributed query history after execution.
- OpenAPI metadata/auth routes now reference typed request and response
  schemas instead of anonymous object payloads.
- `sift-doc` exposes a backend-agnostic apply-operation API for text
  documents, with UTF-8 boundary validation.
- `sift-client-sdk` exposes typed metadata/auth methods and a room
  document-operation WebSocket helper.
- Room runtime exists for the headless foundation:
  - room WebSocket attachment/detachment;
  - in-memory presence;
  - document-operation broadcast;
  - persisted document snapshot updates through `sift-doc`;
  - room-aware operation audit.
- Room-aware query result handling has a foundation shape: direct session
  execution remains unchanged, room/profile context records history, and room
  WebSocket clients receive query result summaries. Full result broadcast
  streams remain deferred.
- Synchronous SQLite metadata work in HTTP handlers runs via `spawn_blocking`
  with explicit backpressure for the local/headless route surface.
- `sift-doc` exists as the first pure document abstraction crate for opaque
  CRDT snapshots and text helpers.
- The current read routes use `GET`; the new HTTP `QUERY` method is reserved
  for future safe/idempotent reads that need a request body. SQL execute stays
  `POST` because SQL can mutate databases.

Deliberately not implemented yet:

- UI.
- Full `SessionStore` to `RoomStore` rewrite.
- Broadcast/fanout result streams.
- Keypair auth.
- OIDC.
- Voice/video.
- Follow mode.
- Web client decision.

## Architecture Rules

1. `sift-protocol` stays pure serde data: no I/O, no Tokio, no OS APIs.
2. UI dependencies must not enter shared crates.
3. Metadata stores opaque CRDT bytes only; `sift-doc` owns document-facing
   semantics.
4. Secrets never live in SQLite; SQLite stores opaque secret handles.
5. Existing `/v1/sessions` APIs remain compatible until room APIs can fully
   replace them.

## Headless Milestone Status

### H1 — Repo Guardrails

- [x] GitHub Actions CI for format, clippy, tests, and cargo-deny.
- [x] `cargo-deny` policy baseline.
- [x] Keep `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`,
  and `cargo test --workspace` green.

### H2 — Metadata Hardening

- [x] Multi-row metadata writes are transactional where needed.
- [x] Credential replacement deletes old secret handles on a best-effort basis.
- [x] Room/document delete/remove APIs.
- [x] Personal-room visibility rules are enforced in list/join routes.
- [x] Blocking metadata work has explicit backpressure.

### H3 — Server Metadata Wiring

- [x] Add metadata config:
  - path
  - secret backend
  - local bootstrap
- [x] Construct `MetadataStore` at server startup.
- [x] Use blocking-safe wrappers or `spawn_blocking` for SQLite work from async
  handlers.

### H4 — Auth Context

- [x] Add `AuthContext` and principal resolver:
  - loopback bypass for local mode
  - metadata-backed API tokens
  - existing shared bearer token only as compatibility fallback
- [x] Add tenant scoping helper.

### H5 — Headless Metadata Routes

- [x] Tenants: list current principal's tenants.
- [x] Rooms: create/list/delete.
- [x] Rooms: join/leave/member management.
- [x] Documents: create/list/update snapshot/delete.
- [x] Connection profiles: create/list/delete/set credential/open from profile.
- [x] Query history: list by room.
- [x] Query history: list by principal.
- [x] HTTP execute records room/profile-attributed query history when metadata
  context is supplied.
- [x] Room routes enforce owner/editor/viewer permissions.
- [x] Add OpenAPI coverage and operation-log entries for metadata routes.

### H6 — `sift-doc`

- [x] Minimal CRDT abstraction crate.
- [x] Initial text document helpers.
- [x] Snapshot/text extraction APIs.
- [x] Apply operation API behind crate-local document semantics.
- Keep Loro/Automerge choice hidden behind this crate.

### H7 — Protocol Room Surface

- [x] Room/document operation variants.
- [x] Dedicated room WebSocket class for presence/doc ops.
- [x] Existing session stream class remains the query page path.

### H8 — Room Runtime

- [x] Introduce room attachments and in-memory presence.
- [x] Move document editing through document operations.
- [x] Add room-aware query result summary events without full result fanout.

## Deferred Until The Headless Layer Is Stable

- GPUI desktop crate.
- Web client.
- Result broadcast and observer lag recovery.
- Keypair remote auth.
- OIDC setup/login.
- Voice/video.
- Follow-mode UI polish.
- Packaging/notarization.

## Known Design Gaps

- `SessionStore` is still session-centric and single-stream-per-WebSocket.
- OpenAPI still has a hand-authored path map, though metadata/auth payloads now
  use typed schemas.
- Metadata uses synchronous SQLite behind a mutex and HTTP handlers isolate
  sync store calls with bounded `spawn_blocking`; hosted scale may still justify
  a metadata actor or connection pool.
- API tokens issued before token lookup migration cannot be verified by lookup;
  no server release used them, so this is acceptable.
- Full query result broadcast streams and observer lag recovery remain
  deferred beyond this foundation.
