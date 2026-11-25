# Sift

Sift is a native, Vim-first database IDE. The Rust server owns database behavior;
the GPUI desktop, SDK, CLI and governed tools use its HTTP/WebSocket API.

Read [README](README.md) first. Choose work from [TODO](docs/TODO.md), consult
[scoped plans](docs/PLANS/README.md), and preserve [ADRs](docs/DECISIONS.md).

## Product constraints

- Vim is the only supported IDE/editor mode. Ignore Standard mode.
- Keep UI work bounded: streaming backpressure, virtualized lists and no
  whole-catalog work per keystroke or continuously repainting animations.
- Local, hosted, SSH and tailnet paths matter. The server host owns database
  reachability and tunnels; joining the desktop to a tailnet does not join its server.
  Use configured origins and preserve TLS certificate/SSH host-key verification.
- `sift-protocol` stays pure serde: no I/O, Tokio or OS APIs. CRDTs hold query
  text only, never results, schemas or sessions.
- Every user-visible action has a typed `Operation` and the required admission/
  terminal audit coverage. Share server behavior across transports and clients.
- Run queries in `tokio::spawn` with deadlines, cancel tokens and bounded native
  owners. A wedged driver must not freeze the server. Timeout, disconnect and RPC
  acknowledgement do not prove rollback or commit; never auto-retry uncertain writes.
- Preserve read-only policy, production confirmation and exact approvals. Recheck
  authority at native admission; hiding a UI control is not authorization.
- Keep TLS crypto selection explicit. Remote clients must not depend on an
  unrelated local driver initializing process-global state.

## Taste

- Prefer the smallest model that makes correct behavior obvious. Remove obsolete
  complexity within scope; do not add machinery for a single use.
- Shared GPUI controls own their appearance. Add reusable variants; keep feature
  styling local and layout on the owning container.
- Derive cheap state; cache expensive projections with source/revision invalidation.
  Frozen approvals and pending requests retain their original source, not the
  currently selected connection. Native leases/settlement may outlive the request.
- A lying spinner or stale connection label is a defect. Show unavailable and
  unknown outcomes honestly rather than inventing success to clear pending state.
- Trace operation boundaries with stable names. Avoid per-cell/row/chunk spans
  and notifications unless measurement justifies the cost.
- Comment only constraints, external quirks or invariants the code cannot show.
  Keep it short; link cross-component reasoning to the existing contract or ADR.
- Honor authorized maintainer intent. Flag real data/authority/contract conflicts;
  do not invent approval gates for already authorized work.
- Do not generate new icons if it does not exist in Qlementine Icons and avoid using icons where not needed for UI.

## Check applicable paths

- **Entry points:** explorer, editor, results, dialogs, palette and Vim bindings.
  Cover reverse states such as cancel, revoke, disconnect and recovery.
- **Consumers:** desktop, SDK, CLI and governed AI/MCP. A human capability does
  not automatically grant agent authority.
- **Contracts:** server validation, wire types, SDK and consumers. No legacy
  fallback that bypasses a new guard.
- **Modes:** in-process, daemon/container, hosted HTTP/WSS, SSH and tailnet;
  include instance/account changes and late replies.
- **Actual workflow:** ping is not schema/result verification. Exercise HTTPS
  WebSocket delivery when changing hosted query behavior.

## Development and live-state safety

Setup and commands: [DEVELOPMENT](docs/DEVELOPMENT.md), [LOCAL_DEV](docs/LOCAL_DEV.md).

- Use private dev roots and actual configured/startup endpoints. Stop only work
  you started or an authorized service; do not kill processes to free ports/Cargo locks.
- No broad-pattern mutations: never use `pkill -f`, `pgrep | kill`, wildcard/
  recursive cleanup or bulk migrations selected only by matching Sift names/paths.
  Use captured spawn PIDs or verified exact service instances. Before data cleanup
  or migration, resolve targets and prove they stay inside the explicitly selected
  disposable dev root, rejecting symlink escapes. Live/installed state requires
  explicit user authorization for that concrete target; dev/test cleanup never includes it.
- Tests and seeded demos use disposable databases, never installed instance state
  or a user's saved connection. Seed representative schemas, nonempty rows and
  permission boundaries.
- Authorized state copies use SQLite backup or a consistent cold backup, including
  WAL state. Copy, never symlink. Disable schedules, pending work and external
  integrations before starting a clone; copy only needed secrets, never back to live state.
- `.env` is the gitignored local runtime source of truth; document variables in
  `.env.example`. `direnv allow` loads it; use the documented fallback if unavailable.
- Dev scripts generate the metadata key and manage the MSSQL password. Do not
  hand-set `SIFT_METADATA__SECRET_KEY_FILE` or change the container password and
  `.env` independently; see the setup guides.
- User credentials belong in `SecretStore`, separate from `.env` and SQLite.
  Never log/commit secret bytes or credential-bearing URLs. 

## Verification

- Iterate with focused crate/test targets. Extend the lowest existing case that
  reproduces a logic/data/protocol bug; establish failure before fixing it.
- No repo-wide sweeps by default. Run `cargo fmt --all`,
  `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`
  only when requested or required by an agreed milestone/release gate.
  General CI stays manual-dispatch-only; desktop development releases run on
  pushes to `master` by explicit maintainer choice. Report unrun gates.
- Test risky behavior: native translation, authorization, persistence/migrations,
  cancellation, parsing, source changes and races. Skip trivial forwarding,
  type guarantees, copy text and mock-call mirrors. No new smoke scripts/workflows unless asked.
- Visual-only fixes use before/after captures; behavior changes need focused
  regressions. Give affected UI workflows one integrated client pass where possible.
  Xvfb evidence does not establish native/accelerated acceptance.
- Synchronize tests with channels, barriers or persisted milestones, not sleeps,
  polling or retries. Prefer controlled clocks; real network/process fixtures need
  bounded deadlines and cleanup. Reserve timing assertions for measured performance work.
- Prefer deterministic provider fixtures. Spend real model credits only for
  provider-specific behavior, using isolation and the smallest adequate model.
  Quota errors are blockers, not passing acceptance.
- Documentation-only edits need link/status/whitespace checks, not Rust rebuilds.

## Documentation and work tracking

- Keep TODO, product coverage and the affected contract consistent at accepted
  milestones. Mark only the proven scope done; preserve genuine gaps and evidence limits.
- Document durable cross-component reasoning the code cannot show. Rewrite or
  delete obsolete guidance; do not append PR narratives, test-count logs or file catalogs.
  Most code changes need no new page and no project-wide doc sweep.
- Keep user guidance task-oriented; keep contributor setup/release/debugging
  procedures separate. Preserve third-party attribution.
- Existing plans carry active work or normative contracts. No new plans, research
  notes, scratch files or second backlog unless requested; temporary work stays outside the tree.
- Design precedes implementation for coupled work. Graduate stable cross-layer
  decisions into ADRs; leave proposed work proposed.
