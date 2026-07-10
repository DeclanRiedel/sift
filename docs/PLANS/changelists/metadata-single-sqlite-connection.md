# Single SQLite connection bottleneck

## Issue

All metadata reads, writes, and durable operation-audit writes share one `rusqlite::Connection` behind `std::sync::Mutex`.

## Current proof

- `MetadataStore` stores `conn: Arc<Mutex<Connection>>`.
- `record_operation_audit`, token verification, room reads, saved-query search, and connection-profile operations all lock the same connection.
- WAL mode is enabled, but the single connection prevents concurrent readers.

## Failure mode

A long metadata read or token verification write blocks every other metadata operation, including audit writes. The `metadata_blocking` semaphore permits concurrency, but most tasks park behind the same mutex.

## Changelist

- Move to a small SQLite connection pool such as `r2d2_sqlite` or `deadpool-sqlite`.
- Use read-only pooled connections for list/search paths and write connections for mutations.
- Give the audit writer its own connection or fold critical audit rows into mutation transactions.
- Replace `std::sync::Mutex` with a design that does not park many blocking tasks behind one lock.
