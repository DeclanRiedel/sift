# AGENTS.md

Guidance for any agent working in this repository. Read `README.md` first for
what sift is and the five product goals; this file is the operational subset.

## Layout

- `crates/protocol` — pure serde wire contract. No I/O, no Tokio, no OS APIs.
- `crates/driver-api` — `Driver` trait + engine ext traits. Server-internal.
- `crates/driver-postgres`, `crates/driver-sqlserver` — engine impls.
- `crates/server` — axum + sessions + rooms + metadata wiring.
- `crates/metadata` — SQLite + refinery; secrets never live here.
- `crates/doc` — text-document apply-op abstraction (real CRDT backend not yet chosen).
- `crates/client-sdk` — thin reference HTTP + WebSocket consumer.
- `crates/core` — reserved for shared server-internal types (currently empty).
- `docs/DECISIONS.md` — load-bearing ADRs.
- `docs/PLANS/server-build-list-v2.md` — code-grounded ordered backlog before GUI.

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
- Both real drivers (Postgres, SQL Server) pass through the `Driver` trait.
  The trait lock is formalized via ADR-017 graduation (build-list Phase A);
  after that, a signature change gates a protocol bump.
- Never commit secrets. Never guess URLs.

## Secrets and local env

- **`.env` at the repo root is the single source of truth for every
  local env var the project reads.** It is gitignored. `.env.example`
  is the committed template listing every var with comments.
- Everything is loaded automatically on directory entry via `.envrc`
  (`use flake` + `dotenv_if_exists .env`). No `source .env` or manual
  export before `cargo test` / `cargo run`. If direnv isn't wired,
  `set -a; source .env; set +a` matches the same behavior.
- Secret-shaped values in `.env`:
  - **`SIFT_METADATA__SECRET_KEY_FILE`** — 32-byte hex keyfile for the
    file secret backend. Auto-generated at `.sift/dev-secret.key` on
    devshell entry via `scripts/dev-secret-key.sh` (invoked by the
    flake `shellHook`). Do not set this manually in `.env`.
  - **`SIFT_MSSQL_PASSWORD`** — SA password for the local MSSQL
    docker container. Managed by `scripts/dev-mssql.sh` (or `nix run
    .#dev-mssql <sub>`), which generates a policy-compliant random
    password into `.env` on first `start` and boots the container
    with the same value. `.env` is authoritative; the container is
    rebuilt from it. Never edit either side by hand.
  - **`SIFT_PG_PASSWORD`** — usually unset. The flake demo Postgres
    (`nix run .#sift-demo-postgres`) uses socket trust auth. Only
    set when pointing at a non-demo PG.
  - **`SIFT_AUTH__BEARER_TOKEN`** — optional, only if testing bearer
    auth locally. Any string.
- Runtime secrets sift stores *on behalf of its users* (connection
  passwords, credentials) are a separate concern handled by the
  `SecretStore` trait (ADR-008); those never touch `.env`.
- If a secret leaks into git, rotate at the source (regenerate the pw,
  reissue the token) and remove the commit — `.env` living in git
  history is the same failure mode as a committed password.
