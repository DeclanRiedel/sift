# PostgreSQL feature map

Work tracking (2026-11-11): **Reference / scoped evidence**. Remaining product work: [current TODO](../TODO.md#postgresql).

Status: **code-traced inventory, reconciled 2026-11-11**. This maps Sift's declared
PostgreSQL surface from engine adapter through API and client to its user entry
point. It does not claim coverage of every PostgreSQL server feature. Product
status lives in the [canonical inventory](ide-parity-and-provider-extensibility.md);
the [provider acceptance record](database-provider-acceptance.md) defines the
tested version, fixtures, and exclusions. A route or SDK method alone is not a
desktop workflow.

## Transport and shared paths

`sift-driver-postgres` implements the locked `Driver` verbs in
[`lib.rs`](../../crates/driver-postgres/src/lib.rs). The built-in provider is
registered in [`registry.rs`](../../crates/server/src/registry.rs); sessions
supervise queries, enforce policy and audit typed `Operation`s in
[`session.rs`](../../crates/server/src/session.rs). HTTP routes are declared in
[`http.rs`](../../crates/server/src/http.rs); the reference client is
[`client-sdk`](../../crates/client-sdk/src/lib.rs). Desktop transport is
[`desktop/app.rs`](../../crates/desktop/src/app.rs), with controls in
[`workspace-ui/shell.rs`](../../crates/workspace-ui/src/shell.rs). These common
links apply to each row unless a narrower implementation is named.

| PostgreSQL surface | Adapter and server path | Client/user entry point | Boundary or remaining gap |
| --- | --- | --- | --- |
| Connect, ping, TLS, socket/TCP, pooling, startup SQL | [`conn.rs`](../../crates/driver-postgres/src/conn.rs), `Driver::open/ping`, profile admission | SDK connection methods; desktop connection editor and health | Acceptance exercised local socket trust, not production certificate validation across platforms. |
| Typed SQL, parameters, batches, paging, cancellation | [`stream.rs`](../../crates/driver-postgres/src/stream.rs), [`decode.rs`](../../crates/driver-postgres/src/decode.rs), `Driver::execute/cancel`, session/cursor supervision | SDK execute/cancel; editor and result grid | Single statements retain native types; simple-query batches return text/NULL metadata. Calendar-month intervals remain native. |
| Native execution progress | [`progress.rs`](../../crates/driver-postgres/src/progress.rs) via `PgExt::observe_progress`, server execution events | Result progress display | Only commands with trustworthy `pg_stat_progress_*` data yield native progress; otherwise generic progress. |
| Transactions and savepoints | `Driver::begin/commit/rollback`, `PgExt::savepoint/rollback_to/release_savepoint`, [`session.rs`](../../crates/server/src/session.rs) | SDK transaction methods; desktop transaction controls | Rollback-to invalidates later savepoints. Database external effects and consumed sequence values are not undone. |
| Schema tree, graph, search, DDL, schema snapshots and migrations | [`schema.rs`](../../crates/driver-postgres/src/schema.rs), [`ddl.rs`](../../crates/server/src/ddl.rs), [`catalog.rs`](../../crates/core/src/catalog.rs), [`migration.rs`](../../crates/server/src/migration.rs) | SDK schema/catalog/DDL/migration methods; explorer, Objects, diagram and designer | Native DDL is separate from structural diff. Partition children/inheritance, foreign-table options, RLS, and rules export natively. Foreign-table reads require ownership and server USAGE. Custom storage and unsupported index states fail explicitly. Standalone ordinary index DDL and bounded index/sequence/policy/partition migrations exist; see the provider gap checklist for exact shapes. |
| SQL intelligence | Server semantic and completion handlers, PostgreSQL dialect/catalog binding | SDK semantic methods; desktop editor diagnostics, completion and refactoring | Runtime-created or dynamic objects cannot be inferred reliably; see [dialect matrix](../SQL_DIALECT_FEATURE_MATRIX.md). |
| Explain, plan comparison and benchmarks | [`plan.rs`](../../crates/server/src/plan.rs), [`benchmark.rs`](../../crates/server/src/benchmark.rs) | SDK explain/plan/benchmark methods; desktop plan and performance views | `EXPLAIN ANALYZE` uses rollback, which cannot undo external function effects or sequence consumption. |
| Result transforms, search, edits, comparison and export | [`result_transform.rs`](../../crates/server/src/result_transform.rs), [`search.rs`](../../crates/server/src/search.rs), [`edit.rs`](../../crates/server/src/edit.rs), [`comparison.rs`](../../crates/server/src/comparison.rs) | SDK search/edit/comparison/export; desktop grid and compare views | Edits need stable keys and conflict checks. Inferred editing of executed SQL accepts only a narrow qualified `SELECT * FROM schema.table` shape in [`result_editing.rs`](../../crates/workspace-ui/src/shell/result_editing.rs). |
| CSV and Parquet import/transfer | `PgExt::copy` in driver, [`csv_import.rs`](../../crates/server/src/csv_import.rs), [`parquet_transfer.rs`](../../crates/server/src/parquet_transfer.rs) | SDK import/transfer; desktop transfer preview, quarantine history, durable CSV resume and cross-engine CSV type mapping | COPY is an internal bounded CSV path; no general COPY export API. CSV v2 recipes support ordered selection, drop, reorder and copies (ADR-130); text/decimal expressions, XLSX/Parquet projections and direct Vim mapping controls are implemented. Broader formats/sources and conversion shapes remain partial. |
| Server settings | [`postgres_settings.rs`](../../crates/server/src/postgres_settings.rs) queries role-visible `pg_settings` through supervised execution | SDK `list_postgres_settings`; desktop Monitor → Settings with paged Vim navigation | Read-only. Each API page is capped at 200 rows; likely credential-bearing values are redacted. PostgreSQL and connection policy decide visibility. |
| Processes, blocking, alerts, terminate | [`process.rs`](../../crates/server/src/process.rs), [`process_alerts.rs`](../../crates/server/src/process_alerts.rs) using `pg_stat_activity` and `pg_blocking_pids` | SDK process/alert methods; desktop Activity and Locks views | Locks view derives blockers from process rows. A bounded dashboard and operator-configured redacted historical deadlock reads are implemented. Broader lock management and remote/durable log collection remain outside the slice. |
| LISTEN/NOTIFY | `PgExt::listen/unlisten`, server WebSocket `Listen` operation; schema cache also listens for `sift_schema_change` | SDK notification stream; desktop background subscription plus explicit single-channel listener with bounded history and stop/restart controls in [`pg_notifications.rs`](../../crates/workspace-ui/src/shell/pg_notifications.rs) | Desktop listener handles one channel at a time. `UNLISTEN` is lifecycle/internal plumbing, not an independent public operation. |
| Explicit-target VACUUM, ANALYZE, REINDEX | [`maintenance.rs`](../../crates/server/src/maintenance.rs), HTTP `/maintenance/postgres`, typed `Operation::PostgresMaintenance` | SDK `postgres_maintenance`; reviewed Vim Monitor maintenance | Source-bound Vim preview/apply exists for explicit targets. No database-wide target, VACUUM FULL or automatic repair. |
| Heap integrity | [`integrity.rs`](../../crates/server/src/integrity.rs), HTTP `/integrity`, typed `Operation::CheckIntegrity` | SDK `check_integrity`; operator/API workflow | Requires already-installed `amcheck`; selected heap only, no recursive partition/index/TOAST check or repair. |
| Dump and restore | [`postgres_backup.rs`](../../crates/server/src/postgres_backup.rs) operator command | `sift-server postgres dump` / `sift-server postgres restore` CLI | Bounded custom-archive workflow; no desktop flow, cluster/PITR recovery or destination compatibility preview. |
| Advisory locks | `PgExt::advisory_lock/unlock` in driver | Driver API and live driver test only | No server `Operation`, HTTP/WS route, SDK method or desktop action. Do not count as a product feature. |

## Administration coverage and remaining work

- [x] Bounded extension/partition management, replication/statistics inspection,
  PostgreSQL roles, RLS policies, ACL/default privileges, ownership and historical
  deadlock inspection have audited API/SDK/Vim workflows.
- [ ] Complete broader extension/partition combinations, credentials, additional
  privilege/grant-chain families, richer migrations and complete recovery workflows.

Use [the provider gap checklist](postgres-provider-gaps.md) for exact native
shapes and [TODO](../TODO.md#postgresql) for the current queue. Ordinary SQL does
not itself establish a managed workflow or native quality certification.

## Evidence and next mapping rule

The [acceptance record](database-provider-acceptance.md) reports 19 live driver
tests and server fixtures for DDL, plans, process control, stream/cancel and
transactions against PostgreSQL 17.10. The mapping above is code inspection;
it is not fresh live-engine acceptance. For each new PostgreSQL feature, add its
adapter/server, typed operation and audit, API/SDK, user entry point, explicit
limits and live or focused evidence here before marking its inventory row done.
