# sift — Server Backend Requirements

Server-side infrastructure required to support `FEATURES.md`. Tiered by ship
priority; within tiers, dependency order. 🔌 markers on the client side cross-
reference items here. [D]esign before [I]mplement within each tier.

## Tier 0 — server doesn't run without these
Maps to Phase 0 of `PHASE0.md`. Without these no client can do anything.

1. [D] HTTP server — axum routes, JSON in/out, sync ops.
2. [D] WebSocket server — bidirectional stream for query results + server-push events.
3. [D] Driver registry + factory — instantiate drivers by engine kind from config.
4. [D] Connection lifecycle — open, validate, close, track per session.
5. [D] Session lifecycle — open, hold connections, suspend, close, reap idle.
6. [D] Operation dispatcher — receive Operation, route to handler, return Response. (ADR-006)
7. [D] Query execution path — submit to driver, return result/cursor handle.
8. [D] Result paging — server-side cursors, page-by-cursor-id, eviction policy. (ADR-011 candidate)
9. [D] Cancellation — CancelQuery op propagates to driver (pg_cancel_backend / SQL Server attention).
10. [D] Error model — driver-agnostic error codes, never leak raw driver errors across the wire. (ADR-004)
11. [D] API versioning — header + semver policy. (ADR-016 candidate)
12. [I] TLS termination.
13. [I] Config loading — figment, TOML + env layered.
14. [I] Structured logging — tracing + tracing-subscriber.

## Tier 1 — reliability layer
Turns a working server into one you'd trust with real data.

15. [D] Connection pooling — per-DB pool, warm on session open, idle reap, broken-detection.
16. [D] Driver isolation — wedged driver can't crash server; queries run in `tokio::spawn` with cancel tokens + timeouts. (ADR-013 candidate)
17. [D] Graceful shutdown — drain in-flight queries, close pools, persist state.
18. [D] Reconnect logic — drop detection, transparent re-establish.
19. [D] Health + readiness endpoints.
20. [D] Audit log — every Operation logged with actor, timestamp, duration, result code. (ADR-006)
21. [D] Persistent state store — SQLite for connection registry (no secrets), saved queries, history, layout.
22. [I] Secrets in OS keychain — never plaintext, never logged.
23. [I] Metrics endpoint — Prometheus format.
24. [I] OpenTelemetry export.

## Tier 2 — performance layer (Zed-style snappiness)
25. [D] Schema cache — lazy load per session, cache invalidated on RefreshSchema op.
26. [D] Schema invalidation signals — LISTEN/NOTIFY (PG), polling or DDL triggers (SQL Server).
27. [D] Predictive result prefetch — fetch page N+1 before client acks page N.
28. [D] Pre-warm pool — open min connections on session open.
29. [I] Large-result spill to disk — cursor eviction bounded; spill optional.
30. [I] Response compression — gzip / br.

## Tier 3 — hosted / multi-user (major scope expansion)
31. [D] Auth — token-based, multi-user identity, loopback exemption for local mode. (ADR-010)
32. [D] Authorization — per-connection permissions (read-only, blocked ops, schema-scoped).
33. [D] Rate limiting.
34. [D] Tenant isolation — connection quotas, query caps, resource limits per tenant.
35. [D] Shared saved-query namespace — per-tenant + per-user sharing model.
36. [D] Live co-edit session broker — CRDT for query text only (Zed lesson §3.5; never for results/sessions/connections).
37. [I] Plugin / extension loading — 3rd-party drivers, custom operations.
38. [I] Scheduler — backup jobs, periodic queries, alerts.

## Tier 4 — operations polish (comes last)
39. [D] Background updater — fetch new binary on idle, swap on next launch. (Zed lesson §2.3)
40. [D] Server-side migrations — refinery for sift's own metadata DB.
41. [D] Single-binary distribution — in-process / daemon / docker modes, same binary. (ADR-010)
42. [I] Backup/restore ops — wrap native tools (pg_dump, sqlpackage / mssql-scripter).
43. [I] Query plan capture + retrieval.

## Sequencing rationale
- **Tier 0** must exist before any client can do anything useful; aligns 1:1 with `PHASE0.md` build order.
- **Tier 1** is the difference between "demo works" and "I'd put real data in this."
- **Tier 2** is where snappiness lives — caches, prefetch, pool warmth. The differentiator vs Navicat-class tools that feel sluggish.
- **Tier 3** unlocks hosted/multi-user; large scope jump, defer until single-user server is proven.
- **Tier 4** is ops polish, last.

Each client-side 🔌 in `FEATURES.md` traces back to one or more items here. Closely-coupled pairs (autocomplete UI ↔ schema cache + invalidation; inline edit ↔ transaction state) should be designed together across both docs even if implemented in different phases.
