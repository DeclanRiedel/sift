# Query performance workbench

Status: serial benchmarks, a PostgreSQL measured-plan Profile foundation,
configurable Performance panel, live job progress and private saved-run library implemented;
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
- [x] Profiling capability matrix and captured environment context. The matrix
  below distinguishes native evidence per engine. Benchmark and Profile
  responses include bounded server version when `ping` succeeds, explicit
  preparation/instrumentation and isolation policies, uninspected session
  defaults, and unknown cache state. Selected actual database session settings are now captured before workload
  setup (ADR-128); broader environment coverage remains in milestone 3.
- [x] Versioned benchmark report and audited Benchmark/cancel API actions.
- [x] Dedicated audited save/list/get/delete snapshot actions.
- [x] Dedicated audited PostgreSQL Profile and cancel actions.

## Milestone 2 — profile and Performance panel

### Current capability matrix

| Evidence | PostgreSQL | SQL Server | SQLite |
| --- | --- | --- | --- |
| Estimated plan | JSON `EXPLAIN` | Showplan XML | `EXPLAIN QUERY PLAN` |
| Measured Profile | `EXPLAIN ANALYZE` | `STATISTICS XML` | Bounded ordinary read beside estimated plan |
| Native duration | Planning and execution | `QueryTimeStats` when present | Unavailable |
| Per-node actual rows/time | Available from instrumented plan | Available when emitted by Showplan XML | Unavailable |
| Native buffer/IO evidence | Buffer counters when emitted | Runtime IO counters when emitted | Unavailable |
| Read-only enforcement | Dedicated read-only transaction | Read-only login required | Dedicated read-only transaction |
| Benchmark clock | Sift client elapsed, first row, full consumption | Same | Same |
| Benchmark environment | Server version if ping succeeds; selected session settings observed before workload setup; preparation defaults and unknown cache | Same | Same |
| Profile environment | Dedicated `EXPLAIN ANALYZE` read-only transaction; version if ping succeeds | Dedicated `STATISTICS XML` connection; version if ping succeeds | Dedicated estimated-plan plus read-only measured read; version if ping succeeds |

Native duration and plan counters belong to Profile. Benchmark's native database
execution clock remains unavailable for all three drivers. A successful `ping`
is not evidence of cache temperature or actual session settings. Separate bounded
observations now capture nine selected settings on the dedicated connection
before workload setup; later applied transaction/timeout policies remain separate. SQL Server
textual `STATISTICS IO/TIME` remains unavailable through the TDS driver.

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

- [x] Current statement/selection targeting uses the query tab's target.
  Benchmark and Profile now freeze SQL, bound parameters, connection identity
  and run settings in a review dialog before dispatch. Confirmation rejects a
  changed tab, target, or disconnected profile; cancellation clears the pending
  state without starting work.
- [~] Summary, Runs, Plan, Compare and Saved sections now have direct `g 1`–`g 5`
  Vim navigation. Summary owns configuration and aggregate context; Runs keeps
  virtualized samples, Plan shows profiling evidence, Compare shows the A/B
  verdict, and Saved opens the private run and definition browsers. Saved
  browsing still uses separate dialogs rather than an embedded section.
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
- [x] Raw JSON and a normalized plan tree are available; explicit
  reported estimate/actual row deltas and ratios are displayed, with zero
  estimates and unavailable/invalid counters handled explicitly. These compare
  native reported rows without normalizing provider loop/rebind semantics.
  Live/private saved Profile nodes now support j/k, gg/G, linked observations,
  n/N observed-node navigation and wrapped selected-node details (ADR-171).
- [~] Performance Plan shows bounded scan/sort observations and native PostgreSQL
  disk-sort, temporary-write and multi-batch hash evidence. Instrumentation
  overhead and inclusive counters are labelled; no node times are summed.
  SQL Server operator-local spill warnings and available numeric details are
  retained and rendered. Exact captured-node links now bind scan/sort/native-spill
  observations to the virtualized plan and selected-node details (ADR-171);
  broader diagnostic findings remain open.
- [x] Throttled background Benchmark/Profile progress: stage and attempt counters
  on existing active registrations, current-policy reads, at most two polls per
  second and one acknowledged UI update in flight (ADR-131). Completed sample
  lists remain virtualized and capped by validated limits; Vim typing and cancel
  controls stay usable. Explain access is unchanged. This is functional input
  acceptance, not a new renderer frame-budget claim.

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
- [~] Capture preparation mode, reuse, isolation, session settings, engine
  version and observed/unknown cache conditions. Benchmark context is captured
  as above. Benchmark and Profile now retain nine selected actual settings
  observed before workload setup (ADR-128), with bounded capture, cancellation
  and no editor-session inheritance. Broader settings, driver preparation
  internals and later overrides remain unproven; instrumentation and isolation
  policies stay separate from observations.
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
- [x] Persistence for reusable definitions and attached profiling plans.
  V048 stores private reusable benchmark definitions through SecretStore with
  owner-scoped SQLite indexes, optimistic revisions and no saved bind values.
  Saved benchmark payloads optionally retain a separately selected bounded
  Profile capture (ADR-136), with matching frozen desktop workload identity,
  encrypted storage and execution-free Vim Plan reopening. It remains a separate
  instrumented population and caller-associated evidence; raw plans may contain
  sensitive values. Artifacts and AI/MCP measurement views omit attachments.
- [x] Notes/tags now use owner-private encrypted payloads, revision-checked
  updates, immutable measurements and a source-bound Vim editor with retained
  conflict drafts (ADR-126). AI/MCP measurement inspection excludes annotations.
  Optional workspace/Git source context is frozen at workload review and saved
  only on explicit opt-in, encrypted beside immutable measurements (ADR-134).
  It is client-reported, includes cached observation age/coverage and stays out
  of artifacts and AI/MCP projections. Owner-private age/count retention now has
  revision-checked preview/activation, save-time enforcement, bounded automatic
  maintenance and durable encrypted cleanup (ADR-135). Default is disabled;
  original save timestamps remain authoritative after recovery.
- [x] Definition browser and validated parameter-aware rerun connection. Audited
  CRUD and explicit rerun APIs use the current connection, fresh bind values,
  engine/count checks and the existing workload confirmation. The Vim desktop
  browser now lists/opens, saves the current bound query, confirms deletion,
  and reviews fresh bind values before a revision-checked rerun on the matching
  query and exact current session/connection. Connection changes invalidate review/results;
  the report opens in Performance. SQLite positional `?` and `?NNN` binds are
  counted. Named SQLite slots now use shared native numbering, case-sensitive
  names/aliases and prepared-metadata checks; source-bound fresh-value review
  labels show actual slot mappings (ADR-138). Gaps and count mismatches are refused.
  Desktop editing now exposes name, SQL, parameter count and all serial budgets
  through a Vim form, preserving engine and optimistic revision. Stale saves
  retain drafts; server/tenant changes reject dispatch.
- [~] Saved A/B comparison: pin a private saved run as A, open another as B,
  and inspect absolute/relative client-median deltas plus variability. A
  paired variant runner now freezes two workloads and controls their interleaving;
  controlled before/after data/cache capture remains open.
- [x] In-memory baseline pinning and observed median delta; incomplete runs,
  different engines and selected configuration mismatches suppress comparison.
- [x] Alternating/randomized A/B order; record ordering and seed. Separate
  populations share one dedicated connection and total budget; incomplete pairs
  withhold comparisons. Vim pin/review, API/SDK and confirmed CLI execution retain
  ordering in private saves and JSON/text artifacts (ADR-144).
- [x] Saved A/B view shows absolute/relative median deltas, each run's sample
  deviation, and an explicitly inconclusive verdict for uncontrolled conditions.
- [~] Compatibility warnings for parameters, data, versions and settings.
  Saved and in-memory A/B views now disclose absent parameter values,
  missing environment metadata or server versions, differing preparation,
  connection, isolation or session settings, missing/different selected
  before-workload observations, limited observation coverage, and differing
  reported cache states. Data/schema equivalence and
  controlled cache/load conditions remain unproven.
- [~] Saved attached Profile plan comparison now shows bounded normalized
  operator/counter/attribute changes with heuristic alignment, explicit ambiguous/
  work-limit fallback, paths and separate capture clocks (ADR-137). Vim browsing
  and explicit CLI `performance compare --plans` execute no workload and retain
  private-data notices. Optional bounded untimed decoded-result checks now ship
  for paired benchmarks (ADR-182): ordered or duplicate-preserving unordered
  fingerprints, fixed outcomes and no samples after unsuccessful checking.
  Nonfinite/opaque values are refused; mathematical SQL equivalence, native
  precision coverage and nondeterministic/later timed-data equivalence remain open.
- [~] CLI versioned JSON import/export and numeric sample CSV, bounded import
  through normalized private save, optional SQL redaction and existing
  permission-checked deletion. Text reports recompute medians and outcome counts
  without SQL or free-form metadata. Broader redaction and permission-checked
  sharing remain open.

## Milestone 5 — advanced tools

- [x] Parameter matrices, multi-query suites and explicit per-case private
  baselines now have a bounded audited API/SDK and confirmed CLI runner (ADR-164).
  Each named query/case retains independent measurements, actual ordering and
  validated private-save/artifact lineage; incomplete suites withhold comparisons.
  The CLI resolves a complete explicit baseline map before execution. Source-bound
  Vim query/case forms, frozen review/confirmation, progress/cancellation and
  separate-case Performance/private saving now ship in ADR-165. Independent
  per-case warm-ups, iterations, timeout, budget and delay overrides with Vim
  controls, shared deadline and validated v2 lineage now ship in ADR-167.
  Serial, round-robin and seeded shuffled rounds retain exact dispatch traces,
  v3 private/artifact lineage and frozen Vim review (ADR-168).
  See [suite design and evidence](benchmark-suites.md).
- [~] CLI saved-run comparisons accept absolute milliseconds and relative
  percent slowdown limits, with independent observed crossings, unavailable
  comparisons and explicit uncertainty. Conditional binomial order-statistic
  median intervals now have 97.5% marginal/95% joint IID-model coverage, explicit
  unavailable states, outward-rounded derived bounds and shared CLI/AI/MCP/Vim
  presentation (ADR-150). Sampling-assumption evidence and controlled regression
  conclusions remain open.
- [~] Headless CLI/API Benchmark and Profile runners, explicit workload
  confirmation, private saving and saved A/B comparison are implemented
  (`sift performance`, ADR-108). User-invoked descriptive regression reports
  add optional slowdown limits; controlled regression inference remains open.
  No CI changes.
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

- Environment-context milestone (2026-10-04): version-2 reports optionally
  retain the dedicated connection's bounded `ping` server version, declared
  preparation and reuse policy, transaction isolation, uninspected profile
  session settings, and unknown cache state. Older reports decode without
  context. The Performance panel displays the context; saved A/B comparison
  flags known version and policy differences while keeping the verdict
  inconclusive. The matrix above lists each provider's native evidence.
  Verification: workspace formatting, strict Clippy, and all workspace tests
  pass. Opt-in live PostgreSQL (20/20, serial) and SQL Server (9/9, serial)
  driver suites pass against local containers. Profile-specific context was
  added in the next milestone; actual session-setting capture remains open.

- Profile environment milestone (2026-10-04): Profile responses optionally
  carry a bounded version from the dedicated connection, instrumentation mode,
  isolation policy, uninspected connection-profile defaults, and unknown cache
  state. The Performance panel displays these next to native counters without
  converting estimated SQLite nodes into actual evidence. Focused SQL Server
  and SQLite server tests pass. The opt-in live server plan/Profile acceptance
  passed for PostgreSQL and SQL Server, including version-bearing context and
  100,000-row streams. Five separate local runs measured stream medians of
  28.468 ms for PostgreSQL (27.907–29.682 ms) and 97.033 ms for SQL Server
  (94.713–97.849 ms). These are fixture observations, not portable latency
  guarantees. Full workspace gates are recorded with the milestone commit.

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

- Reusable-definition backend milestone: V048 indexes at most 100 private
  definitions / 32 MiB per owner under opaque secret handles. POST/GET/PUT/DELETE
  `.../metadata/tenants/:tenant/benchmark-definitions` support create, keyset
  list, get, revision-checked edit and delete. Names, SQL and configuration stay
  in SecretStore; only owner-scoped identifiers, revisions, timestamps, sizes
  and handles stay in SQLite. Missing secret payloads remain browsable and
  deletable. Tenant restore copies definition secrets under fresh handles.
  `POST .../sessions/:id/connections/:conn_id/benchmark-definitions/:definition/run`
  accepts a fresh run ID and caller-supplied current bind values, checks owner,
  tenant, revision, engine and parameter count, then uses the existing supervised,
  read-only, confirmed benchmark path. No bind values are stored. The server
  treats parameter count as owner-declared; the desktop derives PostgreSQL and
  SQL Server positional counts and SQLite `?`/`?NNN` counts. Dialect-wide
  placeholder checking and server-side mismatch warning at save remain open.
  The driver validates actual bind use when a run starts. The server
  audits all definition actions, and the reference SDK exposes every route.
  A crash after secret write but before index commit may leave an unindexed
  encrypted payload; a failed old-payload cleanup after a committed edit leaves
  an unreachable encrypted payload and logs a bounded warning. Automatic
  orphan collection remains open. A focused HTTP test covers owner and tenant
  isolation, bind count, engine match, confirmation, successful rerun and
  operation audit without SQL or definition names.


## Governed measurement access and CLI (ADR-108)

Private saved benchmark snapshots now have bounded AI/MCP list, statistic
inspection and descriptive comparison tools. Shared core comparison recomputes
successful measured client-elapsed samples and preserves failures separately;
engine/configuration mismatch or incomplete runs withhold numeric deltas. These
snapshots have tenant/owner scope, no managed-profile provenance or attestation.
Public AI contexts are denied until an explicit publication contract exists.

The SDK-backed CLI can run explicitly confirmed Benchmark/Profile workloads,
save measurements and list/get/compare private runs. Query inputs and confirmation
are human CLI actions; agents cannot self-confirm execution. Existing audited
permissions, dedicated supervised connections, cancellation and engine-specific
protections are reused. See `../sift-mcp.md` for the command inventory.

### Saved-run regression report design

The human CLI comparison may accept finite, nonnegative absolute milliseconds
and/or relative percent slowdown limits. Compare recomputed measured-success
medians through the existing compatibility gate, then evaluate each supplied
limit independently (strictly greater exceeds; equality does not). A crossing
is descriptive observed evidence, never a causal or statistically significant
regression verdict. Incompatible/missing medians yield an unavailable result,
never a pass. Thresholds cannot authorize execution, change saved snapshots,
or suppress the comparison warnings. No CI or automatic failure policy is added.

### Benchmark artifact exchange design

Human CLI export reads an owned snapshot through the audited get route. JSON
uses a strict version-1 exchange envelope with the existing versioned report
and an explicit SQL-redacted flag; it omits library names, owner identifiers
and saved IDs. Optional SQL redaction clears only SQL, not environment strings.
CSV exports numeric timing/sample fields and fixed outcomes, excluding SQL,
names and environment text. Missing timings/row counts are empty, never zero.
Import accepts at most 1 MiB and a supported envelope/report version, then
uses the existing audited, owner-scoped save API with a new explicit name.
Server normalization remains authoritative. Import creates no execution,
sharing, ownership restoration or provenance attestation. Public sharing and
richer readable reports remain separate.

### Reusable definition editor design

Edit the explicitly opened private definition with a frozen ID, revision and
engine. A host-owned scalar form edits name, multiline SQL, parameter count,
warm-ups, iterations, timeout, total budget and delay. Local parsing checks
bounds; existing audited update API validates SQL/binds and optimistic revision.
Save never executes SQL. Stale revisions retain the draft and require explicit
reload; no automatic overwrite. Instance/tenant changes invalidate dispatch.
Vim command focus owns edit/save/cancel; Tab cycles inputs, Enter/Escape returns
from fields, and Escape cancels the form. Editing blocks rerun/delete/navigation
until saved or cancelled. Engine changes use a new definition.

Imported snapshots receive a fresh local run UUID to preserve duplicate-save
protection. A bounded provenance warning retains the artifact's supplied source
run UUID and explicitly states that import did not execute a workload. Full
warning lists are refused rather than losing that provenance notice.

- Performance follow-up (2026-11-04): bounded plan observations retain native
  PostgreSQL disk/temp evidence and operator-local SQL Server spill warnings;
  reported row deltas handle zero/missing estimates explicitly. The Vim private
  definition editor saves SQL/count/budget changes against a frozen revision
  and retains stale drafts. CLI saved comparisons offer descriptive slowdown
  limits; JSON/CSV/text exchange and bounded private import retain unavailable
  values, explicit SQL redaction and unverified source-run provenance. Import
  allocates a fresh local run identity without executing SQL or overwriting an
  existing snapshot. Formatting, strict workspace Clippy and the complete
  workspace test suite pass on pinned Rust 1.96.1. Native SQL Server spill
  capture, statistical confidence, controlled A/B execution, richer plan links,
  sharing and the remaining advanced tools are not graduated by this slice.


### Live progress milestone (ADR-131)

Benchmark and Profile expose active stage/elapsed observations, with separate
completed warm-up and measured attempt counts for Benchmark. Progress performs
no work on the connected database and contains no SQL, bind values or rows.
Current session ownership, read permission, original workload permission,
Execute permission and frozen-SQL schema restrictions are rechecked. Snapshots
are removed with the existing job; inactive/null is not a terminal outcome.

The Vim Performance panel and definition rerun dialog poll alongside the original
request. One bounded read and one acknowledged update prevent queue growth under
a stalled UI. Completion drops polling; unsupported/transient progress reads do
not replace workload results. Per-connection generations let parked runs keep
updating when visible connections change while rejecting reconnects, including
numeric ID reuse. Late or wrong-run messages cannot revive finished work or
move keyboard focus.

Acceptance covers real managed SQLite Benchmark/Profile cancellation, attempt
counters, wrong run/mode/session, session-owner fencing, permission revocation,
positive/negative schema policy, no-store/privacy and registry cleanup. The
mock PostgreSQL cancellation regression also verifies that reading progress
invokes no driver method. GPUI tests cover stale/wrong-mode/late updates, frozen
source checks, background connection switching, reconnect identity reuse and
Vim typing during progress. A desktop test holds UI acknowledgements to prove
one-event backpressure, a minimum 500 ms poll interval and result completion
while progress acknowledgement remains pending. Formatting, strict workspace
Clippy and the full workspace suite pass. Native Windows/macOS acceptance stays
deferred; no native percentage or new render-timing result is claimed.

### Controlled paired benchmark milestone (ADR-144)

Frozen A/B workloads now execute alternating or seeded randomized rounds on one
owned dedicated connection with a shared total budget. Reports retain separate
samples/statistics and original pair identity, population, order/seed, actual
attempt trace and overall completion. The first unsuccessful attempt stops both
populations; incomplete pairs withhold comparisons. Existing read-only policy,
source authorization, exact progress/cancellation and private save normalization
apply. JSON/text artifacts preserve lineage across fresh local import IDs.

Vim pins A without execution, reviews both workloads and editable seed, validates
anchors/runtime identities, then navigates A/B in existing Performance sections.
The confirmed CLI executes and optionally saves both populations separately;
AI and standalone MCP expose bounded inspection metadata without execution.
Caller abandonment requests cancellation while the HTTP audit supervisor survives
to record the final outcome. Native close retains ownership beyond observation.

Linux acceptance: 1,726 workspace tests pass with five existing ignored; all three
native PostgreSQL/SQL Server/SQLite paired cases pass. Formatting and strict
workspace/all-target Clippy pass. See [design and evidence](paired-benchmark.md).
Data/result equivalence, cache/load control, instrumented populations and the
remaining advanced tools are still open; native Windows/macOS remains deferred.


### Conditional median uncertainty milestone (ADR-150)

Shared comparisons now recompute conservative median bounds from complete
ordinary client-elapsed populations, with explicit IID assumptions and method
identity. They withhold intervals for incompatible, malformed, failed, incomplete
or too-small populations, exclude warm-ups, and do not trim outliers. Baseline
intervals containing zero withhold relative bounds; absolute bounds remain valid.
Integer/rational arithmetic prevents overflow, cancellation and inward float
rounding. Vim caches comparison text after report/baseline changes; CLI and
private AI/MCP measurements use the same projection without raw samples or source
content. Observed thresholds retain their previous meaning and exit behavior.
See [design and acceptance](benchmark-median-uncertainty.md). Conditional coverage
is not evidence that serial observations meet the model, nor a causal or
controlled performance conclusion. Advanced tools and sampling controls remain.

### Paired decoded-result verification (ADR-182)

Opt-in preflight compares complete single-result decoded fingerprints before
warm-ups and measurements, with explicit ordered/unordered mode, row/byte bounds,
raw EOF and fixed failure outcomes. Only matching observations permit sampling.
Checks share cancellation, source generation, policy, budgets and dedicated
cleanup. PostgreSQL uses read-only repeatable read; SQLite read-only serializable;
SQL Server retains independent observations and requires a read-only account.
Vim v cycles the frozen choice; API/SDK and confirmed CLI expose the same settings.
Pair context v2 preserves bounded row-free metadata through private saves,
artifacts and read-only inspection. Legacy v1 contexts remain readable.
Semantic equivalence, native precision coverage, later timed data/cache/load and
nondeterminism remain unproven. See [design and evidence](paired-result-verification.md).
