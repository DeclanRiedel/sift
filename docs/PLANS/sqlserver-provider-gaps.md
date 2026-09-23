# SQL Server provider gap checklist

Status: active Linux backlog. SQL Server passed the bounded ADR-055 daily-driver
scope. This list separates deliberate exclusions from shipped behavior; the
[acceptance matrix](database-provider-acceptance.md), [DDL gaps](ddl-gaps.md),
and [product inventory](ide-parity-and-provider-extensibility.md) define the
current public support boundary.

## Values, plans, and native definitions

- [ ] Decode `money` and `smallmoney` without a floating-point round trip;
      test exact boundary and fractional values through live TDS responses.
- [ ] Decide a lossless representation and bind contract for supported
      `sql_variant`/UDT families; retain explicit unsupported errors for others.
- [ ] Add actual execution plans with scoped permissions, supervised execution,
      cancellation, result/plan bounds, and a separate measured-plan UI.
- [ ] Export supported temporal, memory, replication, and policy table shapes,
      or keep each explicit rejection until its round trip is proven.
- [ ] Preserve advanced storage, nonordinary indexes, disabled/untrusted
      constraints, CLR/table types, and bound defaults/rules where supported.
- [ ] Export synonym DDL and dependency references.
- [ ] Extend schema diff/migration to represent new native shapes without
      silently reducing them to ordinary tables or indexes.

## Workbench and administration

- [x] Open CSV quarantine reports from the current import result, with
      authorized retrieval and rejected source-row details.
- [x] Configure and retry durable CSV import from the desktop using a target
      checkpoint table and stable run UUID.
- [ ] Reopen retained transfer artifacts through a durable history view.
- [ ] Finish plan, process-control, and bulk-import desktop workflows currently
      marked partial in the product inventory; retain SQL Server's documented
      abort-and-discard cancellation and savepoint limits.
- [ ] Add read-only Query Store inspection before designing any plan-forcing
      action.
- [ ] Add SQL Server Agent and server-settings browsers with bounded reads and
      permission-aware states.
- [ ] Add login, user, role, permission, grant, and ownership editors with
      audited preview/apply paths.
- [ ] Connect existing copy-only backup/new-name restore and integrity-check
      APIs to complete Linux desktop workflows.

## Acceptance

- [ ] Live SQL Server round trips for every added type, DDL shape, plan, and
      administration operation; exercise restricted principals and refusal.
- [ ] Linux certificate verification and representative larger catalog/result
      fixtures; record tested SQL Server versions and limits.
- [ ] `cargo fmt`, strict workspace Clippy, and workspace tests pass after each
      implementation slice; opt-in SQL Server suites pass for engine changes.
