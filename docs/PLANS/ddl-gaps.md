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

## Priority 1 — fix soon (real bugs on shipped surface)

1. **`ForeignTable` currently emits `CREATE TABLE`.** The routing
   drops through the table branch; there's no SERVER / OPTIONS clause.
   **Fix shape:** dedicated `generate_foreign_table_ddl` calling
   `pg_get_foreign_table_ddl` (PG 15+) or hand-composing from
   `pg_foreign_table` + `pg_foreign_server`. Rare in daily use but
   the current output is *misleadingly wrong* — it looks like a table,
   isn't one, and would fail to apply against a live DB.

2. **Column DEFAULT expressions are dropped.** `ColumnMetadata` has no
   field for `default_expr`, so the generator never emits
   `DEFAULT <expr>`. Round-tripping a table with defaults silently
   loses them. **Fix shape:** add `default_expr: Option<String>` to
   `ColumnMetadata`; PG driver fills from
   `pg_get_expr(pg_attribute.atthasdef → pg_attrdef.adbin, atttypid)`;
   MSSQL from `sys.default_constraints`. Formatter appends
   ` DEFAULT <expr>` after the type.

3. **`GENERATED ALWAYS AS IDENTITY` is dropped.** The PG driver
   reports `is_identity: true` in `PgColumnFacets` but the DDL
   formatter ignores facets entirely. **Fix shape:** if facets report
   identity, emit `GENERATED ALWAYS AS IDENTITY` in place of the
   nullability suffix.

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

Do Priority 1 as one bundled pass — the remaining items all sit on
`ColumnMetadata` or table-DDL shape changes and touch the same PG
introspection module. Amortising the protocol churn in one commit is
cheaper than separate passes. Priority 2 items are independent and can
each land as isolated PRs. Priority 3/4 wait for a concrete driver
(a user asks, or a feature above the driver layer needs it).
