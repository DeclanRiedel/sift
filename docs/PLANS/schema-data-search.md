# Design — Schema & data search (Phase D)

> Status: **design, not yet implemented.** Phase D feature, chosen for its
> weight on README **Goal #4 (Zed-class snappiness)**: instant search over a
> large database is the feel differentiator vs Navicat/DataGrip. Composes over
> existing primitives (SchemaCache, cursor registry, execute path) — no new
> `Driver` method, so the ADR-017 trait lock is preserved.
>
> Implies **ADR-024 (candidate): search architecture** — in-memory progressive
> schema index + bounded live data-search fan-out.

## Decisions locked (this session)

1. **Scope = active connection only.** Search targets the one connection the
   user is on. Global cross-connection search is deliberately deferred; it
   layers on later as a fan-out over this same per-connection primitive.
2. **Data search = live bounded fan-out.** Row contents are not indexed;
   searched on demand with hard bounds + cancel. Engine-native FTS is an
   **optional add-on** (phase 2), not the primary path.
3. **Matching = fuzzy subsequence** (command-palette feel, e.g. `usml` →
   `user_email`), a new matcher — diverges from the completion ranker's tiered
   exact/prefix scoring on purpose.

## Two surfaces, two performance profiles

| | Schema search | Data search |
|---|---|---|
| Target | object + column **names** | row **contents** |
| Source | in-memory index (progressive) | live DB queries |
| Latency goal | **< 1 ms**, per keystroke | bounded, streamed, cancellable |
| Transport | sync HTTP (small ranked list) | streamed (WS / cursor registry) |
| Cost model | O(index), no DB round-trip | O(scope) DB work, hard-capped |

---

## Schema search — the snappiness path

### Reuse what exists

`SchemaCache` already stores, per spec, a `CachedSchema { snapshot, dictionary }`
where `Dictionary` (from `sift-completion`) is a denormalized index of every
object (`ObjectEntry`: catalog/schema/name/**name_lower**/kind/columns) with
`by_name` / `objects_by_name` lookups. It is already invalidated on DDL (PG
LISTEN `sift_schema_change`, MSSQL `modify_date` poll) and TTL-bounded. This is
the "progressive post-paint index" the README calls for — most of it is built.

**Gap:** `Dictionary.objects[].columns` is only populated for objects fetched
with `SchemaDepth::Deep`. A whole-DB **column** search needs every column, and
per-object Deep fetches would be N round-trips. So we add a **SearchIndex**.

### SearchIndex (new, per spec, `crates/server/src/search.rs`)

- A flat, search-optimized structure sitting beside `CachedSchema`:
  ```
  SearchIndex {
    entries: Vec<SearchEntry>,     // objects + columns, one row each
  }
  SearchEntry {
    kind: Object(ObjectKind) | Column,
    path: ObjectPath,              // schema-qualified
    display: String,               // "public.users" or "public.users.email"
    haystack_lower: Box<str>,      // precomputed once — the fuzzy target
    type_display: Option<String>,  // for columns
  }
  ```
- **Objects** come from the already-cached shallow snapshot (free).
- **Columns** come from **one bulk catalog query** run in the background
  (not N per-object Deep calls):
  - PG: `SELECT table_schema, table_name, column_name, data_type FROM
    information_schema.columns WHERE table_schema = ANY($1)` (or `pg_attribute`
    join for fidelity).
  - MSSQL: `sys.columns` joined to `sys.objects` + `sys.schemas`.
- **Progressive build (post-paint):** on connection open, spawn a background
  task that runs the bulk column query and builds the index. First schema
  search before it finishes falls back to **objects-only** (already cached) and
  reports `index_state: building` so the GUI can show a subtle "indexing…"
  hint. No blocking, no wait.
- **Invalidation:** the SearchIndex shares the SchemaCache invalidator for its
  spec — a DDL NOTIFY / `modify_date` change drops the index and re-triggers a
  background rebuild. Same TTL ceiling.

### Fuzzy matcher (new, `crates/completion/src/fuzzy.rs`)

Lives in `sift-completion` (pure, testable, benchmarkable; completion may adopt
it later). Subsequence match with a tightness score:

- **Prefilter:** a candidate matches only if every query char appears in order
  in `haystack_lower` (cheap left-to-right scan; rejects the vast majority
  before scoring). Over precomputed lowercase strings this is microseconds each.
- **Score:** reward contiguous runs, matches at word boundaries (`_`, `.`,
  camelCase), and matches near the start; penalize gaps and length. Stable sort
  by (score desc, display asc).
- **Bounds:** stop scoring after a configurable candidate cap; return top-`N`
  (default 50). No allocation in the reject path (borrow `haystack_lower`).

Performance target: **p99 < 1 ms** for a 3-char query over a 100k-entry index
(10k tables × ~10 cols). Enforced by criterion benchmarks in CI with a
regression budget — this also closes the "no completion benchmarks" gap flagged
in `quality-pass-findings-v2.md`.

### Protocol + route

```
SchemaSearchRequest  { query: String, kinds: Option<Vec<ObjectKind>>, limit: Option<u32> }
SchemaSearchResponse { hits: Vec<SearchHit>, index_state: Ready | Building }
SearchHit            { kind, path: ObjectPath, display: String, score: i32,
                       type_display: Option<String>, match_ranges: Vec<(u32,u32)> }
```
`match_ranges` lets the client bold the matched chars. Route:
`POST /v1/sessions/:id/connections/:conn_id/search/schema` (sync HTTP; audited
as `Operation::SearchSchema`, query fingerprinted).

---

## Data search — bounded live fan-out

Searches row contents across a scope with hard safety bounds. Reuses the
`execute_stream` + cursor-registry path so isolation, timeout, cancel, and
per-session cursor caps (ADR-011/013) all apply for free.

### Request

```
DataSearchRequest {
  scope: DataSearchScope,     // Table(ObjectPath) | Schema(name) | Tables(Vec<ObjectPath>)
  query: String,
  per_table_limit: u32,       // default 100, hard max from config
  max_tables: u32,            // default 50, hard max from config
  columns: Option<Vec<String>>,   // restrict to named columns; default = all text-ish
}
```

### Generation (composition, parameterized)

- Resolve the scope's tables from the SchemaIndex/snapshot (no live catalog hit).
- For each table, pick **text-ish** columns (TypeCategory Text; optionally
  numeric/uuid cast-to-text when `query` is short) — skip blobs, skip
  non-castable engine types.
- Emit one **parameterized** statement per table:
  - PG: `SELECT <pk-or-rowid>, <cols> FROM t WHERE col1 ILIKE $1 OR col2 ILIKE $1 … LIMIT $2`
  - MSSQL: `SELECT TOP (@limit) … WHERE col1 LIKE @q OR …` (collation-driven CI).
  - `query` is bound as `%<escaped>%` — never string-interpolated. `%`/`_`
    in the user's query are escaped.
- Identifiers quoted via the drivers' existing quoting helpers.

### Execution & bounds (the performance contract)

- Tables run through a **bounded fan-out** (concurrency cap, e.g. 8) — not all
  at once (protects the pool), not serially (latency).
- Every statement is a normal streamed cursor: **per-table `LIMIT`**, the
  session **row cap + total-bytes cap** (`config.limits`), the **request
  timeout**, and a **cancel token** on client disconnect.
- Each table contributes at most `per_table_limit` rows; total capped by
  `max_tables`. On hitting a cap, the response marks `truncated: true` and names
  what was dropped (no silent truncation — matches the AGENTS.md/plan rule).
- Results stream back tagged by source table:
  `DataSearchHit { table: ObjectPath, row_key: Option<RowKey>, row: Row,
   matched_columns: Vec<String> }`. `row_key` (reusing the inline-edit identity
  logic) lets a hit deep-link to an editable row.

Route: `POST /v1/sessions/:id/connections/:conn_id/search/data` — streams over
the WS session surface (or HTTP chunked), same shape as query streaming; audited
as `Operation::SearchData`.

### Engine FTS — optional add-on (phase 2)

When a table has a native FTS index (PG: a `tsvector` GIN/GiST index or a
generated `tsvector` column; MSSQL: a FULLTEXT catalog on the table), the
generator may emit `@@ plainto_tsquery` / `CONTAINS(...)` instead of `ILIKE`
for that table, falling back to `ILIKE` otherwise. Detection is a catalog probe
cached with the SearchIndex. Deferred behind a `data_search.use_native_fts`
config flag; the LIKE path is the guaranteed baseline.

---

## Where the pieces live

- `crates/completion/src/fuzzy.rs` — pure fuzzy matcher + criterion benches.
- `crates/server/src/search.rs` — SearchIndex build/invalidation, schema-search
  handler, data-search generator + bounded fan-out runner.
- `crates/protocol/src/search.rs` — request/response/hit types (pure serde).
- `Operation::SearchSchema` / `SearchData` — additive audit variants
  (protocol-version bump per ADR-016).

## Performance summary (README Goal #4 alignment)

- Schema search never touches the DB on the hot path — pure in-memory over a
  progressively-built, DDL-invalidated index. Target p99 < 1 ms, CI-enforced.
- Column index built by **one** background catalog query, not N Deep fetches;
  post-paint, non-blocking, with a graceful objects-only fallback while building.
- Data search is the only DB-touching path and is bounded on every axis
  (tables, rows, bytes, time, concurrency) with cancel — it cannot wedge the
  server or exhaust the pool.
- Everything reuses warm primitives (SchemaCache, cursor registry, execute
  pump), so there's no new steady-state cost when search is idle.

## Test plan

- **Fuzzy matcher (unit + criterion):** subsequence correctness, boundary/
  contiguity scoring, ordering stability; p99 budget over a synthetic 100k-entry
  index at 1/3/10-char queries.
- **SearchIndex (server, MockDriver):** objects-only fallback while building;
  transition to Ready; invalidation drops + rebuilds; bulk-column parse.
- **Schema-search handler:** ranking, `kinds` filter, `match_ranges`, limit cap.
- **Data-search (server + live PG/MSSQL):** parameterization (injection attempt
  via `query` stays inert), `%`/`_` escaping, per-table + max-tables + byte caps
  enforced (`truncated` set), cancel mid-fan-out stops pending tables, text-ish
  column selection, `row_key` populated for identifiable rows.

## Open questions (resolve during implementation)

- **SearchIndex memory ceiling.** 100k entries × ~64 B ≈ 6 MB/spec — fine. Cap
  the number of indexed specs (reuse `SchemaCacheConfig.max_entries` philosophy)
  and evict cold ones; flag if an index is skipped for a huge schema.
- **Should schema search fold in comments/descriptions** (PG `COMMENT`, MSSQL
  extended properties) as secondary haystacks? Nice for discovery; adds catalog
  columns to the bulk query. Default off.
- **Numeric/date data search.** Casting every numeric column to text for `ILIKE`
  is expensive and index-defeating. Propose: only search numeric/date columns
  when the query parses as that type, via typed equality/range, not `LIKE`.
- **Ranking data-search hits across tables** — by table relevance (name match to
  query?) or purely by arrival order? Default: arrival order, table-tagged; let
  the client group.
