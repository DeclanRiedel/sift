# AGENTS.md

Guidance for any agent working in this repository. Read `README.md` first for
what sift is and the five product goals; this file is the operational subset.

## Layout

- `crates/protocol` — pure serde wire contract. No I/O, no Tokio, no OS APIs.
- `crates/driver-api` — `Driver` trait + engine ext traits. Server-internal.
- `crates/driver-postgres`, `crates/driver-sqlserver` — engine impls.
- `crates/server` — axum + sessions + rooms + metadata wiring.
- `crates/metadata` — SQLite + refinery; secrets never live here.
- `docs/DECISIONS.md` — load-bearing ADRs.
- `docs/PLANS/server-build-list-v2.md` — ordered backlog before GUI.

## Non-negotiable rules

- `sift-protocol` stays pure serde. No I/O leaks into it.
- UI dependencies never enter shared crates.
- Secrets never live in SQLite; only opaque handles. Never log secret bytes.
- Every user-visible action is an `Operation` variant and is audited.
- A wedged driver cannot freeze the server — queries run in `tokio::spawn`
  with timeouts + cancel tokens, never inline in handlers.
- CRDTs are for query text only. Never for results, schema, or sessions.

## Workflow

- `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace` stay green. CI enforces all three plus cargo-deny.
- Design precedes Implement for tightly-coupled pairs; graduate stable
  decisions into `docs/DECISIONS.md` as ADRs.
- The `Driver` trait is not locked until PG + SQL Server both pass through it.
  Until then, refactor freely; after, a signature change gates a protocol bump.
- Never commit secrets. Never guess URLs.
