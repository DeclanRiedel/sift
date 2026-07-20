# Design — Execution plans (Phase D)

> Status: **design, not yet implemented.** Phase D feature: capture a query's
> execution plan as a typed, engine-neutral `PlanNode` tree. Serves README
> **Goal #5** (a typed, public plan a 3rd-party UI can render) and power-user
> performance debugging (the companion to schema/data search — find the slow
> query, then explain it).
>
> Composes over `Driver::execute` — no new `Driver` method, ADR-017 preserved.
> Implies **ADR-025 (candidate): execution-plan model** (the `PlanNode`
> normalization, the XML dependency, and the ANALYZE-safety rule).

## Decisions locked (this session)

1. **Both engines, fully typed.** PG `EXPLAIN (FORMAT JSON)` parses via
   `serde_json` (already a dep). MSSQL showplan is **XML**, so we add an XML
   parser dependency and normalize both into one `PlanNode` tree.
2. **Estimate + ANALYZE, ANALYZE safe-wrapped.** `analyze=false` is a plain
   estimate (no execution). `analyze=true` returns real runtime counters; for
   any non-SELECT statement it runs inside a transaction that **always rolls
   back**, so DML side effects are never committed.
3. **Common core + `extra` map + raw.** `PlanNode` carries the fields both
   engines expose as typed columns, an `extra` map for engine-specific
   attributes, and the response carries the untouched raw plan.

## Where it lives

- **Protocol** → `crates/protocol/src/plan.rs` (pure serde): `PlanNode`,
  `ExplainRequest`, `ExplainResponse`. `Operation::Explain` variant.
- **Server** → `crates/server/src/plan.rs`: engine capture (build the EXPLAIN
  SQL, run through `SessionStore::execute_http`), PG-JSON and MSSQL-XML parsers
  → `PlanNode`, and the ANALYZE tx-rollback wrapper.
- **Dependency**: add **`roxmltree`** (read-only XML DOM; MIT/Apache-2.0,
  minimal transitive deps — clears `cargo deny`) to the server crate for the
  MSSQL showplan parse. (`quick-xml` is the streaming alternative; `roxmltree`'s
  tree API is the better fit for a small, whole-document showplan.)

## Protocol shape

```text
PlanNode {
  op: String,                    // "Seq Scan", "Hash Join" / MSSQL "Clustered Index Scan"
  relation: Option<String>,      // target table / index / object, when the node has one
  est_rows: Option<f64>,         // estimated output rows
  est_cost: Option<f64>,         // PG total cost / MSSQL EstimatedTotalSubtreeCost
  actual_rows: Option<f64>,      // ANALYZE only
  actual_ms: Option<f64>,        // ANALYZE only (actual total time)
  extra: Map<String, JsonValue>, // engine-specific attrs (join type, index cond, buffers, …)
  children: Vec<PlanNode>,
}

ExplainRequest {
  connection: ConnectionId,
  sql: String,
  params: Vec<Value>,            // bound through the normal execute path
  analyze: bool,                 // false = estimate only
}

ExplainResponse {
  engine: Engine,
  analyzed: bool,
  root: PlanNode,
  raw: String,                   // untouched engine plan (JSON for PG, XML for MSSQL)
  warnings: Vec<DriverWarning>,
}
```

`extra` is the escape hatch that keeps the typed core small while losing
nothing — clients that want PG `Filter` / `Index Cond` / `Buffers` or MSSQL
`LogicalOp` / `Predicate` read them there; the untyped `raw` is the final
fallback and the "public plan" a third party can fully parse.

## Capture per engine (composition, no new Driver method)

**Postgres** — one statement, row-producing, parsed from a single JSON cell:
```
EXPLAIN (FORMAT JSON [, ANALYZE true, BUFFERS true, VERBOSE true]) <sql>
```
Returns one row / one column: a JSON array whose element has a `Plan` object.
`serde_json` → walk `Plan` recursively (`Node Type`, `Relation Name`,
`Plan Rows`, `Total Cost`, `Actual Rows`, `Actual Total Time`, `Plans[]`),
mapping the well-known keys to typed fields and the rest into `extra`.

**SQL Server** — two session settings, then the query:
- estimate: `SET SHOWPLAN_XML ON` → returns the plan XML **without executing**.
- analyze: `SET STATISTICS XML ON` → **executes** and returns the actual-plan
  XML with `RunTimeInformation`.

Parse the XML with `roxmltree`: recurse `RelOp` elements
(`@PhysicalOp` → `op`, `@EstimateRows` → `est_rows`,
`@EstimatedTotalSubtreeCost` → `est_cost`, the `<Object>` child → `relation`,
`RunTimeCountersPerThread` → `actual_rows`/`actual_ms`), nested `RelOp`s →
`children`, remaining attributes → `extra`.

Identifiers are never interpolated — `params` flow through `execute_http`'s
normal bind path, so `EXPLAIN … WHERE id = $1` is bound, not string-built.

## ANALYZE safety (the ADR-025 rule)

`analyze=true` executes the statement. Classify the leading keyword (reuse the
same SELECT/WITH/VALUES vs DML heuristic the drivers already use for
row-vs-DML routing):

- **read (SELECT/WITH/VALUES/SHOW/TABLE)** → run the EXPLAIN ANALYZE directly.
- **write (INSERT/UPDATE/DELETE/MERGE/…)** → run inside a transaction and
  **always ROLLBACK**, using the existing `begin`/`execute`/`rollback` path:
  ```
  BEGIN;  EXPLAIN (ANALYZE true, FORMAT JSON) <dml>;  ROLLBACK;   -- PG
  BEGIN TRAN; SET STATISTICS XML ON; <dml>; SET STATISTICS XML OFF; ROLLBACK;  -- MSSQL
  ```
  The plan (with real row counts) is captured; the mutation is discarded. The
  rollback runs even on error (drop-guard), mirroring `apply_edits`.

`analyze=false` never executes, so it's always safe regardless of statement
kind.

## Route

```
POST /v1/sessions/:id/connections/:conn_id/explain   → ExplainResponse
```
Body `{ sql, params?, analyze? }`. Audited as `Operation::Explain` (SQL
fingerprinted, per the existing redaction contract). Sync HTTP — a plan is
small and bounded.

## Test plan

- **Protocol/parse (unit):** a canned PG `EXPLAIN FORMAT JSON` sample → asserts
  the `PlanNode` tree (op/relation/rows/cost/children + `extra`); a canned MSSQL
  showplan XML sample → same. Malformed input → a clean `DriverError`, not a
  panic.
- **ANALYZE safety (server, MockDriver):** an `analyze=true` on a DELETE issues
  begin→explain→rollback (never commit); assert the rollback ran; assert
  `analyze=false` issues no transaction.
- **Live PG (`live-pg`):** estimate + ANALYZE on a SELECT and on an INSERT
  (row count unchanged afterward — proves rollback); parameterized EXPLAIN.
- **Live MSSQL (`live-mssql`):** SHOWPLAN_XML estimate + STATISTICS_XML analyze;
  bracket-quoted objects; DML rolled back.

## Open questions (resolve during implementation)

- **Multi-statement / batch plans.** PG returns one plan per statement; MSSQL
  showplan can carry multiple `StmtSimple`s. v1: take the first statement's
  plan and warn if more are present (document the single-statement contract).
- **Plan cost units are not comparable across engines** (PG arbitrary cost
  units vs MSSQL estimated cost). Keep them typed but document that `est_cost`
  is engine-relative, not a cross-engine metric.
- **`extra` key naming** — pass engine-native keys through verbatim (PG
  `"Index Cond"`, MSSQL `"LogicalOp"`) rather than inventing a unified
  vocabulary; the typed core is the only normalized surface.
- **Size cap** on `raw` for a pathological plan (very large deep plans) — reuse
  a byte ceiling and truncate `raw` with a warning if exceeded.
