# sift — Phase 0 Plan (TLDR)

> Server-first. The HTTP + WebSocket API **is** the product. Phase 0 ends when
> a third party can read the spec and build a working UI against it without
> talking to us. Desktop client is Phase 1.

Goal: ship a versioned, documented, headless-runnable server with **two** real
drivers (Postgres first as the easy case, SQL Server as fast-follow stress
test), end-to-end, test-covered. Driver trait shape is locked on paper against
SQL Server's model **before any driver code is written**. Nothing else.

## Build order

Each step locks the tech it depends on. Later steps assume earlier ones.

1. **Cargo workspace + crate skeletons.**
   `crates/{protocol, core, driver-api, driver-postgres, driver-sqlserver, server, client-sdk}`.
   `client-desktop` and `client-web` stubbed but empty — out of scope.
   *cargo, cargo-nextest, clippy -D warnings.*

2. **Protocol crate — the public contract.**
   Pure serde: operation enums, request/response structs, error codes, stream
   envelope. Zero I/O, zero tokio (ADR-004). This crate is what 3rd parties
   import; it must be pristine and versioned from commit one.
   *serde, serde_json, thiserror.*

3. **Error model + API versioning.**
   Stable error codes (not driver leaks). `X-Sift-Protocol-Version` header.
   Breaking changes bump major; semver discipline starts now.
   *thiserror in libs, anyhow never crosses the wire.*

4. **Driver trait shape — on paper, against SQL Server's model, before any driver code.**
   Thin core trait (`execute`, `cancel`, `schema`, `begin/commit`) returning
   protocol-union types. Fat per-engine structs. Per-engine extension traits
   for engine-specific ops (MARS, LISTEN/NOTIFY). Design target is the harder
   engine so the trait generalizes; Postgres collapses to the degenerate case.
   No engine-specific types leak into the core trait — JDBC's
   lowest-common-denominator failure mode is the cautionary tale.
   *Output: a written spec, not code. Code starts in step 7.*

5. **Server bootstrap.**
   axum app, tokio runtime, Tower middleware, structured tracing, figment
   config (TOML + env). Local-first mode (ADR-010): one binary, runs in-process
   or as a daemon, same code paths.
   *axum, tokio, tower, tracing + tracing-subscriber, figment.*

6. **Session + connection manager.**
   Session = a logical workspace; holds connection handles. Connection pooling
   per registered DB. Pool warm on session open (Zed lesson §4 #11).
   *deadpool-postgres (pick over bb8 — async-native, simpler).*

7. **`driver-api` trait + `driver-postgres` impl — the easy case.**
   Implement the step-4 spec. Postgres first because tokio-postgres is mature
   and known-good — server-substrate bugs aren't confounded with driver bugs.
   Driver isolation: a wedged driver cannot take the server down (run queries
   in `tokio::spawn` with timeouts + cancel tokens, not in-line handlers).
   *tokio-postgres, refinery (migrations for sift's own metadata, not user DBs).*

8. **Operations v1 — the minimum useful set.**
   `OpenConnection`, `CloseConnection`, `RefreshSchema`, `ExecuteQuery`,
   `CancelQuery`, `ListSessions`. Each is one operation enum variant, one
   server handler, one log line, one audit row (ADR-006).
   *Driven by the protocol crate; no ad-hoc verbs.*

9. **HTTP surface.**
   REST mapping of operations: `POST /v1/connections`, `POST /v1/queries`,
   `POST /v1/queries/:id/cancel`, `GET /v1/schema`. Sync ops over HTTP.
   *axum extractors.*

10. **WebSocket streaming surface.**
    Result sets stream page-by-page from a **server-side cursor** (ADR-011
    candidate). Client never holds the full result; backpressure tied to
    consumer ack. Envelope is in the protocol crate.
    *axum::extract::ws, tokio-tungstenite.*

11. **Auth — minimal but present.**
    Bearer token; local mode may run tokenless on loopback. No multi-user yet,
    but the auth hook exists so 3rd-party hosted use isn't a bolt-on later.

12. **OpenAPI spec, generated and published.**
    The spec is part of the release artifact, not an afterthought. This is what
    makes the API genuinely public.
    *utoipa + utoipa-axum (or hand-rolled if utoipa fights the streaming parts).*

13. **`client-sdk` reference consumer.**
    A thin Rust client proving the API is buildable-against from the outside.
    If the SDK is awkward to write, the API is wrong — fix the API.
    *reqwest (HTTP), tokio-tungstenite (WS).*

14. **Integration tests — Postgres end-to-end.**
    Real Postgres in a container; full flow from connect → query → stream →
    cancel → close. Plus a "blind client" test that consumes only the OpenAPI
    spec, proving 3rd-party viability.
    *testcontainers (postgres), wiremock, cargo-nextest.*

15. **`driver-sqlserver` impl (tiberius) — the hard case.**
    The trait's real stress test. SQL Server's catalog model, multi-result
    batches, full column metadata, MARS, richer cancel/attention semantics hit
    the trait where Postgres didn't. Refactor the trait if a flaw surfaces —
    expected, and contained because the server substrate is already stable.
    **The trait is not "public" until this lands.**
    *tiberius (pure-Rust TDS, ADR-003).*

16. **Integration tests — SQL Server end-to-end.**
    Same flow as step 14 against SQL Server. Confirms the trait genuinely
    generalizes, not accidentally Postgres-shaped.
    *testcontainers (mssql — x64 image, heavier CI).*

## Definition of Done

- **Both** engines connect → query → stream → cancel end-to-end.
- OpenAPI spec published; a 3rd party could build a UI from it alone.
- Protocol versioned; error model stable and driver-agnostic.
- Trait has two implementations; no engine-specific leak into the core trait.
- Server runs headless and as a local companion to a (Phase 1) desktop client.
- Every operation is logged, replayable, auditable.

## Explicitly out of Phase 0

- GPUI desktop client (Phase 1).
- Web client (ADR-009).
- Multi-user session sync, CRDT query tabs (Zed lesson §6, ADR-014 candidate).
- Result-grid virtualization — that's a client problem; server just streams.
- Real auth/RBAC; token-gated is enough for now.

## One constraint that shapes everything

**The API is public on day one.** Every endpoint, error code, and WS message
is something a 3rd party will depend on. Treat breaking changes accordingly.
(Worth graduating into ADR-016: "the protocol crate and the HTTP/WS surface
are a public API, semver-stable from v0.1.")

## Why Postgres first, SQL Server as fast-follow (not deferred)

The trait-design argument cuts toward SQL Server first; the risk-minimization
argument cuts toward Postgres first. Decouple them: **design the trait on
paper against SQL Server's model** (step 4) so the hard case shapes the
abstraction, then **implement Postgres first** so server-substrate bugs aren't
confounded with an immature-driver bug, then **land SQL Server inside Phase 0**
(steps 15–16) so the trait is genuinely stress-tested before being called
public. If SQL Server slips past Phase 0, the trait ships having only ever
been validated against the easy case — that is the failure mode to avoid.
