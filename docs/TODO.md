# TODO — Ordered Work Queue

This is the current implementation order. Keep `docs/PLANS/headless-collab-infra.md`
as the architecture plan of record; use this file for execution order.

## Work On First

1. Harden metadata runtime shape.
   - Current sync SQLite calls are isolated with `spawn_blocking`.
   - Hosted mode should use a metadata actor or connection pool, plus clearer
     backpressure and shutdown behavior.

2. Expand `sift-doc`.
   - Add an apply-operation API.
   - Hide the eventual Loro/Automerge choice behind crate-local adapters.
   - Add document diff/merge tests before any UI consumes it.

3. Add typed client SDK methods for metadata/auth routes.
   - Tenants, rooms, members, documents, profiles, tokens, and history.
   - Keep the lab as a manual workbench, but make SDK coverage the stable API
     contract.

4. Introduce room runtime.
   - In-memory attachments/presence.
   - Document operation WebSocket class.
   - Room-aware operation audit.

## Improve Next

5. Start room-aware result handling.
   - Keep direct session execution working.
   - Add supportable shape for future result fanout without implementing full
     broadcast streams yet.

## Work On After That

6. Defer UI until the headless layer is stable.
   - No GPUI crate, web-client decision, OIDC, keypair remote auth,
     voice/video, or follow-mode polish until the above foundation is solid.

## Completed In Phase 0 Polish

- Added role-aware room authorization:
  - tenant membership remains the outer guard;
  - room viewers can read;
  - room editors can write documents and query history context;
  - room owners can manage members and delete rooms.
- Added negative metadata route coverage for non-member, viewer, and editor
  permission failures.
- Scoped metadata API tokens to their issued tenant when `tenant_id` is set.
- Added optional `room_id` and `connection_profile_id` fields to HTTP execute
  requests and record room/profile-attributed query history when supplied.
- Replaced anonymous metadata OpenAPI route payloads with typed schemas for
  metadata/auth request and response bodies.
