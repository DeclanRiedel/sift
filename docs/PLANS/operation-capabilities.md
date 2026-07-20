# Contextual operation capabilities

Status: accepted (ADR-028).

## Surface

`GET /v1/operations/available` accepts optional `session`, `connection`, and
`transaction` query parameters and returns every typed `OperationKind` with:

- `available`: whether the server can dispatch it in this context;
- `reason`: a stable, human-readable explanation when unavailable;
- `destructive`: whether clients should require deliberate confirmation;
- `engine`: the engine derived from the selected connection, when present.

`GET /v1/operations` remains the existing replayable operation log. Reusing
that path with a different response shape would break current SDK consumers.

## Evaluation

The server derives capability facts from authoritative session state. Clients
do not submit an engine or claim that a transaction exists.

- Session operations require a valid session where applicable.
- Connection operations require a connection owned by that session.
- Begin requires no transaction on the selected connection.
- Commit, rollback, savepoint, and execute-in-transaction require the selected
  active transaction.
- Engine-specific operations report unavailable on the wrong engine (for
  example Postgres savepoint release and SQL Server bulk insert).
- Room and metadata capabilities are reported as requiring their dedicated
  resource context; authorization remains enforced at dispatch time.

The response is advisory for presentation and is never an authorization
bypass. Every operation route repeats its normal state and permission checks.

## Contract growth

`OperationKind` is a payload-free mirror of `Operation`. Adding a user-visible
`Operation` requires adding its kind and evaluator row in the same protocol
change; an exhaustiveness test enforces the mapping.
