# DDL — supported scope and remaining gaps

Reconciled 2026-09-06. See [provider acceptance](database-provider-acceptance.md)
for executed evidence and [graduation](postgres-sqlserver-graduation.md) for gates.

## Implemented and live-tested

- PostgreSQL tables and partition roots use native catalog definitions for exact
  type modifiers, defaults, collations, identity options, generated expressions,
  constraints, index expressions/order/includes/predicates, and trigger states.
  Owned serial sequences retain configuration and ownership in table export.
- SQL Server ordinary tables preserve modifiers, collations, identity seed and
  increment, computed/persisted columns, constraints, ordinary clustered and
  nonclustered indexes, includes, filters, options, and trigger states.
- Both engines export configured sequences and standalone triggers. PostgreSQL
  exports enum/composite/domain types; SQL Server exports alias types.
- Existing views, PostgreSQL materialized views, and routines remain supported.
- Native round trips test both engines, restricted permissions and explicit
  unsupported shapes. Rich column shapes are fenced from structural migrations
  whose catalog model cannot preserve them.

## Deliberate exclusions

- Standalone index addressing: indexes are exported with tables; adding an
  ObjectKind requires a public protocol change.
- PostgreSQL foreign tables, multiple inheritance, custom
  storage/options and unsupported index state return explicit errors. Simple
  partition children (including local indexes, constraints, and triggers),
  single-parent inheritance, table RLS policies, and rewrite rules export
  natively. Structural migrations fence these shapes.
- SQL Server advanced temporal variants, memory/replication/policy tables,
  most advanced storage, nonordinary indexes, untrusted/disabled constraints,
  CLR/table types and bound defaults/rules require separate support. Basic
  system-versioned tables, nullable sparse columns, and uniform ROW/PAGE
  compression on ordinary rowstore tables and indexes round-trip; structural
  migrations fence these shapes.
- SQL Server synonyms and PostgreSQL extensions remain unimplemented.
- Object grants/owners, live sequence counters, statistics, dependency-recursive
  export and full database dumps are outside object DDL. Standalone PostgreSQL
  sequence export does not reconstruct ownership.
- Native DDL fidelity does not imply arbitrary graph/diff/migration fidelity.
  Rich index comparison and migration metadata remain future work.
- Optional AST-equivalence checks remain deferred; current fixtures compare
  regenerated native definitions and assert non-default properties.

## Migration safety for inherited parents

An ordinary PostgreSQL table may be the parent of an inheritance child that is
outside the requested schema scope. The portable catalog projection cannot
represent that dependency. Fingerprint direct descendants on the parent and
fence structural migration of the parent, even when the child is absent from the
snapshot. Verify this with a parent-only live catalog request and a migration
preview refusal. This is a safety boundary; authoring a lossless inheritance
migration remains separate work.
