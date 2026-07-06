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
  snapshots, active room attachments, and room-scoped history.
- Existing Phase 0 server remains intact: sessions, inline connection specs,
  HTTP execute, WebSocket execute/listen, operation log, OpenAPI, SDK.

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
3. Metadata stores opaque CRDT bytes only; a future `sift-doc` crate owns CRDT
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

- Add metadata config:
  - path
  - secret backend
  - local bootstrap
- Construct `MetadataStore` at server startup.
- Use blocking-safe wrappers or `spawn_blocking` for SQLite work from async
  handlers.

### H4 — Auth Context

- Add `AuthContext` and principal resolver:
  - loopback bypass for local mode
  - metadata-backed API tokens
  - existing shared bearer token only as compatibility fallback
- Add tenant scoping helper.

### H5 — Headless Metadata Routes

- Tenants: list current principal's tenants.
- Rooms: create/list/join/leave/member management.
- Documents: create/list/update snapshot/delete.
- Connection profiles: create/list/delete/set credential/open from profile.
- Query history: list by principal and by room.
- Add OpenAPI coverage and operation-log entries.

### H6 — `sift-doc`

- Minimal CRDT abstraction crate.
- Initial text document helpers.
- Snapshot/apply/text extraction APIs.
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
- `sift-server` does not yet depend on `sift-metadata`.
- OpenAPI is manually assembled and will become harder to maintain as routes
  grow.
- Metadata uses synchronous SQLite behind a mutex; route integration must avoid
  blocking async workers directly.
- API tokens issued before token lookup migration cannot be verified by lookup;
  no server release used them, so this is acceptable.
