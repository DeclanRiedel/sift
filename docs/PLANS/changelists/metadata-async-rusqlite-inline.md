# Metadata async methods run SQLite inline

## Issue

Several `async fn` metadata methods perform synchronous rusqlite work on the async caller before or after awaiting secret-store calls. This leaks blocking SQLite work back onto tokio worker threads.

## Current proof

- `crates/metadata/src/lib.rs` has async methods `upsert_connection_profile`, `delete_connection_profile`, `set_per_user_credential`, and `resolve_connection_spec`.
- Those methods lock `Arc<Mutex<rusqlite::Connection>>` and run transactions/prepared statements directly.
- `crates/server/src/http.rs` calls `resolve_connection_spec(...).await` directly in `open_connection_from_profile`, outside `metadata_blocking`.

## Failure mode

Concurrent connection-profile writes or profile-based opens can stall runtime workers, delaying WebSocket pumps, query streams, and unrelated request futures.

## Changelist

- Split secret I/O from SQLite I/O so every rusqlite section can run inside `metadata_blocking`.
- Prefer synchronous metadata methods plus a secret-store abstraction that is either synchronous or explicitly wrapped by the caller.
- Update handlers to avoid direct metadata `.await` calls that contain SQLite work.
- Add a lint-style test or code search assertion for async metadata methods that lock `conn`.
