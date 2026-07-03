# sift — Build Order

Design [D] and implement [I], in dependency order. Minimal prose.

## 0. Foundation
1. [D] Driver trait shape — thin core + fat per-engine structs + per-engine ext traits + union protocol types
2. [D] Protocol v1 union types — Connection, Catalog, Schema, Object, ColumnMetadata (superset of both engines), TypeRef, Error codes
3. [D] Operation enum v1 — OpenConnection, CloseConnection, RefreshSchema, ExecuteQuery, CancelQuery, ListSessions
4. [D] WS envelope + cursor paging protocol
5. [D] API versioning scheme — header + semver policy
6. [D] Auth model — bearer token, loopback exemption
7. [D] Workspace snapshot shape — tabs, queries, layout, scroll, column widths
8. [I] Cargo workspace — `crates/{protocol, core, driver-api, driver-postgres, driver-sqlserver, server, client-sdk}`
9. [I] Protocol crate — pure serde, zero I/O
10. [I] Error model + versioning middleware

## 1. Server substrate (Postgres — the easy case)
11. [I] Server bootstrap — axum, tokio, tower, tracing, figment
12. [I] `driver-api` trait (from step 1 spec)
13. [I] Session + connection manager
14. [I] deadpool-postgres pool
15. [I] `driver-postgres` impl
16. [I] HTTP surface — REST ops
17. [I] WS streaming — server-side cursor + backpressure
18. [I] Auth middleware
19. [I] OpenAPI generation
20. [I] `client-sdk` reference consumer
21. [I] Integration tests — Postgres end-to-end + blind-client spec-only test

## 2. Hard-case validation (SQL Server via tiberius)
22. [D] SQL Server ext-trait surface — MARS, catalog switching, batch semantics, cancel/attention
23. [I] `driver-sqlserver` impl (tiberius)
24. [I] SQL Server integration tests
25. [I] Trait refactor if step 23 exposes a flaw (expected; contained to driver layer)

## 3. Snappiness hardening (Zed lessons §2, §4)
26. [I] Workspace snapshot persistence + restore-on-launch (paint before any DB round-trip)
27. [I] Pre-warm connection pool on session open
28. [I] Schema cache + invalidation (LISTEN/NOTIFY for Postgres; polling for SQL Server)
29. [I] Predictive result prefetch
30. [I] Background updater — fetch-on-idle, swap on next launch

## Phase 0 done when
- Both engines connect → query → stream → cancel end-to-end
- OpenAPI published; blind client built from spec alone
- Trait has 2 impls; no engine leak into core trait
- Snapshot restores workspace before any DB round-trip
- Every op logged + replayable
