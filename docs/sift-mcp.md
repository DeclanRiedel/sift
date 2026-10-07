# MCP and AI backend work

Status reconciled against ADRs 096–108 and the implementation on 2026-10-07.
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

## Performance and CLI milestone (2026-10-07)

- [x] Governed performance measurement inspection/comparison tools (ADR-108):
      private saved-run list/statistics/comparison with current ownership, bounded
      output and recomputed sample statistics; no agent workload execution.
- [x] CLI convenience commands use existing SDK/governance for tool list/call,
      managed queries, explicitly confirmed Benchmark/Profile workloads and
      private benchmark list/get/compare.

## Separate future scope

- [ ] Design Execute mode and unattended run supervision before implementation.
- [ ] Design and validate Windows/macOS native provider isolation.

UI work is tracked separately. The old unchecked provider/context/harness notes
were an initial plumbing sketch, not evidence that those implementations were
missing.

## Performance tools and CLI usage

AI tools and standalone `sift mcp` expose `sift_benchmark_runs`,
`sift_benchmark_run` (`run_id`) and `sift_benchmark_compare` (`baseline_id`,
`candidate_id`). Measurements are private user-saved tenant snapshots, without
profile provenance or server attestation. Room-public contexts cannot access
them. Inspection omits SQL, binds and individual samples; comparison uses
successful measured client-elapsed samples, excludes warmups and failed samples,
retains outcome counts and withholds deltas for incomplete/incompatible runs.
Differences are descriptive; data/schema/cache/load equivalence is not established.
Agents cannot launch Profile or Benchmark or claim their own workload approval.

CLI usage: `sift tools --help`, `sift query --help`, `sift performance --help`.
Every client command requires an explicit `--server` URL and protected
`--token-file`. Query and measured execution require `--tenant-id` and
`--profile-id`; the CLI opens a managed session and closes it after completion,
failure or Ctrl-C. Tool calls preserve approval-required responses; approval is
still granted through the authenticated Sift client. Request/SQL files are
bounded to 1 MiB. Native provider tools stay unavailable inside AI isolation.

| Command | Input / behavior |
| --- | --- |
| `sift tools list` | Optional `--mcp-only`; authorized extension registry |
| `sift tools call` | `--tool`, `--arguments-file`, optional existing `--approval-id` |
| `sift query` | `--sql-file`, optional `--params-file` JSON array; ordinary audited execution |
| `sift performance benchmark` | `--request-file`, `--confirm-workload`, optional `--save-name` |
| `sift performance profile` | `--request-file`, `--confirm-workload`; dedicated measured profile |
| `sift performance runs` | `--tenant-id`, optional `--cursor`; private saved runs |
| `sift performance get` | `--tenant-id`, `--run-id`; private snapshot JSON includes SQL |
| `sift performance compare` | `--tenant-id`, `--baseline-id`, `--candidate-id`; shared comparison logic |

Benchmark request example (add explicit CLI confirmation; JSON cannot grant it):

```json
{"sql":"SELECT 1","warmups":1,"iterations":5,"query_timeout_ms":1000,"total_budget_ms":10000,"delay_ms":0}
```

Profile request example:

```json
{"sql":"SELECT 1","timeout_ms":1000}
```

Both accept optional typed `params` and `run_id`. Profile connection identity is
bound to the managed connection opened by the CLI. Benchmarks never save bind
values. Existing server permissions, read admission, resource budgets and
engine-specific protections apply unchanged; SQL Server still requires a
suitably restricted database account.


Acceptance: core sample accounting, real PostgreSQL/SQL Server/SQLite benchmark
snapshots and governed AI reads/comparison, ownership/malformed-input rejection,
public-context denial, CLI bounded-input/explicit-confirmation checks, denied
request cleanup and end-to-end SQLite query/benchmark-save/MCP comparison/Profile
passed. Workspace `cargo fmt`, strict `cargo clippy --workspace --all-targets -- -D warnings`
and `cargo test --workspace` passed: 1,609 tests passed, 5 ignored. No new smoke scripts or CI workflows were introduced.
