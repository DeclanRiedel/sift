# Query performance workbench

Status: serial benchmarks, a PostgreSQL measured-plan Profile foundation,
configurable Performance panel and private saved-run library implemented;
broader profiling, definitions and advanced tools remain in progress.

## Design contract

- Explain estimates a plan; Profile executes with instrumentation; Benchmark
  measures repeated ordinary executions. Never combine their timing populations.
- Keep Explain separate. Add Performance to the results panel, opened explicitly
  by Profile/Benchmark. New query tabs remain editor-only. Large comparisons may
  expand into a workspace tab. All controls support Vim and the command palette.
- Freeze SQL, parameters and run configuration before execution. Never silently
  append LIMIT. Record timing boundaries and result-consumption policy.
- Use dedicated connections, bounded jobs, cancellation and existing audited
  Operations. Preserve the Driver trait contract. No UI dependencies in core.
- Start with serial read-only workloads and engine-enforced restrictions where
  available. SELECT classification alone is not protection against side effects.
  Production approval covers the entire workload. Rollback is not a sandbox.
- Missing counters are unavailable, not zero. Planner costs are engine-relative.
  Cache state is unknown unless independently established; reconnect is not cold
  cache. Never clear shared caches or change database settings automatically.
- Benchmark report version 2 records separate optional nanosecond clocks: native
  database execution (only when independently observed), server-side client
  elapsed (before driver.execute through task completion), first row received,
  and full stream consumption (when Done reaches the server). Legacy elapsed_ns
  remains the client-elapsed summary and display source when the optional typed
  alias is absent. Version 1 saved reports decode with new dimensions absent;
  no old duration is relabelled as database execution.
- Persist definitions separately from immutable runs. SQL, plans and parameters
  can contain sensitive data: private by default, explicit sharing/redaction,
  secret handles only. Do not store result rows by default.

## Milestone 1 — contracts and measurement foundations

- [x] Record design and full implementation checklist.
- [x] Pure measurement model: warm-up/measured samples and explicit outcomes.
- [x] Deterministic summaries excluding warm-ups and unsuccessful samples;
  retain outcome counts and flag insufficient samples for tail percentiles.
- [x] Timing dimensions: database execution, server-side client elapsed, first
  row, full consumption; unavailable dimensions remain optional and separate.
- [x] Validated serial-run iteration, warm-up, timeout, delay and total budgets.
- [x] Freeze SQL, parameters and configuration for a serial benchmark.
- [ ] Full profiling capability matrix and captured environment context.
- [x] Versioned benchmark report and audited Benchmark/cancel API actions.
- [x] Dedicated audited save/list/get/delete snapshot actions.
- [x] Dedicated audited PostgreSQL Profile and cancel actions.

## Milestone 2 — profile and Performance panel

### PostgreSQL Profile foundation design

Profile is a separate audited operation and result population. It accepts one
frozen SQL statement, bind values, a run ID, timeout, and explicit workload
confirmation. The server admits only a classifiable read query and requires
both Profile and ordinary query permission. It opens a dedicated connection,
starts a read-only transaction, and executes `EXPLAIN (ANALYZE, BUFFERS, FORMAT
JSON)` there. The read-only transaction is a database-enforced DML boundary;
external functions may still have effects, so confirmation remains required.
The editor's connection and transaction are never used for instrumentation.

A supervised task owns the dedicated connection until rollback and close. A
per-source run registration supports explicit cancellation; timeout and
cancellation interrupt execution, attempt driver cancellation, and discard the
connection. Text plan payloads are capped before JSON decoding, and all plans
are capped before normalized-tree parsing. The response
keeps raw JSON and a normalized tree, and labels PostgreSQL planning/execution
durations separately from Sift's server-observed elapsed time. Missing native
counters remain unavailable. The Performance panel presents this measured plan
apart from estimated Explain and serial Benchmark results.

### SQL Server measured-plan design

SQL Server Profile uses the same audited request, run registration, dedicated
connection, supervisor, timeout and cancel endpoint. It requires both Profile
and Execute permission, a classifiable single read query, and explicit workload
confirmation. SQL Server has no read-only transaction equivalent for this path;
the UI must say that the login should have read-only database permissions and
that read classification does not contain side effects of called functions.
The database separately enforces `SHOWPLAN` on every referenced database for
`SET STATISTICS XML` output. Refusals remain errors, never empty plans.

The dedicated TDS session enables `STATISTICS XML` before the read and disables
it after completion. It never changes the editor session. The driver streams
and discards ordinary result rows, counting rows and estimated encoded bytes;
if either cap is reached, it cancels and discards the dedicated connection.
Only one bounded Showplan XML document is accepted and parsed. Timeout or
explicit cancellation aborts the driver task, invokes its abort-and-discard
cancel path when a cursor exists, then closes the dedicated connection. A
failed OFF/reset also discards that connection. The server labels wall-clock
duration as Sift-observed and extracts SQL Server `QueryTimeStats` timing only
when present. Per-node runtime and IO counters come from Showplan XML.
`SET STATISTICS IO/TIME` textual messages are not exposed by the current TDS
driver and remain unsupported rather than guessed or summed from nested nodes.
See Microsoft's [`SET STATISTICS XML` permission and output contract](https://learn.microsoft.com/en-us/sql/t-sql/statements/set-statistics-xml-transact-sql?view=sql-server-ver17).

- [~] Current statement/selection targeting uses the query tab's target; an
  explicit SQL and parameter preview before execution remains open.
- [ ] Summary, Runs, Plan, Compare and Saved sections; keyboard navigation.
- [x] Initial Performance tab: summary, virtualized samples, in-memory baseline,
  JSON clipboard export and keyboard controls. Open through the command palette
  (`Query Performance: Open Benchmark Panel`) without executing anything.
- [x] PostgreSQL JSON actual plan and available per-node runtime/buffer counters;
  unavailable counters remain absent.
- [x] SQL Server actual plans use a supervised dedicated connection, bounded
  result drain, timeout/cancel and Showplan XML runtime/IO counters. Native
  `QueryTimeStats` elapsed is shown when present; textual `SET STATISTICS
  IO/TIME` messages remain unavailable through the current TDS driver. Live
  parameterized capture, oversized-result refusal, and restricted-login
  `SHOWPLAN` grant/revoke acceptance pass.
- [~] SQLite estimated plan and bounded measured-read timing/row count use the
      existing Profile path. Native per-node runtime counters are still
      unavailable through the locked Driver trait; the API and UI leave those
      fields absent rather than synthesizing them.

### SQLite measured-read profile design

Use the existing audited Profile operation and dedicated connection. For SQLite,
capture `EXPLAIN QUERY PLAN` as an **estimated** plan, then execute exactly one
validated read in a read-only transaction on the same disposable connection.
Fully drain or refuse the result at the profile's row/byte limits. Report
server-observed dispatch-to-consumption time and completed row count separately
from the estimated plan. Native per-node rows, cost, execution clocks, and
statement-status counters remain unavailable: the `Driver` trait exposes none
of them, and estimated plan nodes must not be relabelled as actual. Reuse the
Profile timeout, cancel token, per-query resource reservation, permission
recheck, source disconnect cancellation, and bounded connection cleanup. The
desktop's existing Profile action labels SQLite evidence explicitly. Real-file
HTTP tests must prove a successful parameterized read, a write refusal, and a
bounded-result refusal without changing the source database.
- [~] Raw JSON and a normalized plan tree are available; explicit
  estimate/actual deltas and node links remain open.
- [ ] Evidence-based scans/sorts/spills findings without double-counting nested
  node time; instrumentation overhead labelled.
- [ ] Throttled background progress, virtualized samples, bounded memory;
  typing remains responsive. Preserve existing Explain access.

## Milestone 3 — serial benchmark runner

- [x] Dedicated connections, single-read SQL validation and per-iteration policy
  checks. PostgreSQL/SQLite use read-only transactions. SQL Server explicitly
  requires a read-only account for database-enforced protection; it has no
  read-only transaction mode. Neither mechanism sandboxes external functions.
- [x] Preset: two warm-ups, ten measured runs, one connection. UI cycles 1/10/100
  measured runs; API exposes all validated limits.
- [x] UI editors for measured runs, warm-ups, timeouts, total budget and
  inter-query delay. Shared validation with the server; frozen settings per run.
- [x] Per-statement timeout, sampling deadline, delay, cancellation and partial
  reports. Setup/cleanup are separately bounded and may outlast sampling budget.
- [x] Fully drain without retaining result rows. Server-observed execution/drain
  and first-row timings are labelled separately from database/desktop timings.
- [ ] Optional fetch/render measurements and instrumented profile populations.
- [ ] Capture preparation mode, reuse, isolation, session settings, engine
  version and observed/unknown cache conditions.
- [x] Dedicated connection cleanup, source-disconnect cancellation and explicit
  whole-workload confirmation. Cancellation stays bound to the original profile;
  failure to confirm cancellation is surfaced rather than silently ignored.
- [x] All samples visible, median/mean/range/sample deviation; insufficient tail
  percentiles remain unavailable. Unfinished row counts are unknown, not zero.

## Milestone 4 — save and compare

- [x] Immutable private saved-run snapshots, owner-scoped paginated browser,
  reopen/copy, baseline reuse and confirmed deletion. Names, SQL and samples are
  stored through SecretStore; SQLite holds only identifiers and opaque handles.
- [x] Saved library command works independently of open query tabs and database
  connections. Missing secret payloads remain browsable and deletable.
- [ ] Metadata migrations for reusable definitions and attached profiling plans.
- [ ] Notes, tags, workspace/Git context and editable retention policy.
- [ ] Definition browser and validated parameter-aware rerun connection.
- [~] Saved A/B comparison: pin a private saved run as A, open another as B,
  and inspect absolute/relative client-median deltas plus variability. A
  repeatable variant runner and controlled before/after capture remain open.
- [x] In-memory baseline pinning and observed median delta; incomplete runs,
  different engines and selected configuration mismatches suppress comparison.
- [ ] Alternating/randomized A/B order; record ordering and seed.
- [x] Saved A/B view shows absolute/relative median deltas, each run's sample
  deviation, and an explicitly inconclusive verdict for uncontrolled conditions.
- [ ] Compatibility warnings for parameters, data, versions and settings.
- [ ] Plan changes and optional untimed result-equivalence checks with explicit
  ordering, duplicate, floating-point and nondeterminism rules.
- [ ] Versioned JSON import/export, CSV samples and readable reports; bounded
  import validation, redaction and permission-checked sharing/deletion.

## Milestone 5 — advanced tools

- [ ] Parameter matrices, multi-query suites and per-query baselines.
- [ ] Absolute and relative regression thresholds with uncertainty handling.
- [ ] Headless CLI/API runner and user-invoked regression reports; no CI changes.
- [ ] Explicit concurrency/load mode: latency, throughput, errors, saturation,
  rate limits and bounded connection budgets.
- [ ] Isolated test-database setup/teardown and index experiments.
- [ ] Explicit advanced write benchmarking; disclose non-rollback side effects.
- [ ] Optional historical server statistics, separate from controlled runs.

## Verification and handoff

- [ ] Focused tests for sampling, cancellation/cleanup, authorization, persistence,
  comparison compatibility and keyboard/UI state; no new smoke scaffolding.
- [ ] Run formatting, strict workspace Clippy and workspace tests at milestones.
- [ ] Commit verified milestones and update this checklist honestly.
- [ ] Graduate stable cross-layer decisions into docs/DECISIONS.md.

## Implementation log

- PostgreSQL Profile foundation: `ProfileQuery` and `CancelProfile` are separate
  audited operations and SDK calls. The server requires ordinary query and
  Profile authorization, a confirmed single read, and a PostgreSQL connection.
  Each run owns a dedicated connection and read-only transaction; `SET LOCAL`
  applies the validated statement timeout before `EXPLAIN (ANALYZE, BUFFERS,
  FORMAT JSON)`. The plan stream is bounded by the result byte limit, and a
  supervisor handles cancellation, timeout, rollback and connection cleanup.
  The response separates PostgreSQL planning/execution milliseconds from Sift's
  plan-capture elapsed nanoseconds. The Performance panel has a distinct
  measured-plan section, a Profile action and cancellation; bind values use
  the query tab's remembered parameters. UI confirmation is the Profile button,
  and SQL/parameter preview remains open. The read-only transaction protects
  database writes, but external function side effects and cache state remain
  unknown. Focused server tests cover the measured response, write rejection,
  cancellation and source-connection survival. Verification: formatting,
  server/SDK and desktop/workspace UI checks, and the two focused Profile
  integration tests pass; strict workspace Clippy and full tests await the
  integration gate.

- Timing dimensions milestone: report version 2 records server-side client
  elapsed at task completion, first nonempty row page, and full consumption at
  the Done page as distinct boundaries. Database execution stays unavailable:
  no driver exposes a comparable native measurement. Existing elapsed_ns and
  summary fields remain client-elapsed clocks; version 1 saved reports decode
  with new dimensions absent. Saved-report validation rejects contradictory
  version 2 boundaries, and both Performance views label each dimension.
  Server tests cover success/timeout boundaries and version 1/2 persistence.

- Initial core foundation: `crates/core/src/performance.rs` provides serial-run
  budget validation and single-dimension timing summaries. Four focused tests
  cover rejected budgets, warm-up/outcome exclusion, missing/zero/large timings,
  variance and nearest-rank percentiles. p95 requires 100 timed successes and
  p99 requires 1,000; these display floors do not promise statistical confidence.
  No execution endpoint, persistence or Performance UI is wired yet.
  Verification: formatting check, strict workspace Clippy and workspace tests
  passed for this foundation (including all 29 core and 472 editor tests).

- Serial benchmark implementation: POST `.../connections/:id/benchmark` accepts
  a frozen `BenchmarkRequest`; POST `.../benchmark/:run_id/cancel` requests stop.
  Client SDK methods expose both. The server owns a dedicated reused connection,
  drains every row page without the grid retention cap, and returns versioned
  samples and summary. Result export includes SQL but omits bind values; the UI
  labels this explicitly. No automatic cache clearing, query rewriting or DML.
  Performance controls: `r` confirms/runs, Escape requests cancellation, `i`
  cycles measured iterations, `b` pins a baseline and `y` copies JSON.
  Reports and pinned baselines are currently transient, not a saved-run library.
  Backend milestone: `0f043a9`. Verification: formatting, strict workspace Clippy
  and workspace tests pass, including 473 UI/editor tests. Focused regressions
  cover draining beyond the grid limit, timeout/partial reports, cancellation
  cleanup, SQL write/batch rejection and stale UI completion responses.

- Configuration milestone: Performance now has editable warm-ups, measured
  iterations, per-query timeout, sampling budget and inter-query delay. `c`
  focuses configuration; Tab/Shift-Tab navigates fields and returns to panel
  controls. Existing `i` presets remain available. Inputs use the same pure
  `BenchmarkLimits` validation as the server, and invalid settings cannot start
  a run. Run count confirmation includes warm-ups; active runs freeze settings.
  Summary exposes p95/p99 availability, and baseline comparisons also reject
  differing iteration counts or total budgets. Saved runs, profiling and the
  remaining advanced workbench checklist are still outstanding.
  Verification: formatting, workspace check, strict workspace Clippy and the
  full workspace test suite pass (474 UI/editor tests). Regressions cover
  malformed/overflowing numbers, shared budget limits and invalid-run blocking.

- Saved-run library milestone: V047 indexes private immutable benchmark snapshots.
  POST/GET `.../metadata/tenants/:tenant/benchmark-runs` save and keyset-page runs;
  GET/DELETE `.../benchmark-runs/:id` retrieve/delete owner-only snapshots. Save
  recomputes summaries from bounded, validated samples. These are user-submitted
  snapshots, not server attestations or rerunnable definitions; bind values are
  absent. Limits: 4 MiB per snapshot, 500 snapshots / 64 MiB per tenant-owner,
  no automatic expiry or eviction. Save writes the secret before indexing it and
  compensates on rejected inserts. Delete removes secret bytes before the index;
  an interrupted deletion can leave an unavailable index entry, which can be
  deleted again. Backup/recovery must include both metadata and the secret store;
  tenant restore rebinds benchmark secret handles. Backend crashes during save
  can leave unindexed secret entries; automatic orphan collection is not added.
  UI: `s` in Performance opens the private save form; command palette
  `Query Performance: Browse Saved Runs` opens the independent browser. `j/k`
  selects, Enter opens, `r` refreshes, `n` pages, `c` edits the save name, `s`
  saves, `b` reuses the opened run as a query baseline, and `y` copies JSON
  (including SQL). `d` requests deletion; Enter confirms; Escape cancels.
  Definitions, parameter-aware reruns, sharing, tags/notes and instrumented plans
  remain separate unchecked work.
  Verification: formatting, workspace check, strict workspace Clippy and full
  workspace tests pass (475 UI/editor tests, 78 API tests). New regressions cover
  encrypted persistence/reopen, owner isolation, pagination, missing payloads,
  duplicate saves, normalized summaries and keyboard deletion confirmation.
  An initial full run hit the existing SQLite close/reopen PoolExhausted test;
  it passed unchanged in isolation and on both subsequent workspace runs.
  Backup fixtures were updated for schema V047; no driver code was changed.
