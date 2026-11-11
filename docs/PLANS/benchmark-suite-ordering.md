# Explicit benchmark suite ordering

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#performance-workbench).

Status: implemented and accepted on Linux (2026-11-06).

Add explicit Serial, RoundRobin and RandomizedRoundsV1 { seed: u64 } ordering to
suite requests. Omission preserves Serial. Flatten query/case identity as before;
round-robin dispatches one attempt from each nonexhausted case per round. Shuffled
rounds permute the active cases in each round. Each case retains its own ordinal,
warm-ups then measurements and limits. All modes remain serial on one dedicated
connection; no population is pooled and no concurrency/load mode is implied.

RandomizedRoundsV1 specifies SplitMix64 with wrapping state increment and the
existing paired-run mixing constants, then Fisher–Yates over active indices with
multiply-high range reduction. This is deterministic, bounded and replayable for
all u64 seeds, including zero; it is not a cryptographic or uniformly sampled
experimental design. Stable golden traces form part of the versioned contract.
Limit schedules to the existing 32 cases and 4096 attempts before native setup.

Retain shared deadline, cancellation, policy rechecks, query supervision and
owned cleanup/audit. Per-case budgets remain elapsed wall time from first dispatch,
including intervening cases and delays. Check a case's deadline at its next
scheduled dispatch and bound its query by that deadline. Exhaustion ends the
suite at that dispatch, retaining earlier attempts; unscheduled cases stay empty.
Between-case delays use the preceding case's delay and shared deadline. Completed
case reports survive later failure. Order does not control cache/data/load state,
and shuffled rounds do not make observations IID or prove causal differences.

Portable suite context version 3 records the ordering (including seed), all
effective limits and shared budget. Validation reconstructs the exact schedule,
checks actual trace prefix and indexed sample count, and refuses mixed/unknown
version fields. Versions 1 and 2 retain their original serial semantics and no
ordering data. Native reports, private saves and artifacts preserve the trace.
The report case list remains stable query/case order regardless of dispatch order.

The Vim composer cycles ordering explicitly and exposes a seed field for shuffled
rounds. Review names the algorithm, seed, effective limits and complete bounded
planned trace. Hidden order/seed changes invalidate confirmation; replies must
match the frozen ordering and vector. API/SDK and the confirmed CLI use the same
request. No new workflow or smoke script. Extend core/native/GPUI acceptance for
unequal counts, round coverage, deterministic seeds, partial failure/cancellation,
legacy lineage and corrupt traces. Run fmt, strict default/native-feature Clippy,
three-engine acceptance and full workspace tests within the 10% Rust/Cargo cap.
Native Windows/macOS acceptance remains deferred.

## Acceptance evidence

Strict default and native-feature all-target workspace Clippy pass. The existing
native paired/suite fixture passes PostgreSQL, SQL Server and real-file SQLite
(3/3). All three modes preserve heterogeneous warm-up/measurement populations;
round-robin and seed-zero shuffled dispatch match exact expected traces. Private
saving and redacted artifact import retain v3 ordering/seed/trace. An interleaved
failing query retains the [0,1,2] trace, including an earlier completed case and
an incomplete warm-up population, and withholds suite comparisons. Existing source
transaction, cleanup, cancellation and private-bind checks remain.

Core acceptance passes stable golden traces for seeds zero, one and u64::MAX,
every prefix/population of those heterogeneous traces, aggregate bounds and
v1/v2/v3 lineage validation. Ordered hung-run cancellation/progress remains green.
The GPUI suite acceptance passes real r cycling, configured seed dispatch, hidden
seed edits after frozen review, explicit confirmation, changed-order reply refusal
and existing case-limit/source/progress/report ownership checks. Performance and
text reports expose ordering and seed; review wraps the complete planned trace.

Full workspace acceptance passes 1,780 tests, zero failures and six existing
ignores. Formatting and strict default/native-feature workspace/all-target Clippy
pass. Rust/Cargo retained the shared 160% CPU quota on the 16-logical-CPU host,
one CPU affinity, one build job/test thread, no swap and the unchanged
3,287,567,155-byte memory maximum. The successful full validation service peaked
at 3,282,219,008 bytes. Normal UI optimization and the four established large
package test overrides (codegen-units=256, debug=0) were retained. No Driver
signatures, CI workflows or smoke scripts changed. Native Windows/macOS
acceptance remains deferred.
