# TODO — Ordered Work Queue

This is the current implementation order. Keep `docs/PLANS/headless-collab-infra.md`
as the architecture plan of record; use this file for execution order.

## Work On First

1. Add role-aware room authorization.
   - Enforce owner/editor/viewer permissions on room member, document, and
     delete routes.
   - Keep tenant membership as the outer guard, but stop treating it as enough
     for every room action.

2. Replace manual OpenAPI assembly.
   - The current JSON literal is large and brittle.
   - Move metadata route schemas into typed request/response definitions or a
     small OpenAPI builder helper.

3. Add metadata route coverage for negative cases.
   - Non-member room access once role-aware room auth exists.
   - Delete/update permissions by role.
   - Expired token and tenant mismatch cases are covered; broaden from there.

4. Record query history from execute paths.
   - The metadata store has history APIs, but HTTP execute does not yet record
     actor/room/profile context.
   - Decide how room/profile context is passed to execute without breaking the
     existing session API.

## Improve Next

5. Harden metadata runtime shape.
   - Current sync SQLite calls are isolated with `spawn_blocking`.
   - Hosted mode should use a metadata actor or connection pool, plus clearer
     backpressure and shutdown behavior.

6. Expand `sift-doc`.
   - Add an apply-operation API.
   - Hide the eventual Loro/Automerge choice behind crate-local adapters.
   - Add document diff/merge tests before any UI consumes it.

7. Add typed client SDK methods for metadata/auth routes.
   - Tenants, rooms, members, documents, profiles, tokens, and history.
   - Keep the lab as a manual workbench, but make SDK coverage the stable API
     contract.

## Work On After That

8. Introduce room runtime.
   - In-memory attachments/presence.
   - Document operation WebSocket class.
   - Room-aware operation audit.

9. Start room-aware result handling.
   - Keep direct session execution working.
   - Add supportable shape for future result fanout without implementing full
     broadcast streams yet.

10. Defer UI until the headless layer is stable.
    - No GPUI crate, web-client decision, OIDC, keypair remote auth,
      voice/video, or follow-mode polish until the above foundation is solid.
