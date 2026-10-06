# MCP and AI backend work

Status reconciled against ADRs 096–107 and the implementation on 2026-10-06.
The canonical product inventory is
[IDE parity and provider extensibility](PLANS/ide-parity-and-provider-extensibility.md).
The detailed contract and acceptance evidence live in
[AI agent integration](PLANS/ai-agent-integration.md).

## Implemented

- [x] `sift mcp` exposes the server-governed tool registry over bounded stdio
      JSON-RPC, with explicit server/token/context arguments. Authorization,
      operation classification and auditing stay on the server.
- [x] Codex, Claude Code and OpenCode adapters use existing local CLI sign-in,
      restricted Linux launches and Sift-only tools. Live isolated tool
      roundtrips passed for all three providers.
- [x] The shared harness calls the SDK directly, bounds history/input/output,
      preserves invocation identity, shares read/proposal quotas and enforces
      absolute deadlines, cancellation and terminal receipts.
- [x] Query and workspace context includes frozen execution provenance,
      SQL/selection/statement ranges, revision-bound diagnostics, errors and
      source-matched transaction/read-only hints. Explicit rows/history/plans
      use server-resolved, digest-bound attachment previews.
- [x] Driver context uses the managed tenant/profile/connection, provider and
      dialect. Schema/catalog, native object DDL, estimated explain, scoped
      query history and saved/analyzed-plan reads use the governed gateway.
- [x] Multiplayer backend supports room-public history, ordered run/tool/proposal
      events, future-member visibility, explicit resource publication and
      separate source-owner/room-owner external MCP grants.
- [x] Reviewed external MCP inventory/reads and local SQL/row/schema intent
      adapters retain encrypted provenance. Remote writes are never invoked;
      ordinary human review/apply remains mandatory.
- [x] Retention, resumable key rotation and encrypted full/tenant recovery
      preserve authorization boundaries and interrupt imported live turns.

## Acceptance closeout

- [x] Verify Claude's live isolated Sift-tool roundtrip with current native sign-in
      (Claude Code 2.1.291, 2026-10-06).
- [x] Complete combined real PostgreSQL, SQL Server and SQLite read/propose/apply,
      freshness, revocation, cancellation and replay acceptance, plus desktop
      completion/disconnection settlement. Retain existing publication/recovery
      regression coverage and run the workspace checks.

## Separate future scope

- [ ] Add governed query-performance measurement/comparison tools. Existing
      `performance.rs` contracts and API support do not automatically expose
      measurements to agents.
- [ ] Extend CLI convenience commands for direct API workflows where useful;
      reuse existing SDK/governance rather than a second authorization path.
- [ ] Design Execute mode and unattended run supervision before implementation.
- [ ] Design and validate Windows/macOS native provider isolation.

UI work is tracked separately. The old unchecked provider/context/harness notes
were an initial plumbing sketch, not evidence that those implementations were
missing.
