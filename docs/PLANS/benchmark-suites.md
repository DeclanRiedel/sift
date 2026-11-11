# Serial parameter matrices and query suites

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#performance-workbench).

Status: API/SDK/CLI implemented and verified on Linux (ADR-164). Structured
desktop composition and case browsing are tracked in the
[desktop follow-up](benchmark-suite-desktop.md) (ADR-165). Independent per-case
limits are implemented in ADR-167 ([design/evidence](benchmark-case-limits.md)).

A suite freezes named queries, each with explicitly named parameter cases. Cases
are explicit rows, not an implicit Cartesian product. Flatten query/case order and
run each case's warmups then measurements serially on one dedicated connection,
with one registration, cancellation token and total deadline. Reuse the existing
read classifier, native read-only transaction where available, resource admission,
bounded driver supervision and connection cleanup. Recheck suite and ordinary
execution policy for every case before setup and each attempt. SQL Server retains
its read-only-login requirement. Reusing a session can affect later cases; ordering,
cache, data and load equivalence are not controlled or inferred.

The pure-serde request has a non-nil suite run ID, explicit whole-workload
confirmation, shared limits and 1–32 total cases. Query and case IDs are distinct
within their scopes, 1–64 ASCII letters/digits/underscore/hyphen. Bound expanded SQL
to 1 MiB, serialized bind values to 256 KiB, each case to 256 parameters and total
planned attempts to 4096. Reject invalid requests before opening a dedicated
connection. Each case has its own report and unique run identity; never pool
latencies across queries or parameter cases. Return the actual attempt ordering
and completion state. Reports retain validated, unattested suite lineage through
private saving and portable artifacts; raw bind values never enter reports,
progress or audit. Incomplete suites withhold suite-derived comparisons.

Expose a distinct audited BenchmarkSuite operation and API/SDK runner. Reuse
benchmark progress/cancel routes, authorizing every frozen query for progress.
HTTP observer loss cancels the suite while an owned supervisor settles cleanup
and one typed audit. Expose `sift performance suite` with explicit CLI confirmation,
request-file limits, optional private per-case saving and returned case/ID mapping.
An optional baseline file maps query/case IDs to existing private saved runs;
resolve every baseline before execution, reject unknown/duplicate/missing cases,
and compare case-by-case only. No aggregate score, causal regression conclusion,
implicit baseline selection, automatic writes or CI workflow is introduced.

Native acceptance must exercise at least two queries and two parameter cases,
row counts, distinct samples, warmup boundaries, source transaction preservation,
read-only refusal, authorization, stale baseline mappings, cancellation and partial
case reports. Reuse existing integration fixtures; no smoke scripts/workflows.
Run formatting, strict workspace Clippy and full workspace tests within the
existing 10% CPU/RAM cap. Driver signatures remain unchanged. Desktop suite forms
landed in ADR-165, and independent case limits in ADR-167; additional ordering
now ships in ADR-168. Native Windows/macOS acceptance remains deferred.


## Acceptance evidence (2026-11-06)

The existing native paired-benchmark fixture now also exercises suites against
PostgreSQL, SQL Server and real-file managed SQLite. All three pass two named
queries and three explicit parameter cases, independent row counts, distinct case
run identities, warmup boundaries and exact serial traces. The source connection's
Serializable transaction stays active and is rolled back explicitly after testing;
the dedicated suite connection is gone. Unconfirmed requests and write SQL refuse
before execution. A failing middle query leaves later cases unattempted, retains
partial reports and withholds comparisons even for earlier completed cases.

Every native case survives private save/read and redacted artifact import with
its suite context intact. Corrupt traces refuse save. Different case identities
withhold comparisons, and private bind sentinels stay out of report/audit JSON.
The existing cancellation test now covers single, paired and suite runs, proving
bounded cancellation, registration release and original-connection preservation.
The HTTP observer-loss test covers suites and retains one terminal audit. The
shared bounded audit-owner acceptance still verifies original actor/correlation.

The SQLite CLI fixture executes and privately saves two parameter cases, reuses
the returned saved-ID map for case-by-case baselines, rejects unknown and relabelled
mappings, and closes every owned session. Each CLI invocation generates a fresh
run ID when the request file omits it. The store continues to refuse attempts to
overwrite immutable saved run identities. The OpenAPI/SDK coverage check includes
`benchmarkSuite` and passes.

Formatting and strict workspace/all-target Clippy pass with both default features
and `sift-server/live-pg,sift-server/live-mssql`. Native acceptance passes 3/3;
full workspace tests pass 1,774 with six existing ignores and zero failures.
No workflows, smoke scripts or Driver signatures changed.

Rust/Cargo used the shared 160% CPU quota on the 16-logical-CPU Linux host, with
one-core affinity, one Cargo job, one test thread, no incremental codegen and no
swap. The memory maximum remained 3,287,567,155 bytes (10% host RAM); the final
validation service peaked at 3,281,391,616 bytes. Tests used codegen-units=256 and
debug=0 overrides for sift-server, sift-ui, sift-workspace-ui and sift-desktop,
retaining normal UI optimization. The desktop follow-up supplies structured matrix forms and separate case browsing.
Independent case budgets now ship in ADR-167. Round-robin and seeded shuffled rounds ship in ADR-168;
native Windows/macOS acceptance remains deferred.

Suite round-robin and seeded shuffled rounds now extend the original serial
runner/composer in ADR-168 ([design/evidence](benchmark-suite-ordering.md)).
Native Windows/macOS acceptance remains deferred.
