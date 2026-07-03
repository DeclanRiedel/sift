# sift — Tech Stack

> Status: phase 0 (dev environment + scaffolding). Picks below are locked for
> phase 0; items marked **(tbd)** are deferred and not yet binding.

sift is a database IDE. The **server is the product**; the desktop and web
clients are thin, stateless consumers of a single HTTP + WebSocket API. The
whole stack is Rust, developed inside a Nix flake.

## At a glance

| Layer | Choice | Notes |
| --- | --- | --- |
| Language | Rust (stable) | pinned via `rust-toolchain.toml` |
| Async runtime | tokio | server, drivers, client-sdk |
| Dev environment | Nix flakes + rust-overlay + direnv | `nix develop` / `use flake` |
| Toolchain pin | `rust-toolchain.toml` | read by both rust-overlay and rustup |
| Build / test | cargo, cargo-nextest | |
| Lint / format | clippy (`-D warnings`), rustfmt | |
| Supply chain | cargo-deny | advisories + licenses |
| Faster rebuilds | sccache, cargo-watch | |
| HTTP server | axum | Tower/hyper based, async-native |
| WebSocket | axum ws / tokio-tungstenite | query streaming, server push |
| Serialization | serde + serde_json | MessagePack candidate for binary streams later |
| SQL Server driver | **tiberius** | pure-Rust TDS — no ODBC |
| PostgreSQL driver | tokio-postgres | + refinery for migrations |
| Connection pooling | **(tbd)** deadpool-postgres / bb8 | |
| HTTP client (SDK) | reqwest | desktop + wasm builds |
| WS client (SDK) | tokio-tungstenite | |
| Desktop UI | GPUI | isolated to its own binary crate |
| Web UI | **(tbd, phase 1+)** | Leptos / Seed / etc. |
| Errors | thiserror (libs), anyhow (apps) | |
| Tracing / logging | tracing + tracing-subscriber | structured, OTel-exportable later |
| Config | figment | TOML + env layered |
| Testing | cargo-nextest, wiremock, testcontainers | |
| Editor support | rust-analyzer | provided by the flake |

## Why these picks

- **tiberius over ODBC for SQL Server.** `msodbcsql18` is unfree, painful to
  package on Nix, and drags in freetds/unixODBC runtime deps. tiberius is a
  pure-Rust TDS implementation — fully reproducible in Nix, no native runtime
  dependency, async-native under tokio. Trade-off: less coverage of exotic SQL
  Server features; watch for it.

- **axum.** Tower/hyper ecosystem, the de-facto async Rust web stack. Clean
  extractor model, first-class WebSocket support, trivial to unit-test. Matches
  the operation/command handler model we want on the server.

- **Protocol crate is pure serde.** No tokio, no I/O — only request/response/
  operation types. Lets the exact same contract be consumed by the desktop
  binary, a future wasm web client, and the server, with no dependency friction.

- **GPUI confined to the desktop binary.** GPUI is desktop-only and its API is
  still evolving. By never making `core`, `protocol`, `server`, or the drivers
  depend on it, UI/logic separation is enforced at the compiler level and the
  desktop UI tech stays swappable.

- **Nix flake as dev env.** One command (`nix develop`) yields the same pinned
  Rust toolchain, native db libs (libpq, openssl), and cargo tooling on every
  machine and in CI. `rust-toolchain.toml` is the single source of truth that
  both rust-overlay (Nix) and rustup (non-Nix) read, so there is no split-brain
  between environments.

## Out of scope for phase 0

Web client, auth, multi-user session synchronisation, real driver
implementations, GPUI views, deployment packaging. See `DECISIONS.md` for the
deferral rationale.
