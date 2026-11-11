# Controlled serial A/B benchmark execution

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#performance-workbench).

Status: implemented; accepted as ADR-144. This implements the formerly open
alternating/randomized A/B requirement in query-performance.md. Comparing saved
independent runs is already available; this design adds a controlled execution
schedule while preserving separate latency populations and honest context limits.

## Execution contract

A paired request freezes baseline A and candidate B SQL/bind values, one set of
validated per-population warmup/measured counts, one shared sampling deadline,
query timeout, inter-query delay, order and explicit whole-workload confirmation.
Validate both single-read workloads and ordinary execution permissions before
admission, and recheck current source policy for every attempt. One source/run
registration owns the dedicated connection, cancellation and progress. Both
workloads use the same dedicated session and existing engine read-only policy;
SQL Server still requires a read-only account for database-enforced protection.
Neither SQL classification nor read-only transactions sandbox external functions.

Warmup rounds precede measured rounds. Each round executes each workload once.
Alternating order starts A/B and flips to B/A on the next round, restarting at
A/B for the measured phase. Seeded randomized order uses a specified version-1
SplitMix64 coin per round; seed zero is valid. Seed, ordering algorithm and actual
attempt order are retained. Scheduling is deterministic, never a crypto claim.
Stop both populations at cancellation, budget exhaustion, authority loss, resource
failure or the first unsuccessful attempt. No later workload may continue after
an uncertain/failed attempt. Fully consume and discard rows; never fill the grid.

## Measurements and persistence

A paired result contains two ordinary BenchmarkReports with separate samples,
outcome counts and recomputed statistics. Warmups and unsuccessful/unfinished
attempts never enter latency distributions. Each report carries optional versioned
pair context: root run identity, population, ordering/seed, actual global attempt
order and overall pair completion. Local sample ordinals remain contiguous per
population; the global order supplies the interleaving. Pair contexts persist in
private encrypted saved reports and portable artifacts; they confer no provenance
attestation or execution authority. Never retain raw bind values in reports.

Existing single runs remain unchanged. Saved population A and B can be inspected,
exported, saved individually and compared using existing private workflows.
Comparison must disclose interleaving and withhold deltas when the paired run was
incomplete. Do not flatten A/B into one latency distribution, claim significance,
or assert equal data, cache, plan, load or complete environment. A shared session/
transaction can itself create ordering/cache/session interactions. Instrumented
Profile populations remain separate advanced work; do not auto-attach a Profile
or mix its clocks into either population.

## Product and ownership

Add an audited paired operation, HTTP/SDK request and explicitly confirmed CLI
runner. Reuse exact source/run cancellation and bounded progress, authorizing reads
against both workloads. Progress counters describe aggregate A+B attempts, not
committed work or latency samples. Keep native cleanup alive beyond its observation
timeout and preserve late-open ownership; caller/source cancellation cannot orphan
owned native resources. No Driver signature change or UI dependency in shared
crates is needed.

In Vim, pin A from a normal frozen Benchmark review without executing it. A later
review of B can select single, alternating or seeded randomized execution. Show
both SQL/bind populations, counts/order/seed and shared budget before explicit
confirmation. Require original instance/tenant/profile/runtime identities and
unchanged source SQL/binds for both anchors; reconnect, source edits or expired
anchors require fresh review. A paired result lets users inspect A or B in the
existing Performance sections and save/export the selected population with its
pair context. Preserve opt-in private workspace source observations per population.

## Acceptance

Prove deterministic balanced scheduling, warmup exclusion, separate statistics,
partial outcome handling, current authorization, exact cancellation and native
cleanup through realistic behavior tests. Native PostgreSQL, SQL Server and SQLite
fixtures must exercise distinct workloads and ordering. Verify actual Vim pin/
review/confirm, population navigation and stale-source refusal, plus CLI request
confirmation and private artifact round trips. Run fmt, strict workspace/all-target
Clippy and workspace tests; graduate stable decisions, reconcile inventories and
commit verified work. Native Windows/macOS acceptance remains deferred.

## Linux acceptance evidence

Workspace tests pass: 1,726 passed, five existing ignored. Native PostgreSQL,
SQL Server and SQLite HTTP/SDK acceptance passes all three cases, covering both
orders, distinct fully consumed row counts, warmup exclusion, separate statistics,
failed second populations, private save/load, redacted JSON artifact import with
original pair lineage and rejection of a corrupted attempt trace. No bind sentinel
appears in reports or audit. An incomplete pair withholds deltas even when A
locally completed.

Actual GPUI keystrokes cover pin-without-execution, seeded review including zero,
seed-field focus, confirmation, population navigation and edited-anchor refusal.
The managed SQLite CLI path executes and separately saves A/B, then inspects its
bounded standalone MCP context. Cancellation tests cover aggregate counters and
wrong-run refusal; dropping the HTTP handler during measurement still cancels
owned work, preserves the editor connection and produces a final audit record.
A paused-time native-close observer regression retains quota after timeout and
closes exactly once. Formatting and strict workspace/all-target Clippy pass.

This graduates controlled interleaving only. Result equivalence, controlled data/
cache/load changes, instrumented paired Profile populations and the remaining
advanced workbench tools remain open. Native Windows/macOS acceptance is deferred;
no new CI workflow or smoke script was added.
