# Query performance workbench

Status: implementation started; no new execution action is available yet.

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
- Persist definitions separately from immutable runs. SQL, plans and parameters
  can contain sensitive data: private by default, explicit sharing/redaction,
  secret handles only. Do not store result rows by default.

## Milestone 1 — contracts and measurement foundations

- [x] Record design and full implementation checklist.
- [x] Pure measurement model: warm-up/measured samples and explicit outcomes.
- [x] Deterministic summaries excluding warm-ups and unsuccessful samples;
  retain outcome counts and flag insufficient samples for tail percentiles.
- [ ] Timing dimensions: database execution, client elapsed, first row, full
  consumption; unavailable dimensions remain optional and separate.
- [x] Validated serial-run iteration, warm-up, timeout, delay and total budgets.
- [ ] Immutable query/config snapshot and capability matrix.
- [ ] Versioned API types and audited Profile/Benchmark/cancel/save actions.

## Milestone 2 — profile and Performance panel

- [ ] Current statement/selection targeting, explicit execution preview.
- [ ] Summary, Runs, Plan, Compare and Saved sections; keyboard navigation.
- [ ] PostgreSQL JSON actual plan, buffers and supported runtime counters.
- [ ] SQL Server actual plans and supported statistics IO/time collection.
- [ ] SQLite plan and timing; capability-gated deeper runtime counters.
- [ ] Raw and normalized plans, estimate/actual differences and node links.
- [ ] Evidence-based scans/sorts/spills findings without double-counting nested
  node time; instrumentation overhead labelled.
- [ ] Throttled background progress, virtualized samples, bounded memory;
  typing remains responsive. Preserve existing Explain access.

## Milestone 3 — serial benchmark runner

- [ ] Dedicated connections with enforced read restrictions and permissions.
- [ ] Preset: two warm-ups, ten measured runs, one connection; editable limits.
- [ ] Per-statement timeout, overall deadline, delay, cancel and partial results.
- [ ] Fully drain without retaining rows by default; distinguish optional
  fetch/render measurements from instrumented profiles.
- [ ] Capture preparation mode, reuse, isolation, session settings, engine
  version and observed/unknown cache conditions.
- [ ] Safe session cleanup, disconnect handling, production workload approval.
- [ ] All samples visible, median/mean/range/variability and sample-size warnings.

## Milestone 4 — save and compare

- [ ] Metadata migrations for definitions, immutable runs/samples and plans.
- [ ] Names, notes, tags, workspace/Git context, private visibility and retention.
- [ ] Saved browser independent of open query tabs; validate rerun connection.
- [ ] Baseline pinning, A/B variants and before/after comparisons.
- [ ] Alternating/randomized A/B order; record ordering and seed.
- [ ] Absolute/relative deltas and variability; explicitly inconclusive verdicts.
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

- Initial core foundation: `crates/core/src/performance.rs` provides serial-run
  budget validation and single-dimension timing summaries. Four focused tests
  cover rejected budgets, warm-up/outcome exclusion, missing/zero/large timings,
  variance and nearest-rank percentiles. p95 requires 100 timed successes and
  p99 requires 1,000; these display floors do not promise statistical confidence.
  No execution endpoint, persistence or Performance UI is wired yet.
  Verification: formatting check, strict workspace Clippy and workspace tests
  passed for this foundation (including all 29 core and 472 editor tests).
