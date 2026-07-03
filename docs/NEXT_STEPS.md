# sift — Next Steps After Phase 0

## Product Goal

Build a fast, server-first database IDE. Server owns connections, schema,
query execution, history, audit, and collaboration primitives. Clients stay
thin and consume the public HTTP/WebSocket API.

## Phase 0 Verdict

Phase 0 is effectively complete for the stated goal: a third party can run the
headless server, inspect OpenAPI, use the SDK, and build UI flows against
Postgres and SQL Server without private guidance.

Known limits are not Phase 0 blockers:

- SQL Server cancel drops/cleans the connection instead of sending true TDS ATTENTION.
- SQL Server MARS and native BCP are unsupported; CSV bulk import works.
- Postgres month-aware intervals are engine-native because `chrono::Duration` has no months.

## Phase 1 — Usable Local IDE

Goal: make one person productive against real PG/SQL Server daily.

1. **Workspace persistence**
   - Persist sessions, tabs, query text, recent connections, layout metadata.
   - Do not persist plaintext secrets; add keychain-backed secret references.
   - Restore UI state before any DB round-trip.

2. **Schema cache and search**
   - Cache shallow/deep schema snapshots per connection.
   - Invalidate via PG notifications where available; SQL Server starts with polling.
   - Add global schema search endpoint: table/column/function by name.

3. **Query history and saved queries**
   - Store executed SQL, timing, row counts, engine, connection label, and errors.
   - Add saved-query CRUD API.
   - Keep history append-only; allow local pruning.

4. **Result export**
   - Public export API for CSV, TSV, JSON, clipboard-friendly payloads.
   - Stream large exports; avoid buffering full result sets in memory.

5. **Process and cancellation UX APIs**
   - Add active-query listing per session/connection.
   - Keep current SQL Server cancel behavior explicit in API response.
   - Later replace with true TDS ATTENTION when backend supports it safely.

## Phase 2 — Daily Driver Quality

Goal: make sift feel materially faster and safer than generic DB tools.

1. **Large-result ergonomics**
   - Cursor paging/prefetch beyond current ACK-gated stream.
   - Configurable page size, memory budget, cursor eviction.
   - Optional disk spill for huge results.

2. **Autocomplete substrate**
   - Endpoint for symbols scoped by connection/database/schema/query context.
   - Engine-aware keyword/function/type metadata.
   - Incremental refresh from schema cache.

3. **Transaction and edit workflows**
   - Public savepoint endpoints.
   - Inline row edit draft API: compute update/delete/insert SQL with preview.
   - Guardrails for missing primary key, ambiguous rows, and unsafe updates.

4. **Explain plans**
   - PG `EXPLAIN (FORMAT JSON)` and SQL Server estimated plan retrieval.
   - Stable protocol type for plan trees plus raw engine payload escape hatch.

## Phase 3 — Product Surface

Goal: start UI/client product work on top of stable server APIs.

1. **Desktop client shell**
   - Session picker, connection form, query tabs, result grid.
   - Use SDK only; no DB driver code in client.

2. **Editor integration**
   - SQL syntax highlighting, formatting, snippets.
   - Parameter prompt/run flow using HTTP execute params.

3. **Visual database navigation**
   - Schema tree, object detail panels, DDL preview.
   - Search-first navigation.

4. **Polish loop**
   - Measure cold start, first query, schema refresh, large result latency.
   - Add benchmarks before optimizing internals further.

## Backend Debt Queue

Tackle only when product need justifies it:

- True SQL Server TDS ATTENTION cancellation.
- SQL Server MARS concurrent execution.
- SQL Server native BCP bulk format.
- PG month-aware interval type with months/days/micros fidelity.
- Prepared statement cache if benchmarks show parse overhead.
- OpenTelemetry/Prometheus instrumentation.

## Immediate Next Sprint

1. Add local metadata store crate/table migrations.
2. Persist workspace/session/query history without secrets.
3. Add schema cache with explicit refresh/invalidate API.
4. Add result export API and SDK method.
5. Start minimal desktop/web client only after those APIs are stable.
