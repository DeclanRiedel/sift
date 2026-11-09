# Concurrent benchmark load

Status: bounded concurrent client-load mode implemented and verified on Linux. API/SDK/confirmed CLI,
source-bound Vim composition/review, progress/cancel and private inspection retain
validated v4 lineage. Native PostgreSQL, SQLite and SQL Server overlap, typed binds,
source transaction preservation, private save/import, cross-phase pacing, aggregated
warmup saving and warmup-failure barriers pass. Eight focused ownership/scheduling
tests and all package checks pass: 670 server tests and 1205 other workspace
tests, zero failures and 18 existing ignored tests. Broader database saturation instrumentation, controlled load/cache
and advanced write/index experiments remain separate product work. Historical
foundation evidence below describes source and scope at those earlier milestones.

Design before implementation. Extend the existing audited BenchmarkSuite action
with `BenchmarkSuiteOrder::ConcurrentV1`: 1–8 independent dedicated workers, an
explicit global start-rate ceiling (1–1000 starts/second), and a measured-failure
policy (stop run or retire the failed worker). Each worker executes the frozen
suite's full per-case warmup/iteration counts. Validate multiplied attempts <=4096,
existing case/SQL/parameter limits, and all limits before native opens. Existing
serial/round-robin/shuffled modes retain their contracts. Unknown scheduling tags
are rejected by older servers before work; no silent serial fallback. Public
protocol 5 and locked Driver signatures remain unchanged, following the existing
additive versioned-suite contract.

## Ownership and authorization

Freeze native source generation before owned supervision. Require BenchmarkSuite
and ordinary Execute permission for every SQL statement, retain current policy
and source-authority callbacks, and admit only one classifiable read statement.
Explicit confirmation covers the multiplied workload, rate and connections;
read classification/rollback cannot contain external function effects.

Reserve all worker connection slots before opening any worker. Recheck original
source and both permissions after waits, after opens and before every dispatch.
Workers share one run registration/cancel token/deadline and retain driver-task,
query/cursor/retained-byte and shutdown ownership through actual work/cleanup.
Use existing dedicated opening/late-open ownership. Never use or instrument the
editor connection/transaction or replay stronger credentials. PostgreSQL workers
use read-only read-committed transactions; SQLite workers read-only serializable;
SQL Server requires a read-only native account and retains the existing warning.
Capture each worker's selected session settings/server version before setup.
No cache clearing, shared setting changes or implicit LIMIT.

Workers own private native handles, never registered as public session connections.
Use `OpeningConnection` for handle/tunnel/quota/opening-admission ownership and
adopt directly into an exclusively owned worker. This avoids exposing generated
connection IDs to session consumers that could otherwise reconnect/end/close them.
Source close/session close still cancel the original registered benchmark token.
Current source authority and generation checks use the captured public source;
query/cursor/byte reservations use its connection provenance. Private worker
connections consume the same tenant connection slots and bounded driver work.
A stopped attempt transfers the private worker, query/cursor resources, driver-task
permit and shutdown guard to the native owner until raw EOF and cleanup; the
observation deadline never aborts that owner. Close ownership encompasses actual
private rollback/close, with no nested session-close observer timeout. Setup,
late-open and cancellation cleanup need the same actual-lifetime guard. Explicitly
supervise every nested spawn; Tokio task locals do not inherit automatically.

`run_bounded` captures the shutdown guard through native open. The pending
opening result now retains that guard through actual late close as well;
otherwise a timed-out open could release shutdown tracking before its
unregistered connection closes. Normal adoption releases opening tracking.
An attempt owner can retain an `Arc` of the exclusively owned private worker, reply only
at verified EOF on success, and keep the worker/resources after an observation
timeout. The controller retires it and records the failed observation; it never
starts another attempt on that lease. Failed-worker cleanup must not stall healthy
workers under retire-worker policy, while the last owner still performs rollback
and actual close and publishes a cleanup receipt.

All workers finish setup before warmups; successful warmups finish across every
worker before the measured window starts. No warmup/setup/cleanup duration enters
latency distributions or throughput's observation interval. Warmup failure stops
the run. Per-case budgets begin at that case's first dispatch across workers and
remain bounded by the shared deadline; per-case delay is worker think time.
A minimum-spaced global pacer applies across all cases/workers (ceil(1s/rate),
no accumulated catch-up bursts), preserves the last warmup's spacing across the
measurement barrier, releases bookkeeping while waiting, and checks
cancellation/deadlines/authority/resource admission before admitting dispatch.

Each worker has at most one native attempt at once. Stream consumption is bounded
by configured row/byte limits, requires raw EOF after terminal completion and
rejects late errors, repeated/incomplete result sets and missing completion.
Failed/timed-out/cancelled attempts never enter successful latency summaries.
Unsafe connections are discarded; never retry a failed statement. Stop-run cancels
peers; retire-worker leaves that worker's remaining attempts unexecuted while
healthy workers finish their own workloads. Report the reduced worker population
and incomplete workload explicitly. No replacement connection exceeds the budget.
Observer loss cancels dispatch while owned cleanup and one original actor/
correlation audit settle. Existing progress/cancel/close/shutdown routes remain
operative and account for multiplied attempts. Cleanup can outlast observation;
unknown/pending cleanup prevents a completed suite claim.

## Measurement and portable lineage

Retain one BenchmarkReport per flattened case; pool only that case's identical
SQL/parameters across workers, never across cases or with instrumented Profile.
Each load trace links its worker and global dispatch index to exactly one existing
sample and stores monotonic start/end and pre-dispatch wait offsets, with no rows,
parameters or native error text. Samples remain in per-case client-admission ordinal
order. This is not native wire dispatch attestation; setup/version/settings reads
are separate from paced workload attempts and share the total setup/run budget.
Capture both offsets against one run clock and derive elapsed as end minus start;
separate elapsed-clock reads cannot establish the exact trace/sample alias.

Suite context v4 preserves raw per-worker case limits, order/configuration,
shared dispatch case IDs, exact phase boundaries and case-local load traces.
Legacy context v1–v3 remains readable. Core validation bounds workers, attempts,
indices, counts, timestamps, worker nonoverlap, warmup/measurement separation and
completion flags; normalize/import recomputes descriptive metrics. Reports and
worker environment observations remain caller-supplied/unattested after saving.

Expose successful latency distribution, all outcome/unattempted counts, completed
attempt/success throughput over the measured observation interval, observed client
peak concurrency, client busy-worker utilization and pre-dispatch/pacing wait.
Client saturation describes this load generator; database CPU, IO and native
saturation remain unavailable rather than inferred from latency or client activity.
Timed-out/cancelled traces end at observer settlement. Native work and cleanup can
remain active afterward; client occupancy metrics do not measure that outstanding
native lifetime or prove backend concurrency fell when observation stopped.
No controlled cache/load/IID or semantic-equivalence claim follows. Concurrent
samples may be dependent; descriptive comparisons require matching load config
and complete lineage, and IID median uncertainty must remain unavailable.

Private case saves, portable artifacts, CLI/AI/MCP inspection and comparisons keep
the mode/trace/metrics without raw bind values. Existing suite API/SDK/CLI accept
this mode. Vim provides a Load entry point, worker/rate/failure controls, exact
multiplied-workload review, progress/cancellation and load metrics beside case
results; source/request guards remain. Standard interaction mode stays excluded.

## Acceptance

Pure scheduling/accounting tests cover limits, phase windows, trace validation,
outcome counts, throughput/utilization and dependent-sample uncertainty refusal.
Executable behavior proves real overlap, global pacing, maximum connection/query
budgets, source/authority changes after waiting/opening, per-case limits, both
failure policies, warmup failure, cancellation/observer loss, actual-EOF/late-error
handling and cleanup/quota/audit ownership. Reuse native PG/SQL Server/SQLite
benchmark fixtures to prove independent workers, typed parameters/rows, preservation
of the editor transaction, private/artifact lineage and partial failure results.
Vim/CLI tests exercise frozen configuration/review, exact targets and metric labels.
Run formatting, strict Clippy and all package tests under the persistent 10% cap;
use established grouped verification when combined feature unification exceeds
memory. No new CI/workflows/smoke scaffolding; native Windows/macOS stays deferred.

## Foundation evidence — 2026-11-09

The observation types remain pure serde. Core code bounds multiplied workloads,
builds independent warmup/measurement schedules, validates case-local and complete
suite traces, and recomputes outcome counts, throughput, half-open client occupancy
and admission waits. Eight focused tests cover limits, phase barriers, both failure
policies, cross-case overlap/order/rate violations, corrupted samples, partial
workloads, zero-duration windows and extreme clocks. No request/runner/UI path yet
enables concurrent mode; persistence and comparison integration remain open.

Formatting and strict workspace/all-target Clippy with `sift-server/live-mssql`
passed. All package tests passed in two groups: server with `live-mssql` (52 groups,
662 passed, 13 ignored), then workspace excluding server with
`sift-server/live-mssql` (47 groups, 1204 passed, 5 ignored). The 18 existing ignored
tests were not rerun for this pure accounting change. Combined feature unification
in a single workspace test invocation remains unverified under the resource cap.

Rust validation shared CPUQuota=160% on the 16-logical-CPU host, MemoryMax=
3287567155 bytes and no swap; Cargo jobs/tests were serialized with one-CPU
affinity. The first test build hit the cap. The successful retry used 8192 server
test codegen units, 256 for UI/workspace UI and 512 for desktop, package test debug
information disabled, and jemalloc `narenas:1,dirty_decay_ms:0,muzzy_decay_ms:0,retain:false`.
No assertions/overflow checks were disabled. Peak service memory was 3284217856
bytes. These validation settings do not change committed product build profiles.

## Opening shutdown ownership evidence — 2026-11-09

The existing SessionStore opening regression now scopes an actual shutdown guard
around timeout, caller loss and missing-session cases. It verifies one in-flight
owner while native open/late close are gated, release after actual close, and
immediate opening-guard release after normal adoption. The focused regression,
formatting and strict workspace/all-target Clippy passed. All package tests passed
in the same two capped groups: 662 server and 1204 other tests, zero failures,
18 existing ignored. Peak service memory was 3284213760 bytes with the unchanged
10% cap and successful retry settings described above. The private-worker draft
is unwired and was not compiled or exercised by these checks.


## Concurrent runner acceptance — 2026-11-09 (ADR-186)

Existing native benchmark fixtures now exercise PostgreSQL, SQL Server and SQLite
concurrent workers through HTTP/SDK. All three pass overlapping client observations,
typed binds and complete row consumption, independent source transaction preservation,
selected worker environments, private saves and portable imports. They also verify
minimum global spacing across warmup/measurement, configuration-mismatch comparison
refusal, dependent-sample uncertainty refusal, retire-worker partial outcomes,
failed warmups preventing measurement, valid aggregated warmups above 100 on private
save, and exactly one terminal typed audit for a successful concurrent suite.

Eight focused runner tests pass. They cover whole connection-budget preflight
without native opening; source generation/authority changes during gated native
opening; queued pacing revocation without holding bookkeeping; stop-run versus
retire-worker fixed work; abandoned opening; late errors after Done while raw EOF
is withheld; and observer loss, cancellation and timeout before a native cursor
exists. Gated cancel/EOF/close assertions retain connection/query/cursor/byte quota,
driver admission and shared shutdown tracking through actual cleanup. A case-budget
regression proves that exhaustion cancels a peer already running another case under
retire-worker policy, rather than relying on that peer's next admission. The
source-bound Vim test includes frozen worker-count edits and existing suite result
navigation; complete workspace UI tests pass.

Formatting and strict workspace/all-target Clippy with `sift-server/live-mssql`
pass. Final package checks pass in two groups: server with `live-mssql` (52 groups,
670 passed, 13 ignored), then workspace excluding server with
`sift-server/live-mssql` (47 groups, 1205 passed, 5 ignored). The three-engine native
fixture additionally passes with `live-pg,live-mssql`. The 18 existing ignored tests
were not all executed by these default package groups. A combined single workspace
test invocation remains unverified under the cap; the two groups cover all packages.

The final validation service retained CPUQuota=160% on this 16-logical-CPU host,
MemoryMax=3287567155 bytes and no swap, with one-CPU affinity, one Cargo job and
serialized tests. Peak service memory was 3284205568 bytes. Package test codegen/
debug and jemalloc settings match the foundation evidence above; no assertions or
overflow checks were disabled. Validated Rust sources remained unchanged through
staging. No dependencies, Driver signatures, environment variables or CI workflows
changed. Native Windows/macOS acceptance remains deferred. Database CPU/IO/saturation,
controlled cache/load, isolated index experiments and advanced write benchmarking
remain open in the broader performance plan.
