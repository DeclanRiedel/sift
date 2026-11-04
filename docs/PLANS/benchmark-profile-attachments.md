# Private Profile attachments

Design before implementation. Attach an optional, explicitly selected Profile
capture to a private saved benchmark snapshot. Profile remains a separate
instrumented execution and timing population; attachment never inserts Profile
clocks into benchmark samples or attests equivalent data/cache conditions.

The pure wire attachment records capture time, exact SQL, parameter count and
the existing typed Profile response. Saving requires equal SQL, engine and
parameter count with the benchmark, a distinct run ID, and bounded plan content.
It is caller-associated evidence, not server provenance or authorization. No
bind values are stored as association metadata; raw plans can themselves expose
literals, names and values, so saving requires a separate visible opt-in.

Keep the attachment only in the existing encrypted snapshot payload, under the
same 4-MiB snapshot/64-MiB owner limits. Bound raw plans to 1 MiB, the complete
attachment to 2 MiB and normalized trees to 4096 nodes/64 levels before traversal
or display. Reject non-finite/negative numeric plan clocks and counters. No new
SQLite table, migration, driver API, environment variable or Operation is needed:
existing audited save/get/delete and revision-checked annotation replacement
cover the snapshot. Annotation edits preserve attachments; retention and durable
cleanup remove them with their snapshot. Old snapshots decode without a plan.

The desktop freezes an ephemeral salted workload fingerprint at confirmed
Benchmark/Profile dispatch: exact SQL, bound values, instance/profile and exact
session/connection generation. Retain no bind values or fingerprint on disk.
Offer an attachment only when both successful responses correspond to those
matching frozen identities and distinct run IDs, with matching SQL/engine/count.
A reconnect, changed SQL/values or unrelated report cannot create a match.
Failure/stale responses must not relabel a prior Profile as the new attempt.
Freeze the offered attachment when opening the save dialog, separate from the
workspace/Git context option, off by default. Newer profiles cannot replace that
candidate. A report opened from a definition or saved library has no live
workload association. API clients explicitly provide their own association.

The Vim saved-run browser shows capture identity, estimated versus analyzed
state, native versus server-observed clocks and the private-data warning.
An explicit action opens the attached plan through the existing Performance Plan
renderer in an open query tab without any execution. This action must respect
server/tenant scope; ordinary baseline reuse remains measurement-only. Private
snapshot JSON copy includes the attachment and labels this. Portable benchmark
artifacts, numeric exports, list summaries and AI/MCP projections remain
measurement-only. Reopening clears stale live association state.

Acceptance: legacy decode; invalid/mismatched/oversized/deep attachments;
file-backed encrypted persistence and annotation preservation; owner-private HTTP
save/reopen with no plan fields in audit/list/artifacts/AI projections; matching
and mismatched workload values/source/generation; stale Profile response;
explicit Vim opt-in frozen at dialog open and execution-free plan reopening.
Run formatting, strict workspace Clippy and full workspace tests, graduate
ADR-136, update canonical inventory, and commit. Native Windows/macOS acceptance
remains deferred.

## Acceptance — 2026-11-04

Implemented and graduated as ADR-136. Protocol acceptance proves legacy absence,
matching SQL/engine/count and separate run IDs, raw/whole-payload size limits,
normalized node/depth and attribute limits, invalid clocks/counters, and SQLite
estimated-only node semantics. File-backed encrypted metadata acceptance proves
reopen, owner/tenant boundaries, annotation preservation and absence of plan text
from SQLite/encrypted-file plaintext. HTTP/SDK acceptance validates mismatch
refusal, normalized benchmark statistics unaffected by Profile clocks, private
plan persistence and omission from audit/list/artifacts.

The real-file SQLite CLI/MCP and governed-AI acceptance paths save private plan
sentinels and prove their measurement projections omit them. UI acceptance proves
matching frozen values/source/generation, stale/failed/mismatched replies,
ephemeral salt separation, refusal to replace active work, explicit Vim opt-in,
frozen candidate identity and scope-fenced execution-free Plan reopening.

Linux validation passed: `cargo fmt --check`,
`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`, and
`cargo test --workspace --locked --offline` (1,699 tests passed, five ignored).
No Driver, schema migration, CI workflow or environment-variable changes.
Native Windows/macOS acceptance remains deferred. Controlled A/B workloads,
plan comparisons and the other advanced performance tools remain open.
