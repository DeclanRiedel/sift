# Plan — Headless Collab Infrastructure

Status: active. This is the current plan of record.

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
  mode resolves to the bootstrapped local principal.
- Headless HTTP routes now cover tenant listing, room create/list/delete,
  room member join/leave/add/remove/list, document create/list/update/delete,
  connection profile create/list/delete, per-user credential set,
  open-session-connection-from-profile, token issue/list/revoke, room-scoped
  history, and principal-scoped history.
- Synchronous SQLite metadata work in HTTP handlers runs via `spawn_blocking`
  for the local/headless route surface.
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

## Next Headless Milestones

### H1 — Repo Guardrails

- GitHub Actions CI for format, clippy, and tests.
- `cargo-deny` policy baseline.
- Keep `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`,
  and `cargo test --workspace` green.

### H2 — Metadata Hardening

- Multi-row metadata writes are transactional.
- Credential replacement deletes old secret handles on a best-effort basis.
- Room/document delete/remove APIs.
- Personal-room visibility rules are enforced in queries and later routes.

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
- [x] Add OpenAPI coverage and operation-log entries for metadata routes.

### H6 — `sift-doc`

- [x] Minimal CRDT abstraction crate.
- [x] Initial text document helpers.
- [x] Snapshot/text extraction APIs.
- [ ] Apply operation API once CRDT backend is selected.
- Keep Loro/Automerge choice hidden behind this crate.

### H7 — Protocol Room Surface

- Room/document operation variants.
- Priority WebSocket class for presence/doc ops.
- Keep existing stream class for query pages.

### H8 — Room Runtime

- Introduce room attachments and in-memory presence.
- Move document editing through CRDT operations.
- Later, replace session-only result handling with room-aware fanout.

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
- OpenAPI is manually assembled and will become harder to maintain as routes
  grow.
- Metadata uses synchronous SQLite behind a mutex and HTTP handlers now isolate
  sync store calls with `spawn_blocking`; hosted mode may still want a metadata
  actor or pool.
- API tokens issued before token lookup migration cannot be verified by lookup;
  no server release used them, so this is acceptable.
- Metadata route coverage is intentionally headless and minimal; permissions
  are tenant-scoped today and need role-aware room authorization before hosted
  multi-user mode.
