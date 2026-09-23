# PostgreSQL provider gap checklist

Status: active Linux backlog. PostgreSQL passed the bounded ADR-055 daily-driver
scope; this checklist tracks work beyond that scope. A checked item here requires
working behavior and evidence, not merely a visible control. The
[acceptance matrix](database-provider-acceptance.md), [DDL gaps](ddl-gaps.md),
and [product inventory](ide-parity-and-provider-extensibility.md) remain the
sources for current support claims.

## Native definitions and migrations

- [ ] Export partition children and inheritance without losing partition bounds,
      attachment, indexes, and dependency order; reject unsupported shapes.
- [ ] Export foreign-table server/options metadata with permission-aware reads.
- [ ] Represent row-level security policies and rules in native DDL, with
      explicit ownership and grant boundaries.
- [ ] Cover custom table storage/options and currently rejected index states.
- [ ] Export extension definitions and dependencies without treating extension
      member objects as independent creations.
- [ ] Address standalone indexes and sequence ownership. Any new public object
      kind or signature follows ADR-017 and a protocol bump.
- [ ] Extend catalog diff/migration to preserve supported rich index, partition,
      policy, and ownership shapes; reject loss before preview/apply.

## Workbench and administration

- [x] Open CSV quarantine reports from the current import result, with
      authorized retrieval and rejected source-row details.
- [x] Configure and retry durable CSV import from the desktop using a target
      checkpoint table and stable run UUID.
- [x] Reopen retained CSV quarantine reports through a workspace history view.
- [x] Listen to PostgreSQL notifications in the desktop with scoped stream
      cleanup, bounded history, and keyboard navigation.
- [ ] Finish the PostgreSQL plans, process-control, and bulk-import
      desktop workflows currently marked partial in the product inventory.
- [ ] Add extension and partition inspection/management UI through audited,
      previewable operations.
- [ ] Add replication and statistics inspection UI with bounded reads and
      explicit permission errors.
- [ ] Add a read-only server-settings browser; design writes separately with
      scope, policy, confirmation, and audit.
- [ ] Add database users/roles, grants, ownership, and RLS editors with
      capability checks and reviewable changes.
- [ ] Connect existing dump/restore, maintenance, and integrity-check backends
      to complete Linux desktop workflows where operator policy permits.

## Acceptance

- [ ] Live round trips for each added native DDL shape, including restricted
      roles, cross-object dependencies, and explicit unsupported cases.
- [ ] Linux TLS certificate verification and larger catalog/result fixtures;
      record tested versions and performance limits rather than claiming all
      PostgreSQL deployments.
- [ ] `cargo fmt`, strict workspace Clippy, and workspace tests pass after each
      implementation slice; opt-in PostgreSQL suites pass for engine changes.
