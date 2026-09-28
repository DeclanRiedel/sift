# SQL Server provider gap checklist

Status: active Linux backlog. SQL Server passed the bounded ADR-055 daily-driver
scope. This list separates deliberate exclusions from shipped behavior; the
[acceptance matrix](database-provider-acceptance.md), [DDL gaps](ddl-gaps.md),
and [product inventory](ide-parity-and-provider-extensibility.md) define the
current public support boundary.

## Values, plans, and native definitions

- [x] Decode `money` and `smallmoney` without a floating-point round trip;
      exact positive/negative boundaries, fractions, and NULLs pass through
      live TDS responses. The local Tiberius 0.12.3 patch retains signed
      ten-thousandths as `Numeric(scale=4)` before the driver formats
      `Value::Decimal`; revisit the patch on upstream upgrades.
- [ ] Decide a lossless representation and bind contract for supported
      `sql_variant`/UDT families; retain explicit unsupported errors for others.
- [ ] Add actual execution plans with scoped permissions, supervised execution,
      cancellation, result/plan bounds, and a separate measured-plan UI.
- [~] Export supported temporal, memory, replication, and policy table shapes,
      or keep each explicit rejection until its round trip is proven. Basic
      system-versioned tables now round-trip; other shapes remain excluded.
- [~] Preserve advanced storage, nonordinary indexes, disabled/untrusted
      constraints, CLR/table types, and bound defaults/rules where supported.
      Sparse nullable columns and uniform ROW/PAGE compression on ordinary
      rowstore tables and indexes now round-trip; other listed shapes remain.
- [x] Export synonym DDL and dependency references. Native `CREATE SYNONYM`
      round-trips in the live SQL Server fixture, and graph nodes retain the
      catalog base-object path as an unresolved dependency when no target edge
      is proven. Definition export requires `VIEW DEFINITION`.
- [ ] Extend schema diff/migration to represent new native shapes without
      silently reducing them to ordinary tables or indexes.

## Workbench and administration

- [x] Open CSV quarantine reports from the current import result, with
      authorized retrieval and rejected source-row details.
- [x] Configure and retry durable CSV import from the desktop using a target
      checkpoint table and stable run UUID.
- [x] Reopen retained CSV quarantine reports through a workspace history view.
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
