# sift — Early Architectural Decisions

> Recorded at project start (phase 0). Each is reversible until code locks it
> in; once a crate boundary depends on one, treat it as load-bearing.

Format is ADR-lite: **Context · Decision · Consequences.**

---

## ADR-001 — Three-artifact split: server, desktop, web

**Context.** Requirements are: native desktop performance, browser/tablet
access, hosted multi-user sessions. No single UI technology serves desktop and
browser equally well; GPUI in particular is desktop-only.

**Decision.** Build three artifacts sharing one protocol:
`workspace-server` (the product), `client-desktop` (GPUI), and `client-web`
(later). The server does not know or care which client is talking to it.

**Consequences.** (+) frontends can be built/released independently and each
play to its strengths; (+) the server is reusable and headless-ly testable;
(+) multi-user is a server feature, not a UI problem. (−) some duplicated UI
effort across desktop and web; (−) the protocol must be kept stable and
versioned, because two consumers depend on it.

---

## ADR-002 — The server is the product; clients are stateless shells

**Context.** Navicat-style tools couple the window directly to the database.
That makes them brittle, single-user, and impossible to host or share.

**Decision.** All business logic — connections, sessions, drivers, query
execution, schema metadata, exports — lives in the server. Clients render
server state only. The call chain is:

```
Window -> WorkspaceClient -> WorkspaceAPI -> WorkspaceServer -> Driver -> DB
```

never `Window -> Connection -> DB`.

**Consequences.** (+) UI stays thin and testable; (+) multi-user falls out for
free; (+) one backend serves every client. (−) higher latency than an embedded
client; (−) the protocol and streaming model must be good enough that this
isn't felt.

---

## ADR-003 — tiberius (pure Rust) for SQL Server, not ODBC

**Context.** SQL Server from Rust is commonly done via ODBC + `msodbcsql18` +
`freetds`/`unixODBC`. That stack is unfree, awkward to package on Nix, and
introduces non-reproducible native runtime dependencies — directly fighting the
Nix dev-env goal.

**Decision.** Use **tiberius** (pure-Rust TDS client) for SQL Server, and
**tokio-postgres** for PostgreSQL. Both are async and tokio-native.

**Consequences.** (+) fully reproducible Nix builds with no native runtime deps;
(+) idiomatic async; (+) same packaging story on Linux/macOS/Windows. (−)
tiberius is narrower than the ODBC surface for some exotic SQL Server features;
flag any gap when a driver needs it and reassess per-feature.

---

## ADR-004 — The protocol crate is pure serde types, no I/O

**Context.** The desktop binary, a future wasm web client, and the server must
all agree on the exact same request/response/operation contract.

**Decision.** A dedicated `protocol` crate holds operation enums,
request/response structs, error codes, and serde models — and **nothing else**.
No `tokio`, no networking, no filesystem. Pure data.

**Consequences.** (+) shareable across native and wasm without dependency
friction; (+) trivially versionable and unit-testable; (+) the API surface is
explicit and reviewable. (−) requires discipline to stop server-internal types
leaking into it.

---

## ADR-005 — GPUI is confined to the desktop binary crate

**Context.** GPUI is desktop-only and its API is still maturing. Letting it
touch shared crates would couple the whole system to one UI toolkit.

**Decision.** GPUI lives **only** in `client-desktop`. It is never a dependency
of `core`, `protocol`, `server`, `driver-api`, or any driver. Shared types are
mapped to GPUI models at the boundary, inside the desktop crate.

**Consequences.** (+) UI/logic separation enforced at the compiler level;
(+) desktop UI tech remains swappable without touching the server. (−) a little
boilerplate mapping protocol types to GPUI view entities at the edge.

---

## ADR-006 — Operation/command model (Zed-inspired)

**Context.** Ad-hoc verbs ("execute SQL") do not scale to an IDE's surface area
and don't give us a command palette, undo, replay, or uniform audit.

**Decision.** Every user action is an **Operation** sent to the server:
`OpenConnection`, `RefreshSchema`, `ExecuteQuery`, `CancelQuery`, `RenameTable`,
`ExportCsv`, `Import`, `Backup`, `CompareSchemas`, ... The server is a command
processor over a session.

**Consequences.** (+) powers the command palette; (+) every action is loggable,
replayable, and auditable in one place; (+) uniform auth/rate-limit hooks.
(−) a bit more upfront design per action; the protocol is the product.

---

## ADR-007 — Async end-to-end (tokio)

**Context.** Database work is I/O-bound; queries run concurrently; results
stream over WebSocket.

**Decision.** tokio runtime throughout the server, drivers, and client-sdk.

**Consequences.** (+) idiomatic for the driver crates; (+) natural streaming.
(−) GPUI runs its own event loop — the desktop client must bridge carefully
(run tokio on a side runtime, pass results back onto the UI thread), not nest
them.

---

## ADR-008 — Nix flake dev env; one toolchain source of truth

**Context.** Contributors on different OSes/toolchain versions cause build
drift, "works on my machine" failures, and CI/local divergence.

**Decision.** `flake.nix` + `rust-toolchain.toml`. rust-overlay reads the toml,
so Nix and non-Nix (rustup) hosts use the identical toolchain. direnv
auto-loads the shell on `cd`.

**Consequences.** (+) reproducible, one-command (`nix develop`) setup;
(+) CI can reuse the same flake. (−) Nix learning curve for new contributors;
mitigated by `rust-toolchain.toml` working standalone via rustup.

---

## ADR-009 — Defer the web client

**Context.** Phase 0/1 budget is a working server + desktop client. The web
client is a large surface (wasm, assets, auth UI, browser quirks).

**Decision.** No web client in phase 0 or 1. Ship desktop + server first; build
the web client once the protocol is proven and stable.

**Consequences.** (+) focus and a faster path to something usable; (+) the
protocol is battle-tested before the second consumer arrives. (−) users without
a desktop install can't use sift in the meantime — an accepted, temporary cost.

---

## ADR-010 — Local-first by default, hosted as a mode

**Context.** A single user wanting a Navicat-like experience shouldn't have to
run a daemon and connect to it. But multi-user/hosted must be a first-class
mode, not a bolt-on.

**Decision.** The server runs as a single in-process or localhost instance for
the desktop client by default, and as a real daemon for multi-user/hosted use.
Same binary, same code paths, different config.

**Consequences.** (+) good single-user UX with zero ops; (+) all server logic
is reused in both modes. (−) the client-sdk must treat embedded-vs-remote
transparently, so the UI never knows which mode it's in.
