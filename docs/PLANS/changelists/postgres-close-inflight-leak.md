# Postgres close during in-flight query

## Issue

Closing a Postgres connection while a query task is in flight removes driver bookkeeping but does not abort the spawned query task. When the task finishes, it can restore the connection under a handle that was already closed.

## Current proof

- `crates/driver-postgres/src/stream.rs` spawns `run_query` and drops the `JoinHandle`.
- `crates/driver-postgres/src/conn.rs` `remove_conn` removes the conn slot and cursor entries but cannot abort the task.
- `stream.rs` `finish` calls `inner.restore(conn_id, slot_kind, conn)` unconditionally.

## Failure mode

`execute -> close_connection` during streaming can leak a pooled connection back into `conns` under an orphaned conn id. Repeating this can exhaust pool capacity and retain stale backend sessions.

## Changelist

- Store the query `JoinHandle` in the cursor registry value alongside conn id and cancel token.
- Abort matching tasks in `remove_conn`.
- Make `finish` restore only if the connection is still registered as taken for that cursor generation.
- Add a driver isolation test for close-mid-stream followed by map/pool state checks.
