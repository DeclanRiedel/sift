# Process list and kill contract

Status: accepted (ADR-027).

## Surface

- `GET /v1/sessions/{session_id}/connections/{connection_id}/processes`
  returns a normalized, bounded snapshot of database activity visible to the
  connected database principal.
- `POST /v1/sessions/{session_id}/connections/{connection_id}/processes/kill`
  accepts a numeric database process id and requests termination.
- Both calls are audited `Operation` variants. SQL text and credentials are
  never included in the operation payload.

## Normalized model

The common process record contains engine, process id, user, database, state,
current statement text, start time, wait detail, and blocking process ids.
Fields the engine does not expose are optional. Results are capped at 500 rows
and ordered with active work first.

## Engine mapping

- Postgres reads `pg_stat_activity`, excludes the connection executing the
  catalog query, and terminates with `pg_terminate_backend($1)`.
- SQL Server reads `sys.dm_exec_requests` joined to `sys.dm_exec_sessions` and
  `sys.dm_exec_sql_text`, excludes the current SPID, and terminates using a
  validated numeric `KILL <spid>` statement.

The server composes these calls through the existing bounded execute path. No
driver-trait method is added. Database permissions remain authoritative: an
insufficiently privileged user receives the engine error.

## Guardrails

Only positive process ids are accepted. The list query excludes its own
backend process, and the kill statement refuses the current backend process.
The endpoint does not pretend to provide operating-system process control.
