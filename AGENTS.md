# Sift

Sift is a native, Vim-first database IDE for PostgreSQL, SQL Server and SQLite.
A Rust server owns database behavior, sessions, metadata, authorization and audit;
the GPUI desktop, SDK, CLI and governed tools use its versioned HTTP/WebSocket API.
Run the same server locally or host it for a team.

Read [README.md](README.md) first. Use [the current TODO](docs/TODO.md) to choose
work, [the plan index](docs/PLANS/README.md) for scoped contracts, and
[architectural decisions](docs/DECISIONS.md) for the reasons behind boundaries.

## What we protect

### 1. Open at the core

Sift is AGPL-3.0-only. Keep its source, development status and constraints honest.
Preserve third-party licenses and attribution. Do not invent user counts,
production readiness, supported platforms or native acceptance evidence.

### 2. Performance without compromise

An IDE must remain responsive while queries, catalog reads and transfers run.
Keep blocking work off the UI and async executor threads. Bound queues, payloads,
retained results and background work; preserve streaming backpressure and virtual
lists. Avoid whole-catalog work on each keystroke, unnecessary clones, unbounded
logs and continuously repainting animations. Measure the path you changed;
a small fixture does not establish large-schema or desktop memory acceptance.

### 3. Remote ready

Local, hosted, SSH and tailnet connections are real product paths. The server
host owns database reachability, driver processes, tunnels and runtime secrets.
A desktop on a tailnet does not put its hosted server on that tailnet.
Derive endpoints from configured instances; never bake localhost or a developer's
host into a shipped path. Preserve TLS certificate and SSH host-key verification.

### 4. Complete, keyboard-first workflows

Vim is the only supported IDE/editor interaction mode. Ignore Standard mode.
Cover the explorer, editor, results, dialogs, command palette and relevant Vim
bindings rather than fixing only the entry point you happened to exercise.
Desktop, SDK, CLI and governed AI/MCP paths should share server behavior where
appropriate, with explicit capability refusals where unsupported. Sift's website
is not a shipped web database client; do not assume web/mobile IDE surfaces exist.

### 5. Trustworthy database work

Protect data and tell the truth about outcomes. A timeout, cancelled observer,
disconnected socket or successful RPC does not prove database rollback or commit.
Keep native execution ownership, source identity and receipts through settlement.
Never automatically retry an uncertain write. Retain read-only policy, production
confirmation, exact approvals, current authority checks and audited operations.

## Working style

Prefer ambitious outcomes and small, understandable systems. Understand the real
constraint before adding machinery. Remove obsolete complexity when the task
allows it; do not expand scope to redesign unrelated features.

These instructions are project defaults and constraints, not a reason to invent
approval gates for work already authorized. Follow explicit maintainer direction;
raise real conflicts involving data loss, authority or contracts before crossing
them. People may be using Sift and its hosted databases while you work.

## Vocabulary

- **You** means the coding agent changing this repository. **Maintainers** means
  the people building Sift; **user** means the person using the database IDE.
- **Client** means an API consumer, including the desktop, SDK, CLI or governed tool.
- **Instance** means one server identity and its owned configuration, state and
  credentials. **Daemon generation** means a particular runtime incarnation.
- **Tenant** means a server-side authorization/data scope; **principal** is the
  authenticated actor. Neither is interchangeable with an instance or database role.
- **Database provider** means a registered driver such as `sift/postgres`.
  **AI provider** means an isolated agent runtime such as Codex, Claude Code or
  OpenCode. Say which kind when “provider” would be ambiguous.
- **Connection profile** is saved provider configuration and credential references.
  **Connection** is a live native handle with a generation; numeric IDs alone do
  not prove the same database source survived reconnect.
- **Room/document** means shared query text and its collaboration context.
  **Result** means observed database output; a result reference is not saved rows.
- **Operation** means a typed, audited user-visible action. **Receipt** records
  the scoped observation it actually proves, not an inferred native outcome.

## Three ways to damage a live session

1. **Killing by pattern.** Never `pkill -f`, `pgrep | kill`, or kill a PID selected
   only by a matching name/path/worktree string. Track subprocess PIDs at spawn.
   For a port owner, confirm its identity and `/proc/<pid>/cwd` before stopping it.
   Stop a known managed service only within the authorized task. Preserve other
   servers, database containers, agent processes and the user's open IDE.
2. **Testing against live state.** Never point a test/dev server at an installed
   instance's metadata, secret store or runtime directory. Read authorized data
   or make an isolated copy; do not migrate, clean or write live state for a test.
   Production changes require an authorized operation and a concrete backup,
   migration and recovery path. Preserve service-owned state symlinks, dynamic
   users, credential mounts and lifecycle hooks during upgrades.
3. **Assuming a source or outcome.** Do not reuse the current account/instance for
   queued work, infer an engine from legacy config, or infer commit from returned
   rows. Resolve configuration through its registered provider. Capture original
   actor, endpoint, tenant, profile, connection generation and reviewed material;
   recheck authority at admission and gate late delivery to the original caller.

## Check every applicable path

Before declaring a feature done, identify which entries apply:

- **Entry points:** explorer/context actions, editor commands, results, dialogs,
  palette and Vim bindings. Visible actions need reachable keyboard paths.
- **Consumers:** desktop, SDK, CLI and governed AI/MCP tools. Reuse server services;
  a human capability does not automatically grant an agent authority to use it.
- **Engines:** PostgreSQL, SQL Server, SQLite and negotiated external providers.
  Decide behavior or an explicit unsupported reason for each affected engine.
- **AI adapters:** Codex, Claude Code and OpenCode for provider-shaped behavior.
  Preserve isolation, captured callers, bounded broker access and original Stop owners.
- **Contracts:** wire types, server validation, SDK and consumers. Keep protocol
  compatibility explicit; no silent legacy fallback that bypasses a new guard.
- **Reverse states:** create/delete, connect/disconnect, approve/deny/revoke,
  start/cancel/inspect, enable/disable, failure/recovery. Show truthful terminal states.
- **Connection modes:** local in-process, daemon, container, hosted HTTP/WSS,
  SSH and tailnet. Include multi-instance, account change and stale-reply cases.
- **Real workflow:** connection ping is not proof of schema loading or result
  delivery. An HTTPS query needs its actual WebSocket/result path exercised.
- **Documentation:** change affected guidance or status only; do not update every
  document for a routine implementation change.

## Where code lives

- `crates/protocol` — pure serde wire types; no I/O, Tokio or OS APIs.
- `crates/api-types`, `crates/extension-protocol` — shared API/extension contracts.
- `crates/driver-api` — server-internal `Driver` and engine extension traits.
- `crates/driver-postgres`, `crates/driver-sqlserver`, `crates/driver-sqlite` — native adapters.
- `crates/server` — axum transports, sessions, rooms, policy and supervised native work.
- `crates/metadata` — SQLite/refinery and references to secrets held elsewhere.
- `crates/doc` — Loro abstraction for query text only.
- `crates/core`, `crates/semantic`, `crates/completion`, `crates/sql-variables`,
  `crates/snippets` — shared domain logic and SQL intelligence.
- `crates/client-sdk` — thin HTTP/WebSocket consumer.
- `crates/ai-runtime`, `crates/plugin-host` — isolated local AI runtime and supervised extensions.
- `crates/ui`, `crates/workspace-ui`, `crates/desktop` — GPUI components, workspace and native app.
- `crates/instance-config` — instance manifest/lock contracts and validation.
- `scripts`, `examples/reproducible-instance`, `flake.nix` — development and packaging tools.

## Architecture and taste

- The server owns behavior and authorization; transports decode, call the owning
  domain service and map errors. Keep business logic out of handlers and UI callbacks.
- Both PostgreSQL and SQL Server pass through `Driver`; preserve the ADR-017 trait
  lock. A signature change requires the documented protocol/version process.
- UI dependencies never enter shared crates. Local AI provider credentials never
  move into the server or metadata. Do not persist secret bytes in SQLite, logs or Git;
  store opaque handles through `SecretStore`.
- Every user-visible action has a typed `Operation` and audit coverage, including
  admission refusal and terminal settlement where the contract requires them.
- A wedged driver cannot freeze the server. Run queries in `tokio::spawn` with
  deadlines, cancel tokens and bounded native owners, never inline in handlers.
- CRDTs are for query text only, never results, schemas or sessions.
- Keep database provider/SQL dialect differences at explicit adapter boundaries.
  Preserve native identity, types, absent counters and unsupported cases.
- Keep TLS crypto selection and certificate trust explicit. A remote client must
  not depend on an unrelated local driver having initialized process-global state.
- Reuse existing UI components, icons and interaction patterns. Explain disabled
  operations without exposing implementation details or private diagnostics.
- Add comments for constraints, external quirks and non-obvious invariants.
  Do not narrate control flow or preserve a history of the fix in source comments.

## Development and resource limits

Use the commands and prerequisites in [DEVELOPMENT](docs/DEVELOPMENT.md) and
[LOCAL_DEV](docs/LOCAL_DEV.md). `direnv allow` enters the flake shell and loads
`.env`. Typical targets are `cargo run -p sift-server --bin sift-server`,
`cargo run -p sift-desktop` and `nix run .#sift-desktop-demo`; use disposable demo
state and understand which database the demo may reset before running it.

- Keep aggregate Rust/Cargo CPU and RAM use at the requested **10% host cap**.
  Use enforced resource limits and one Cargo job. `-j 1`, affinity or `nice` alone
  does not enforce both CPU and memory caps. Account for concurrent builds together.
- Do not raise the cap after OOM or run another compiler while one is still active.
  Change the build scope/settings deliberately and disclose artifact tradeoffs.
  A tool observation timeout does not mean the underlying build failed.
- Keep development state separate from installed instances; use explicit private
  roots and read actual endpoints from startup/configuration. Never guess URLs.
- Stop only what you started or an explicitly authorized service. Do not kill an
  existing process merely to free a port or a Cargo lock.
- On Windows use `scripts/dev.ps1 setup`, `env`, `server` and `desktop` as documented.
  Current native Windows/macOS acceptance is deferred until those hosts are available.

## Test data and secrets

- Seed isolated fixtures with representative schemas, nonempty rows, native types
  and the relevant permission boundaries. An empty grid is not a query-delivery test.
- For authorized live-state copies, use SQLite's backup API or a consistent cold
  backup; copying only a live `.sqlite` file can omit WAL state. Copy, never symlink.
- Before starting a cloned server, disable schedules, pending work, external
  integrations and credential use that could affect real systems. Copy only the
  secrets needed by the test. Data flows into the sandbox, never back into live state.
- Live engine tests use disposable databases. Do not run mutating suites against
  a user's saved connection or start a seeded demo against their real database.
- Root `.env` is the single source of truth for local runtime variables; it is
  gitignored. Document variables in `.env.example`. Normal direnv entry loads it;
  if direnv is unavailable, `set -a; source .env; set +a` is the documented fallback.
- `SIFT_METADATA__SECRET_KEY_FILE` is generated by the dev-shell key script under
  `examples/reproducible-instance/scripts`; do not hand-set it in `.env`.
- `SIFT_MSSQL_PASSWORD` is generated/managed by `dev-mssql.sh` in that directory
  (also `nix run .#dev-mssql`). Never manually change the container password or
  its `.env` value independently.
- `SIFT_PG_PASSWORD` is normally unset for the socket-trust demo.
  `SIFT_AUTH__BEARER_TOKEN` is optional for local bearer-auth tests.
- User connection credentials are runtime `SecretStore` data, separate from `.env`.
  Never print tokens, passwords, key bytes, connection strings containing secrets
  or credential-bearing startup URLs. Rotate at the source if a secret enters Git.

## Verification

- Keep formatting, Clippy and tests green. Iterate with focused crate/test targets,
  such as `cargo test -p sift-client-sdk <test-name>` or the relevant driver suite.
- Do not run repo-wide checks by default. Run `cargo fmt --all`,
  `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`
  when requested or required by an explicitly agreed milestone/release gate.
  CI remains manual-dispatch-only; report unrun gates rather than assuming CI runs them.
- Test behavior that can break: native SQL/type translation, policy, migrations,
  persistence, ownership, cancellation, protocol parsing, source changes and races.
  Extend an existing case at the lowest layer that reproduces the bug before
  adding a new test. Logic/data/protocol fixes should fail before the fix.
- Avoid tests for types, trivial forwarding, copy text, static markup, callback
  wiring and mock-call mirrors. Do not add smoke scripts or workflows unless asked.
- Visual-only changes use before/after captures. Behavior changes use focused
  regressions; a user-visible workflow also gets one integrated pass in a real client
  where the host is available. Do not claim native/accelerated acceptance from Xvfb alone.
- Synchronize concurrent tests with channels, barriers, notifications or persisted
  milestones. Avoid sleep/poll/retry-based correctness assertions and wall-clock
  timing checks. Use controlled clocks where possible; real subprocess/network
  fixtures still need bounded deadlines and explicit cleanup.
- Use existing deterministic provider fixtures before spending real model credits.
  Run a real AI provider when the change depends on its behavior, with explicit
  isolation and the smallest adequate model. Provider quota errors are blockers,
  not passing native acceptance.
- Documentation-only changes need link/status/whitespace checks, not Rust rebuilds.

## Documentation and work tracking

- Use [TODO](docs/TODO.md), [product coverage](docs/PLANS/ide-parity-and-provider-extensibility.md)
  and the affected existing contract for current work. Mark only accepted scope
  done; preserve genuine gaps, deferred hosts and native evidence limits.
- Most code changes need no new internal page. Document durable cross-component
  decisions, constraints and traps a maintainer cannot readily infer from code.
  Put local invariants in short nearby comments instead.
- Rewrite or remove guidance made obsolete by a change. Do not append another
  implementation account, per-PR narrative, repeated test-count log or file catalog.
- Keep user guidance task-oriented and in the product's voice. Keep setup, release
  and debugging procedures separate from user workflows; link existing guides.
- Existing `docs/PLANS` files retain active work or normative contracts. Do not add
  implementation plans, research notes or agent scratch files unless requested;
  keep temporary material outside the worktree. Do not create a second backlog.
- Design precedes implementation for tightly coupled work. Graduate stable
  cross-layer decisions into `docs/DECISIONS.md`; keep proposed work proposed.
- `CLAUDE.md` is a symlink to this file. Edit `AGENTS.md` only; preserve that link.

## Commits and pull requests

- Use conventional commit titles and plain language. Describe the problem,
  resulting behavior, relevant validation and material limits.
- Commit coherent verified milestones when requested. Never commit secrets,
  temporary diagnostics or PR-only screenshots/assets.
- Do not create a PR or push unless explicitly asked. Do not enable automatic CI.
- If a PR is requested, include useful UI captures or a short timing/motion video
  when relevant. Follow the repository template; keep the description about the
  final implementation rather than the conversation or abandoned approaches.
