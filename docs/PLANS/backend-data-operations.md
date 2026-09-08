# Backend data operations

Scope: Parquet transfer, state-backup automation, observability, and PostgreSQL
dump/restore. These are API/operator milestones; no visual acceptance required.

## Contracts before implementation

- Parquet uses the existing authorized/audited export and transfer surfaces.
  Preserve column order, nulls, integer widths, floating-point, boolean and binary
  values; preserve other values as lossless text rather than guess precision.
  Reject unsupported imports and multiple result sets explicitly. Bound encoded
  and decoded sizes. Empty exports retain their declared schema.
- State backup automation must preserve ADR-039's exclusive maintenance lock.
  It must never stop a serving process implicitly. Failed backups/uploads must
  not prune recovery copies. Retention only touches archives owned by the policy.
  Remote destinations receive encrypted archives, never archive-key bytes.
- Observability is opt-in where it exports externally; telemetry must not include
  SQL, credentials, query results, raw URLs, or unbounded labels.
- PostgreSQL tools run as bounded, cancellable subprocesses without a shell.
  Passwords must not appear in arguments or diagnostics. Restore defaults to
  validation; actual database mutation requires explicit apply and target.

Each implementation milestone will document its supported scope and automated
acceptance, then be committed separately. Final acceptance includes workspace
formatting, strict Clippy, and workspace tests.

## Completed milestones

- `0098c9c`: typed Parquet transfers and administrator HTTP observability.
- `89c56e5`: offline backup policies, conditional encrypted uploads and
  PostgreSQL operator recovery.
- Final integration: metrics SDK coverage, bounded OTLP response handling and
  normalized Parquet target authorization.

Acceptance on 2026-09-08: workspace tests passed, including all 244 server-library
tests, 76 API integration tests and 454 workspace-UI unit tests. The isolated
PostgreSQL 17.10 recovery test passed separately, including dry-run non-mutation,
restore rollback, no-overwrite publication, audit entry points and process timeout.
No existing database or external object-store account was used. Remote PUT and
OTLP delivery were tested against local HTTP fixtures; live cloud credentials,
remote retention and production restore compatibility are operator concerns.
Final `cargo fmt --all -- --check` and
`cargo clippy --workspace --all-targets -- -D warnings` also passed.

See [backend operations](../BACKEND-OPERATIONS.md) for usage and explicit limits.
