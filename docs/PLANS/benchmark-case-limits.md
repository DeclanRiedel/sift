# Independent benchmark suite case limits

Status: implemented and accepted on Linux (2026-11-06).

Extend explicit suite cases with optional complete BenchmarkLimits overrides.
An omitted override inherits the suite defaults. The suite's total_budget_ms
always remains the absolute shared deadline, including setup; an override's
budget starts immediately before that case's first attempt. Validate effective
limits before connection admission, require each case budget no greater than the
shared budget, and cap the sum of all effective warm-ups/measurements at 4096.
Keep serial query/case ordering, one dedicated connection and existing fail-fast
semantics: an unsuccessful attempt or exhausted case budget ends the suite and
leaves later cases unattempted. A case deadline includes its delays. No time is
reserved for later cases, and no population is pooled. Per-case configuration
does not establish cache, data or load equivalence.

Build scheduling and progress counters from each effective configuration. Apply
the minimum of query timeout, case deadline and shared deadline to execution;
setup remains bounded by the original shared deadline. Delay after the last
attempt of a case must not consume that case's deadline or steal its success.
A delay between cases remains part of the shared budget, with the previous case's
configured delay. Cancellation, policy rechecks, supervised cleanup and typed
terminal audit retain existing ownership.

Portable suite lineage version 2 records the complete bounded effective limit
vector and shared budget. Validate each case report against its indexed limits,
validate the exact serial trace prefix and sample count, and reject mixed or
inconsistent vectors. Version 1 remains readable only with an empty case-limit vector and no shared-budget
value and retains its original uniform schedule. These are caller-supplied,
unattested observations. Saving and artifact import must preserve and validate
both versions; incomplete suites still withhold comparisons.

The Vim composer exposes an explicit case override toggle and five structured
case-limit fields; inheritance is visible. Review shows effective limits for every
case and the shared budget. Hidden edits invalidate frozen review, and replies
must match all effective limits and the complete lineage vector. API/SDK and the
confirmed CLI accept the same request; no new command or workflow is needed.

Extend existing core, native three-engine and GPUI acceptance for heterogeneous
counts/deadlines, inherited defaults, invalid/aggregate limits, v1 compatibility,
v2 corruption, progress counters, source preservation and exact reviewed payloads.
Run formatting, strict workspace/native-feature Clippy and full workspace tests
under the existing 10% Rust/Cargo CPU/RAM cap. Native Windows/macOS acceptance
remains deferred. Alternative suite ordering remains separate work.

## Acceptance evidence

Strict default and native-feature workspace/all-target Clippy passed. The existing
native paired/suite fixture passed against PostgreSQL, SQL Server and real-file
SQLite (3/3). It now runs three suite cases with distinct warm-up/measurement
counts, timeout/budget/delay configurations, exact serial traces and independent
saved reports. Version 2 context survives private save/read and redacted artifact
round trips. A 100 ms case budget with 60 ms internal delays stops before all five
requested measurements and leaves subsequent cases unattempted, although the
shared suite budget is 5000 ms. Existing failure, cleanup, source transaction and
private bind/audit checks remain in the same fixture.

Core acceptance passes heterogeneous schedule and aggregate-limit validation,
v1 round trips and v2 corruption refusal. The existing hung-run test now verifies
heterogeneous requested progress totals and cancellation/registration cleanup.
The GPUI suite test passes the real u toggle, configured request payload, hidden
case-limit edits after review, mismatched reply vectors and existing source,
progress/cancellation and separate-report guards.

Full workspace acceptance passes 1,779 tests, zero failures and six existing
ignores. Formatting and strict default/native-feature all-target workspace Clippy
pass. The workspace run exposed a concurrent AI apply preflight/receipt race;
its separate fix passes eight identical simultaneous SQLite HTTP requests,
three-engine native AI acceptance and the full retry. Every Rust/Cargo run retained the shared 10% CPU/RAM cap, one
CPU affinity, one build job/test thread and no swap. The successful final service
peaked at 3,281,862,656 bytes, below the 3,287,567,155-byte maximum. Normal UI
optimization and the established four large test-package overrides were retained.
Alternative ordering and native Windows/macOS acceptance remain open.
