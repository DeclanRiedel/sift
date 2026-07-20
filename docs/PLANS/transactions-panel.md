# Transactions panel contract

Status: accepted (ADR-026).

## Surface

- `GET /v1/sessions/{session_id}/transactions` returns every transaction the
  server currently owns for the session, ordered by open time.
- Each transaction includes its connection, mode, open time, and ordered
  savepoint list. Savepoints carry a creation time and lifecycle state.
- `POST /v1/sessions/{session_id}/transactions/{tx_id}/preview` accepts the
  transaction's connection and an `action` (`commit` or `rollback`). It is a
  pure server-state query: it does not touch the database.

## Lifecycle rules

- Creating a savepoint appends it to the transaction state. Names must be
  non-empty and unique among active savepoints.
- Rolling back to a savepoint keeps the target active and marks savepoints
  created after it as invalidated. This mirrors both supported engines.
- Releasing a Postgres savepoint marks it released. SQL Server continues to
  reject release because the engine has no matching operation.
- Commit and rollback remove the transaction and all savepoint state only
  after the driver succeeds. On driver failure, the transaction remains open
  so the user can retry or inspect it.

## Preview semantics

Preview reports the action, transaction age, active savepoint count, and the
fact that all savepoints will close. Rollback is flagged as destructive;
commit is not. It deliberately does not claim to know affected rows or
database-side locks because the driver contract exposes neither reliably.

## Audit

Listing, preview, and every lifecycle mutation are `Operation` variants.
Listing and preview are audited even though they are read-only user actions.
