# sift — SQL IDE Feature Checklist

End-user features, tiered by ship priority. Order within tiers = dependency
order. This doc tracks **what the server already supports** versus what
remains; the ordered server-side backlog is
`docs/PLANS/server-build-list-v2.md` (Phase D covers the 🔌 items below).

Per-item status tag (server-side only):
- **server: done** — server support shipped and exercised.
- **server: partial** — shipped with a known gap (gap noted inline).
- **server: no** — not yet on the server; see the build-list phase.
- **client** — client-side feature; no server work beyond the current API.

## Tier 0 — must have for usable v1
The price of entry. Without these the tool is unusable as a Navicat alternative.

1. [D] Connection registry — saved connections, named, grouped, test-on-add.
   **server: done** — `connection_profile` + `connection_credential` tables,
   `open-from-profile`, `ping` route (`http.rs:113`, `:121`).
2. [D] Query editor — syntax highlighting, multi-tab, run selection / statement
   / all. **client** (server provides `execute`).
3. [D] Result grid — pagination, NULL display, type-aware cell rendering.
   **server: partial** — WS streams pages with ack (`http.rs:2152`); the HTTP
   `execute` path drains all rows unbounded (`session.rs:649` — Phase B cap).
4. [D] Schema tree — server → db → schema → tables/views, click-to-inspect
   columns. **server: partial** — shallow + deep schema for PG (incl triggers);
   SQL Server omits triggers and lists only tables/views (`driver-sqlserver`).
5. [D] Run query + cancel — async, cancel button, elapsed indicator.
   **server: partial** — execute + cancel routes work; PG cancel is real
   (`CancelToken`), SQL Server cancel is `task.abort()` (Phase A TDS attention).
6. [D] Error reporting — query failures surface cleanly, not stack traces.
   **server: partial** — `Code` enum + `{kind, message}` body; no correlation
   id yet (Phase B).
7. [I] Connection form — host, port, user, password, db, SSL toggle, SSH tunnel.
   **client** (server stores `ConnectionSpec` + `SslMode`).
8. [I] Tab persistence — query text + result state survive restart.
   **server: no** — sessions are in-memory; query text is not persisted (only
   `query_history` rows).
9. [I] Recent queries / history list. **server: done** — `query_history` table
   + `GET /v1/metadata/history` (`http.rs:100`, bind values omitted).
10. [I] Multiple simultaneous connections. **server: done** — list/open/close
    per session (`http.rs:110`, `:117`).

## Tier 1 — daily-driver essentials
Without these the tool feels amateurish within a week of real use.

11. [D] Autocomplete — tables, columns, keywords, functions; engine-specific.
    **server: no** (Phase D).
12. [D] Result export — CSV, JSON, TSV, clipboard. **server: no** — `PgExt::copy`
    exists at the driver layer; no server route exposes it (Phase D).
13. [D] Inline DDL preview — "generate CREATE script" from any object.
    **server: no** (Phase D).
14. [D] Result sorting + per-column filtering. **client**.
15. [D] Type-aware rendering — JSON pretty-print, UUID, bytes (hex), dates, enums.
    **server: partial** — `Value` enum + `TypeRef` carry the type hints clients
    need (`protocol/src/value.rs`, `column.rs`).
16. [D] Find/replace in editor (and in results). **client**.
17. [I] SQL formatting. **client**.
18. [I] Code folding + multi-cursor. **client**.
19. [I] Snippets / templates. **client**.
20. [I] Large-result streaming + virtualized grid (1M rows without UI stall).
    **server: partial** — WS path streams with backpressure; no server-side
    cursor registry or eviction yet (Phase C).
21. [I] Column freeze / resize / reorder. **client**.

## Tier 2 — power features (where sift beats Navicat)
22. [D] Inline cell editing → generates UPDATE/INSERT/DELETE, diff preview.
    **server: no** (Phase D).
23. [D] Transactions panel — BEGIN/COMMIT/ROLLBACK, savepoints.
    **server: partial** — begin/commit/rollback routes work; savepoints exist
    in the drivers (`PgExt`/`MssqlExt`) but are not reachable over the protocol
    (Phase A `Operation::Savepoint`).
24. [D] Saved query library — per-user, shareable later. **server: no** — the
    `saved_query` table exists but is dead schema (Phase D wiring).
25. [D] Global schema search — find any table/column by name across catalog.
    **server: no** (Phase D).
26. [D] Data search — find a value across all tables (scoped, slow, expected).
    **server: no** (Phase D).
27. [D] Execution plans — EXPLAIN (estimated + actual), visualized.
    **server: no** — and no structured `PlanNode` protocol type yet (Phase D).
28. [D] Process list — see running queries, kill. **server: no** (Phase D).
29. [D] Command palette (already implied by ADR-006 op model).
    **server: partial** — `GET /v1/operations` returns the full list unfiltered
    (`http.rs:649`); no capability-context filtering (Phase D).
30. [I] Table designer — visual create/alter. **server: no**.
31. [I] CSV import → table. **server: partial** — MSSQL `bulk_insert` (CSV) and
    PG `PgExt::copy` (Import) work at the driver layer; only bulk-insert is
    routed (Phase D full pipeline).
32. [I] Result pinning — multiple results side-by-side. **client**.
33. [I] Parameterized queries — bind vars, prompt-on-run.
    **server: done** — `execute` takes `params: Vec<Value>`.

## Tier 3 — differentiators (most defer to Phase 2+)
34. [D] Schema diff — compare two DBs, generate migration script. **server: no**.
35. [D] ERD — auto-layout schema visualization, export image. **client**.
36. [D] Backup/restore — full DB, schedule. **server: no** (Phase J).
37. [D] Locks monitor. **server: no**.
38. [D] Storage + perf counters. **server: no**.
39. [D] Themes (light/dark), keyboard shortcut customization. **client**.
40. [D] Multi-window + detachable tabs. **client**.
41. [D] Live co-editing of query tabs (CRDT — §3.5).
    **server: partial** — room runtime broadcasts document ops + persists
    opaque snapshots, but `sift-doc` is not a real CRDT yet (Phase G).
42. [D] Annotations / comments on shared queries. **server: no**.
43. [I] Visual view builder. **client**.
44. [I] Index manager UI. **client**.
45. [I] Migration tool integration (refinery, sqitch, flyway). **server: no**.
46. [I] Plugin / extension API. **server: no** (Phase I).

## Sequencing rationale
- **Tier 0** = the minimum to be a usable tool. Mostly shipped on the server;
  the open gaps are HTTP result cap, savepoint protocol, SQL Server deep-schema
  parity — all Phase A/B items.
- **Tier 1** = where users decide whether to keep using sift after a week.
  Autocomplete, export, DDL generation land in build-list Phase D.
- **Tier 2** = where comparisons to Navicat / DataGrip flip in sift's favor;
  build-list Phase D (server) covers the 🔌 items.
- **Tier 3** = hosted / multi-user / visual-designer territory; collaboration
  depth is build-list Phase G, extensibility Phase I, backup/restore Phase J.

Within each tier, design [D] precedes implement [I] for tightly-coupled items —
e.g., autocomplete design (#11) informs both the editor UI and the server's
schema-introspection API, so design both before building either.
