# HTTP cancel caller scoping

## Issue

The HTTP cancel endpoint validates cursor/connection ownership only at the driver layer. It does not bind sessions, connections, or cursors to the authenticated principal, so any authenticated caller who can guess a session, connection, and cursor tuple can attempt cancellation.

## Current proof

- `crates/server/src/http.rs` `cancel_query` takes no `HeaderMap` and performs no auth-context or tenant check.
- `SessionStore::open_session` creates sessions without owner metadata.
- Driver cancel checks only that the cursor belongs to the supplied `ConnHandle`.

## Failure mode

Cursor ids and session ids are small monotonic integers. In hosted mode, a caller with any valid token can enumerate recent ids and cancel another user's in-flight query if they discover the tuple.

## Changelist

- Add authenticated owner/tenant metadata to sessions and connections.
- Require `cancel_query` to resolve `AuthContext` and verify principal or room membership before calling `SessionStore::cancel`.
- Make WebSocket cancel inherit the same ownership check.
- Add tests for cross-principal cancel rejection and same-principal cancel success.
