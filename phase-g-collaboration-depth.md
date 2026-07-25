# Phase G — Collaboration Depth

## Summary

Replace the placeholder byte-buffer document model with Loro-backed client and server replicas, durable update sequencing, reconnect/offline merge support, ephemeral presence, room-owned database connections, and transient shared result references.

Phase G begins with typed OpenAPI generation. The public protocol remains version `"1"` because there are no external users; the existing positional document-operation contract is removed rather than supported as a legacy mode.

## Locked product decisions

- Every client and the server hold a Loro replica.
- Clients generate native Loro updates; the server never generates positional edits on their behalf.
- Offline divergence is supported at the protocol level. Durable client storage waits for the product client.
- Loro is the only CRDT backend.
- Full Loro history is preserved; no automatic shallow snapshots.
- Updates are acknowledged and broadcast only after durable SQLite commit.
- Replica IDs are durable random IDs and cannot have two concurrent writers for one document.
- CRDT bytes use standard padded RFC 4648 base64 inside JSON messages.
- Audit attribution is the authenticated submitter, never client-controlled CRDT metadata.
- Presence includes active document and Loro-stable cursor/selection anchors.
- Presence is ephemeral, lease-based, and drives client-side follow mode.
- Shared connections are explicit, idempotent, managed-profile connections.
- Shared connections are autocommit-only.
- Shared execution runs an exact durable document version, optionally restricted to a UTF-8 byte range.
- Current room members, including viewers, may page shared results.
- Results are transient, independently pageable, and retained using bounded memory plus encrypted spill files.
- Query initiators may cancel their own query; room owners may cancel any query or close a shared connection.
- Rejected offline edits are kept locally for manual export; the server never resurrects deleted or unauthorized documents.

## Scope boundaries

Included:

- Loro text replication, persistence, synchronization, compaction, presence, lag recovery.
- Room-owned connection lifecycle and exact-version execution.
- Shared result references and independent readers.
- Protocol, SDK, OpenAPI, authorization, audit, quota, and integration coverage.

Excluded:

- Persistent client-side replica storage or a product GUI.
- Shared explicit transactions and savepoints.
- Durable result storage.
- Semantic current-statement selection; Phase G supports only full text or an explicit UTF-8 range.
- Automatic shallow-history truncation or manual history purge.
- Remote topology, plugins, workspace files, comments, rich-text marks, or CRDT state outside SQL text.

## G0 — Typed OpenAPI prerequisite

- Add `aide = "=0.13.5"` with Axum support. This release matches the repository’s Axum 0.7 and Schemars 0.8 versions.
- Replace the hand-authored OpenAPI path/schema map with `aide::axum::ApiRouter`.
- Preserve existing `schemars::JsonSchema` derives; do not introduce a second schema derive system.
- Declare response types, error responses, operation IDs, summaries, and security requirements beside each route.
- Register room WebSocket message types as OpenAPI components and describe the WebSocket endpoint’s message contract.
- Generate `/v1/openapi.json` once at startup and serve the immutable document.
- Add parity tests requiring:
  - Every public router method/path to appear in OpenAPI.
  - Every OpenAPI operation to have a stable operation ID.
  - Every supported HTTP operation to appear in an explicit client-SDK coverage manifest.
  - No orphan schema references.
- Remove the existing hand-maintained JSON map and source-text route scanner.

## Document model

### `sift-doc`

Replace `CrdtKind`, `DocumentSnapshot`, and positional `TextOperation` with a Loro-backed `TextReplica`.

The only allowed root container is a plain `LoroText` named `"text"`.

Expose:

- Create a replica from a random nonzero peer ID.
- Load from a full Loro snapshot.
- Read materialized UTF-8 text.
- Export a full snapshot.
- Encode/decode version vectors and frontiers.
- Export updates since a version vector.
- Import an update and return applied/no-op/pending information.
- Materialize or fork the text at a known frontier.
- Encode/decode stable Loro cursors.
- Validate that an update leaves exactly one plain-text root, contains no rich-text marks, and respects size limits.
- Export the complete replica state so a future client can persist the snapshot together with its peer ID.

All Loro CPU work runs outside Tokio request workers through a per-document blocking actor.

### Coordinate conventions

- CRDT editing uses Loro-native operations.
- Presence selections use encoded Loro cursors, not numeric offsets.
- Exact-version execution ranges use UTF-8 byte offsets into the materialized text at that version.
- Range boundaries must be valid UTF-8 boundaries.
- Client bindings must explicitly convert JavaScript UTF-16 editor offsets when constructing execution ranges.

## Persistence and migration

Add a metadata migration after V018.

Extend `document` with:

- `crdt_format_version INTEGER NOT NULL`
- `snapshot_seq INTEGER NOT NULL DEFAULT 0`
- `next_update_seq INTEGER NOT NULL DEFAULT 0`
- `snapshot_version BLOB NOT NULL`

Continue using `crdt_state` for the full snapshot bytes. Normalize every row’s `crdt_type` to `loro`.

Add `document_update`:

- `document_id`
- Per-document `server_seq`
- Client `update_id`
- `replica_id`
- Authenticated `submitted_by`
- Loro update bytes
- Decoded byte length
- Creation timestamp
- Unique `(document_id, server_seq)`

Existing `loro`- and `automerge`-labelled rows are known to contain raw UTF-8 rather than genuine CRDT snapshots. Migrate each by creating a Loro document with the existing text and storing a format-version-1 full snapshot. Invalid UTF-8 fails migration with the document ID and leaves the database unchanged.

Document creation no longer accepts a backend selector or arbitrary CRDT snapshot. It accepts metadata plus optional initial text; the server creates the canonical Loro snapshot.

Deleting a document cascades its update log. Old offline replicas cannot recreate it.

## Durable update path

Each loaded document has a serialized `DocumentActor` with committed and validation replicas.

For each update:

1. Authenticate the WebSocket lease and recheck current room membership.
2. Require room editor or owner role.
3. Reject a second live writer using the same `(document_id, replica_id)`.
4. Decode canonical base64 and enforce the 1 MiB decoded-update limit.
5. Import into the validation replica.
6. Reject corrupt updates, missing dependencies, extra containers, rich-text marks, or size-limit violations.
7. If the update contains no new operations, return an idempotent ACK without inserting or rebroadcasting.
8. Insert the update and increment the document sequence in one SQLite transaction.
9. Import the already-validated update into the committed replica.
10. If the in-memory commit unexpectedly fails, reload from SQLite before responding.
11. Send the submitter’s ACK and then publish the room event.

A persistence failure produces no ACK and no broadcast. Retrying the same Loro update is safe because import is idempotent.

Audit only applied updates. Store document ID, room ID, update ID, decoded byte count, submitter, server sequence, and a fingerprint of the resulting version. Never store update bytes, replica IDs, deleted text, or SQL text in the operation audit.

## Snapshot and update-log compaction

Persist a new full Loro snapshot when any condition is met:

- 256 updates since the previous snapshot.
- 4 MiB of appended update bytes.
- Five minutes since the last snapshot while dirty.
- Document actor idle eviction.
- Graceful server shutdown.

Under the document actor lock:

1. Export a full snapshot and encoded version.
2. Transactionally replace the stored snapshot and advance `snapshot_seq`.
3. Delete `document_update` rows through that sequence.
4. Keep later rows untouched.

Full Loro history remains inside the snapshot, preserving synchronization with arbitrarily old replicas. The 256 MiB per-document encoded-history cap is a hard safety limit; further edits return `document_too_large` until an operator raises the configured cap or a future purge design is implemented.

## WebSocket synchronization contract

Keep the existing room WebSocket endpoint, but replace positional document messages.

### Client messages

- `Reauthenticate`
- `Attach { client_id }`
- `Detach`
- `PresenceHeartbeat`
- `PresenceUpdate { active_document_id, selection }`
- `DocumentSync { request_id, document_id, replica_id, known_version }`
- `DocumentUpdate { request_id, update_id, document_id, replica_id, update }`

`known_version`, cursor values, and update payloads are typed base64 newtypes in `sift-protocol`.

### Server messages

- Authentication and attachment acknowledgements.
- Full presence snapshots.
- Chunked document snapshot/update transfers.
- Durable document-update ACKs.
- Committed document-update broadcasts.
- Room connection and result activity events.
- `ResyncRequired`.
- Structured errors containing stable `Code` and optional `request_id`.

### Initial and reconnect synchronization

- A new replica receives the latest persisted full snapshot followed by post-snapshot update rows.
- A known replica supplies its encoded version vector and receives the missing Loro update range.
- After importing the server response, an offline client exports everything missing from the returned server version and submits it as `DocumentUpdate`.
- Updates with unresolved dependencies return `crdt_dependencies_missing` and the current server version; the client restarts synchronization.
- Snapshot transfers use 256 KiB decoded chunks with transfer ID, index, count, payload kind, snapshot sequence, and final server version.
- Realtime updates remain single messages and are limited to 1 MiB decoded.

### Lag recovery

Each runtime broadcast carries:

- A server-start `runtime_epoch` UUID.
- A monotonically increasing in-memory `event_seq`.

If a broadcast receiver lags:

- Keep the socket open.
- Send `ResyncRequired`.
- Refresh presence from the current full snapshot.
- Resynchronize documents from each client version vector.
- Rediscover active room connections and results through HTTP.
- Do not create a durable general-purpose room event log.

A changed `runtime_epoch` after restart triggers the same recovery flow.

## Presence

Presence state contains:

- Attachment ID.
- Principal/display identity.
- Client ID.
- Optional active document ID.
- Optional selection with encoded Loro anchor and head cursors.

Defaults:

- Heartbeat interval: 10 seconds.
- Presence lease: 30 seconds.
- Sweep interval: 5 seconds.
- Presence updates coalesced to at most 20 broadcasts per second per attachment.
- Clean detach or socket close removes presence immediately.
- Presence is never persisted or audited.

Follow mode is entirely a client projection over presence, connection activity, execution events, and result references.

## Room-owned connections

Introduce `RoomConnectionRegistry` in `sift-server`.

### Ownership and provenance

Extend connection provenance so a managed connection is owned by either:

- A principal session, or
- A `(tenant_id, room_id, profile_id)` room scope.

`opened_by` is audit metadata, not connection ownership. Revoking the opener does not by itself destroy a room-owned connection.

Every operation re-evaluates:

- Current actor identity and room role.
- Tenant membership.
- Current profile revision.
- Read-only, allowed/blocked operations, and schema policy.
- Rate and tenant-resource limits.

A profile revision or revocation invalidates the shared connection using the existing hard-revocation machinery.

### HTTP routes

- `GET /v1/rooms/{room_id}/connections`
- `POST /v1/rooms/{room_id}/connections`
- `DELETE /v1/rooms/{room_id}/connections/{connection_id}`
- `POST /v1/rooms/{room_id}/connections/{connection_id}/execute`

Opening takes a managed profile ID. It is idempotent: one shared connection exists per room/profile. A broken connection is re-opened by repeating the open request; mutating operations are never automatically retried.

Connections idle out after 10 minutes with no active query. Only room owners may explicitly close them.

### Autocommit restriction

The shared path rejects:

- Transaction API operations.
- Savepoints.
- Transaction-control SQL such as `BEGIN`, `START TRANSACTION`, `COMMIT`, `ROLLBACK`, and engine equivalents.
- Multi-statement batches containing transaction-control statements.

Private sessions retain their existing transaction behavior.

## Exact-version execution

`RoomExecuteRequest` contains:

- `document_id`
- Encoded Loro frontier/version
- Optional `{ start, end }` UTF-8 range
- Parameters
- Existing execution options that are valid in autocommit mode

The document’s managed profile must match the shared connection’s profile.

The server:

1. Confirms the requested version exists in retained full history.
2. Materializes that exact version without modifying the live replica.
3. Validates and extracts the optional UTF-8 range.
4. Applies the acting editor’s central profile policy.
5. Rejects transaction-control SQL.
6. Starts an asynchronous result pump.
7. Returns `202 Accepted` with a `RoomResultReference`.

The result records document ID, exact version, selected range, actor, connection, timestamps, and status. Broadcasts do not contain raw SQL. Query history continues to follow `metadata.store_sql`.

## Shared results

Introduce an opaque UUID `RoomResultId`; do not expose the underlying driver `CursorId`.

### HTTP routes

- `GET /v1/rooms/{room_id}/results`
- `GET /v1/rooms/{room_id}/results/{result_id}`
- `GET /v1/rooms/{room_id}/results/{result_id}/pages?from_seq=&limit=`
- `POST /v1/rooms/{room_id}/results/{result_id}/cancel`

Pages are immutable and indexed by sequence. Every reader supplies its own `from_seq`; reading never advances another member’s cursor.

The result registry:

- Retains up to 16 MiB per result in memory.
- Spills additional pages to temporary ChaCha20-Poly1305-encrypted files.
- Uses a process-random key, so result files cannot survive restart as readable results.
- Creates files with owner-only permissions.
- Charges both memory and spill bytes to the existing tenant retained-result quota.
- Keeps at most 32 active results per room.
- Expires results 10 minutes after completion or last access.
- Deletes files on expiry, explicit cleanup, room deletion, and graceful shutdown.
- Treats all result references as expired after server restart.

Current room members may inspect pages. The initiating editor or a room owner may cancel; only the room owner may close the underlying connection.

## Protocol and operation changes

Remove:

- `CrdtKind::{Loro, Automerge}`
- Positional `TextDocumentOperation`
- `DocumentOperationEnvelope`
- The old `RoomQueryResult` summary contract
- `OperationKind::ApplyDocumentOperation`

Add typed IDs and base64 wrappers for replica IDs, document versions, frontiers, updates, cursors, room connections, and room results.

Add operation kinds and sanitized operation variants for:

- Apply document update
- Open/close room connection
- Execute room document
- Cancel room query
- Read shared result

Add stable error codes:

- `invalid_crdt_update`
- `crdt_dependencies_missing`
- `replica_in_use`
- `document_version_not_found`
- `document_too_large`
- `room_connection_not_found`
- `room_connection_broken`
- `room_result_not_found`
- `room_result_expired`

Continue using existing `Forbidden`, `RateLimited`, and `TenantResourceExhausted` codes where applicable.

## Authorization rules

- Viewer: attach, synchronize, update presence, observe activity, and page shared results.
- Editor: all viewer actions plus submit CRDT updates, open connections, and execute exact document versions.
- Query initiator: may cancel their own active query.
- Room owner: all editor actions plus cancel any room query and close shared connections.
- Presence and synchronization never grant connection permissions.
- Viewer result access is based on current room membership, not direct profile access.
- Membership removal immediately revokes WebSocket, document, connection, and result access.
- Deleted/unauthorized offline replicas receive a stable denial and retain their local state for manual export.

## Limits and configuration

Add `CollaborationConfig` with these defaults:

- `max_document_text_bytes = 8 MiB`
- `max_document_update_bytes = 1 MiB`
- `max_document_history_bytes = 256 MiB`
- `sync_chunk_bytes = 256 KiB`
- `snapshot_update_threshold = 256`
- `snapshot_log_bytes_threshold = 4 MiB`
- `snapshot_max_age_secs = 300`
- `document_idle_ttl_secs = 600`
- `presence_heartbeat_secs = 10`
- `presence_lease_secs = 30`
- `room_connection_idle_ttl_secs = 600`
- `room_result_memory_bytes = 16 MiB`
- `room_result_ttl_secs = 600`
- `max_room_results_per_room = 32`
- Optional operator result-spill directory

Decoded transfer bytes consume the existing stream-byte rate bucket. Sync/bootstrap uses the heavy-transfer bucket; updates and presence use the interactive bucket; execution uses the query bucket.

Extend tenant usage accounting for collaborative document history and room-owned connections while continuing to charge result data through retained-result bytes.

## Client SDK

Add an in-memory reference `RoomReplica` state machine:

- Construct from caller-supplied persisted peer ID and optional snapshot.
- Attach and synchronize.
- Import chunked server transfers.
- Export and retry missing local updates.
- Keep an update ID stable until durable ACK.
- Apply committed peer updates idempotently.
- Handle runtime-epoch changes and `ResyncRequired`.
- Export peer ID plus snapshot for storage by a future client.
- Encode/decode presence cursors.

Add typed methods for all room connection, execution, cancellation, result discovery, and paging routes.

The SDK does not write replica state to disk. Future Rust, JavaScript, and Swift clients must persist a Loro snapshot together with its peer ID before reusing that peer ID.

## Implementation slices

1. **G0:** Aide router conversion, generated OpenAPI, and parity gates.
2. **G1:** ADR-014 graduation, protocol reset, Loro-backed `sift-doc`, and merge corpus.
3. **G2:** Metadata migration, update log, document actors, durable ACK path, and compaction.
4. **G3:** WebSocket synchronization, chunking, reconnect, replica leases, and SDK replica state machine.
5. **G4:** Separate leased presence, stable cursors, runtime epochs, and lag recovery.
6. **G5:** Room-owned managed connections, provenance changes, policy enforcement, idle cleanup, and autocommit restriction.
7. **G6:** Shared result registry, encrypted spill, independent paging, cancellation, discovery, and activity events.
8. **G7:** Full integration, fault, quota, performance, OpenAPI/SDK parity, and documentation graduation.

Each slice must leave formatting, clippy, and workspace tests green.

## Test matrix

### CRDT unit tests

- Concurrent inserts at the same position converge deterministically.
- Overlapping insert/delete operations converge.
- Unicode and multi-byte UTF-8 edits converge.
- Updates applied out of order, duplicated, or after reconnect converge.
- Missing dependencies are detected.
- Full snapshots merge with arbitrarily old replicas.
- Exact historical frontiers materialize the expected text.
- Stable cursors survive concurrent edits and deletions.
- Extra roots and rich-text marks are rejected.
- Text, update, and history limits are enforced.

### Persistence tests

- Both legacy CRDT labels migrate from raw UTF-8 without loss.
- Invalid legacy bytes abort migration without partial changes.
- Sequence allocation is monotonic per document.
- ACK-visible updates survive server reconstruction.
- Snapshot plus post-snapshot updates rebuild exactly.
- Compaction is transactional and does not delete uncovered updates.
- Duplicate updates create no new sequence or broadcast.
- Document deletion cascades update rows.
- Simulated SQLite failure causes no ACK or broadcast.

### WebSocket tests

- Two online editors converge.
- Two offline editors diverge, reconnect, exchange missing updates, and converge with the server.
- A viewer synchronizes but cannot submit updates.
- A duplicate live replica writer receives `replica_in_use`.
- Membership revocation terminates access.
- Presence expires after lease loss and disappears immediately on clean detach.
- Lag beyond broadcast capacity produces `ResyncRequired`, after which state recovers without reconnecting.
- Runtime restart changes the epoch and triggers document rediscovery.

### Connection and execution tests

- Concurrent idempotent opens produce one driver connection.
- Profile-policy revisions revoke a room connection.
- Exact old document versions execute while newer edits exist.
- Invalid UTF-8 ranges are rejected.
- Transaction APIs and transaction-control SQL are rejected.
- Viewer execution is forbidden.
- Initiator/owner cancellation rules are enforced.
- Room-owner-only connection close is enforced.
- PostgreSQL and SQL Server cancellation preserve their existing driver-isolation guarantees.

### Shared-result tests

- Multiple members independently read identical page sequences.
- A slow reader does not advance or block another reader.
- Memory overflow spills encrypted pages and remains independently pageable.
- Spill plaintext is not visible on disk.
- Tenant retained-byte quota includes memory and spill bytes.
- Membership removal immediately prevents further reads.
- TTL cleanup releases quotas and deletes files.
- Restart expires all result references.
- Viewer page access succeeds while viewer execute/cancel fails.

### Public-contract tests

- Router, generated OpenAPI, and SDK coverage remain in parity.
- Every new request, response, error, and WebSocket message has a schema.
- Every user-visible mutation maps to an `Operation`.
- Audit records contain no SQL, CRDT bytes, result rows, or cursor payloads.
- The protocol crate remains pure serde with no Tokio, filesystem, or network dependency.

## Performance and graduation criteria

- Ten concurrent replicas applying at least 10,000 mixed edits converge byte-for-byte.
- Durable ACK latency and post-commit fanout are benchmarked separately.
- Room WebSocket fanout retains the existing target of p95 under 25 ms at modest room size, excluding SQLite commit time.
- Loro import/export and historical materialization receive Criterion benchmarks at 8 MiB text and representative histories.
- No Loro import, snapshot export, spill I/O, or SQLite work blocks an Axum/Tokio request worker.
- A wedged driver, slow result reader, malformed CRDT payload, or stalled client cannot freeze unrelated rooms.
- `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and cargo-deny pass.
- ADR-014 is graduated into `docs/DECISIONS.md`, Phase G is documented in a dedicated plan, and the build list is checked off only after the complete collaboration matrix passes.
