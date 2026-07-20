# DDL — known gaps

Snapshot of what the Phase D DDL generator (`crates/server/src/ddl.rs`)
does *not* cover today, ordered by priority. Each entry names the
missing kind or feature, the smallest concrete task that would close
it, and the reason it's parked at this priority.

The `live-pg`-gated round-trip test in `crates/server/tests/
ddl_round_trip.rs` covers the sound cases (plain tables with columns +
PK/FK/UNIQUE/CHECK + standalone indexes; views; materialized views).
Everything on this list is a real hole; the round-trip is only a proof
of what already works.

## Priority 1 — resolved in the Phase D polish pass

1. `ForeignTable` no longer emits misleading `CREATE TABLE` output. It now
   returns `UnsupportedForEngine` until server/options metadata is modeled.
2. PG and SQL Server deep schema metadata carry column default expressions,
   and table DDL renders them.
3. PG identity and legacy serial columns, plus SQL Server identity columns,
   are rendered explicitly. Serial columns use the pseudo-type instead of
   copying a `nextval(...)` expression that references the source sequence.

## Priority 2 — natural next batch (unimplemented `ObjectKind`s users can already ask for)

4. **`Sequence`.** Both engines have them; today they return
   `UnsupportedForEngine`. **Fix shape:** PG via
   `pg_get_expr(seqrelid → pg_class + pg_sequence)` producing
   `CREATE SEQUENCE ... INCREMENT ... START ... MINVALUE ... MAXVALUE ...`;
   MSSQL via `sys.sequences`. Adds two dispatch cases + one helper.

5. **`Trigger`.** Both engines. **Fix shape:** PG
   `pg_get_triggerdef(oid)`; MSSQL
   `OBJECT_DEFINITION(OBJECT_ID(...))`. Introspection already reports
   triggers as part of a Deep-scoped table snapshot; a standalone
   trigger DDL just needs the routing case.

6. **`Type`.** PG composite / enum / domain types; MSSQL user-defined
   types. **Fix shape:** PG uses `pg_type` + kind byte to dispatch:
   composite → `CREATE TYPE ... AS (col type, ...)` (built from
   `pg_attribute`); enum → `CREATE TYPE ... AS ENUM (...)` (from
   `pg_enum`); domain → `CREATE DOMAIN` (from `pg_type` +
   constraints).

7. **Standalone `Index`.** `ObjectKind` has no `Index` variant, so a
   caller can't ask for "just this index." Today indexes ride along
   with their parent table's DDL. **Fix shape:** add
   `ObjectKind::Index`, teach shallow introspection to enumerate
   indexes as top-level objects (opt-in via `SchemaFilter.kinds`), and
   dispatch a routine that calls `pg_get_indexdef(oid)` on PG /
   `sp_helpindex` + `sys.indexes` on MSSQL. This is a small protocol
   change; schedule it deliberately with a protocol-version note.

## Priority 3 — engine-specific, defer until asked

8. **`Synonym` (MSSQL only).** `CREATE SYNONYM name FOR base_object`
   from `sys.synonyms.base_object_name`. Straightforward but low
   demand.

9. **`Extension` (PG only).** `CREATE EXTENSION name WITH SCHEMA ...`
    from `pg_extension`. Also straightforward; parked because most
    users don't manage extensions from an editor.

10. **Computed / generated columns
    (`GENERATED ALWAYS AS (...) STORED`).** PG 12+ and MSSQL both
    support this. Neither the driver reports the expression nor the
    formatter would render it. Overlaps with Priority 1 #2 (DEFAULT):
    same shape change, same round-trip loss.

11. **Column `COLLATE`.** Loss on round-trip for any column with a
    non-default collation. Same fix shape as DEFAULT/GENERATED.

## Priority 4 — cross-cutting, ADR-worthy

12. **AST-equivalence assertion for the round-trip test.** Today the
    round-trip compares *re-introspected* shape, not the SQL text.
    That's the right primary bar (formatting variations shouldn't
    fail), but a secondary text-equivalence pass through a canonical
    normalizer (e.g. `sqlparser-rs` on both sides) would catch
    formatter drift the introspection can't see. Not urgent.

13. **MSSQL round-trip.** Test file is PG-only. MSSQL needs its own
    fixture harness (dev container is heavier, GO batch handling
    differs, `OBJECT_DEFINITION` returns verbatim engine-formatted
    text that isn't guaranteed idempotent). Separate follow-up.

## What order to tackle

Priority 2 items are independent and can each land as isolated changes.
Priority 3/4 wait for a concrete driver need.
