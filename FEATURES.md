# sift — SQL IDE Feature Checklist

End-user features, tiered by ship priority. Order within tiers = dependency
order. 🔌 = requires server-side support (see `BACKEND.md`). [D]esign before
[I]mplement within each tier.

## Tier 0 — must have for usable v1 (desktop Phase 1)
The price of entry. Without these the tool is unusable as a Navicat alternative.

1. [D] Connection registry — saved connections, named, grouped, test-on-add. 🔌
2. [D] Query editor — syntax highlighting, multi-tab, run selection / statement / all.
3. [D] Result grid — pagination, NULL display, type-aware cell rendering (text, int, date, bool). 🔌
4. [D] Schema tree — server → db → schema → tables/views, click-to-inspect columns. 🔌
5. [D] Run query + cancel — async, cancel button, elapsed indicator. 🔌
6. [D] Error reporting — query failures surface cleanly, not stack traces.
7. [I] Connection form — host, port, user, password, db, SSL toggle, SSH tunnel.
8. [I] Tab persistence — query text + result state survive restart. 🔌
9. [I] Recent queries / history list. 🔌
10. [I] Multiple simultaneous connections. 🔌

## Tier 1 — daily-driver essentials
Without these the tool feels amateurish within a week of real use.

11. [D] Autocomplete — tables, columns, keywords, functions; engine-specific. 🔌
12. [D] Result export — CSV, JSON, TSV, clipboard. 🔌
13. [D] Inline DDL preview — "generate CREATE script" from any object. 🔌
14. [D] Result sorting + per-column filtering.
15. [D] Type-aware rendering — JSON pretty-print, UUID, bytes (hex), dates, enums. 🔌
16. [D] Find/replace in editor (and in results).
17. [I] SQL formatting.
18. [I] Code folding + multi-cursor.
19. [I] Snippets / templates.
20. [I] Large-result streaming + virtualized grid (1M rows without UI stall). 🔌
21. [I] Column freeze / resize / reorder.

## Tier 2 — power features (where sift beats Navicat)
22. [D] Inline cell editing → generates UPDATE/INSERT/DELETE, diff preview. 🔌
23. [D] Transactions panel — BEGIN/COMMIT/ROLLBACK, savepoints. 🔌
24. [D] Saved query library — per-user, shareable later. 🔌
25. [D] Global schema search — find any table/column by name across catalog. 🔌
26. [D] Data search — find a value across all tables (scoped, slow, expected). 🔌
27. [D] Execution plans — EXPLAIN (estimated + actual), visualized. 🔌
28. [D] Process list — see running queries, kill. 🔌
29. [D] Command palette (already implied by ADR-006 op model).
30. [I] Table designer — visual create/alter.
31. [I] CSV import → table. 🔌
32. [I] Result pinning — multiple results side-by-side.
33. [I] Parameterized queries — bind vars, prompt-on-run.

## Tier 3 — differentiators (most defer to Phase 2+)
34. [D] Schema diff — compare two DBs, generate migration script. 🔌
35. [D] ERD — auto-layout schema visualization, export image.
36. [D] Backup/restore — full DB, schedule. 🔌
37. [D] Locks monitor. 🔌
38. [D] Storage + perf counters. 🔌
39. [D] Themes (light/dark), keyboard shortcut customization.
40. [D] Multi-window + detachable tabs.
41. [D] Live co-editing of query tabs (CRDT — Zed lesson §3.5). 🔌
42. [D] Annotations / comments on shared queries.
43. [I] Visual view builder.
44. [I] Index manager UI.
45. [I] Migration tool integration (refinery, sqitch, flyway).
46. [I] Plugin / extension API.

## Sequencing rationale
- **Tier 0** = the minimum to be a usable tool. Maps to desktop Phase 1.
- **Tier 1** = where users decide whether to keep using sift after a week.
- **Tier 2** = where comparisons to Navicat / DataGrip flip in sift's favor.
- **Tier 3** = hosted / multi-user / visual-designer territory; much of this can wait until the server is proven in single-user mode.

Within each tier, design [D] precedes implement [I] for tightly-coupled items — e.g., autocomplete design (#11) informs both the editor UI and the server's schema-introspection API, so design both before building either.
