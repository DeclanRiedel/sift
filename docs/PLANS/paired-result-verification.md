# Paired decoded-result verification

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#performance-workbench).

Design: close the paired benchmark's missing result-check boundary with an opt-in,
untimed, bounded preflight. Preserve serial latency populations, immutable reports,
private measurement semantics and existing explicit workload confirmation.

Add optional ordered/unordered verification settings to BenchmarkPairRequest:
1–100,000 rows per result, 1 KiB–16 MiB canonical bytes per result (defaults 10,000
rows/4 MiB). None retains existing execution. Preflight runs A then B once on the
owned dedicated connection before warm-ups; its clocks and dispatches never enter
samples, statistics, actual_order or finished-attempt counters. Add a Verifying
progress stage. Both reads count against the shared deadline, query timeout,
current source/policy checks, query resources and cancellation/cleanup ownership.

Freeze the pair's original connection generation before the supervisor starts;
copy credentials/configuration only under that generation check and refuse source
replacement between preflight/measurement dispatches. Never reopen the editor's
source to perform a check. PostgreSQL verification/measurement uses a dedicated
read-only repeatable-read transaction, SQLite its existing read-only serializable
transaction, and SQL Server retains the read-only-account requirement and separate
read observations. Record the actual workload isolation policy. No cache clearing
or shared settings changes occur; checks can warm caches.

Compare complete single-result decoded output, including column names/type refs,
typed values, row order or duplicate-preserving unordered row fingerprints. Hash
streaming canonical encoding under byte limits; canonicalize JSON object order.
Reject nonfinite floats, opaque Native/display-only values, unexpected typed-null
results, malformed/multi-result streams and excessive JSON depth/nodes. Do not
confuse missing result metadata/completion with an empty result. Require raw EOF.
No approximate numeric or cross-provider normalization is claimed. Fingerprint
matching is observational evidence about decoded values, not mathematical SQL
semantic equivalence or an attestation about all native precision/representations.

Unordered mode sorts bounded row digests and retains multiplicity. Fingerprints,
row bytes, column names and raw native errors never appear in reports, logs or
saved/artifact/AI/MCP projections. Keep only fixed outcome labels, limits, mode,
complete row/byte/column counts and the engine's snapshot policy. Reserve bounded
scratch capacity and never store result rows. Checks are opt-in; the confirmation
covers two additional potentially effectful reads, since read classification and
rollback are not a sandbox for external functions.

Only Matched permits warm-ups/measured attempts. Different, bounds/unsupported
output, failure, access/source change, timeout or cancellation returns incomplete
zero-sample reports with explicit check status; cleanup still completes or remains
owned. If settings setup ends before checking, retain a requested check's aborted
status instead of silently presenting it as disabled.

Pair context v2 carries verification; v1 remains readable with no check. Validate
bounds/counts/outcome invariants, identical complementary context and no timed
samples after a failed check on private save/import/comparison. Context remains
unattested caller-supplied measurement, never execution authority. Preserve the
check through encrypted saves and portable artifacts. Existing comparison warnings
about timed data/cache/load equivalence and IID assumptions remain in force.

API/SDK/confirmed CLI JSON expose settings and fixed results. Vim paired review
cycles off/ordered/unordered with v, freezes the choice alongside SQL/params/order
and displays limits/extra reads; Performance and private/artifact/CLI inspection
show outcomes without hashes. AI/MCP receive bounded inspection metadata only.

Acceptance: order sensitivity, unordered duplicates, metadata/type/null/JSON,
nonfinite/opaque refusal, zero rows and bounds; forged/legacy saved contexts;
matched/mismatch without sampled work, timeout/cancel/access/source replacement,
private projections, frozen Vim review, CLI roundtrip and real PostgreSQL/SQL Server/
SQLite behavior. Run formatting, strict all-target Clippy and workspace tests under
the persistent 10% Rust/Cargo cap. No Driver signature or dependency/CI changes.
Native Windows/macOS acceptance remains deferred. Broader database/environment
identity, controlled load/cache, instrumentation and semantic equivalence stay open.

Native acceptance found that PostgreSQL's extended-query path emitted column
metadata only after its first row. Retain the implicitly prepared statement
explicitly and emit its descriptor once after query startup, including empty
results. This uses the same prepare/execute policy, not an additional prepare or
cached statement. Preserve existing command-tag affected counts for empty streams
and parameterized/CTE writes; never substitute decoded rows for native counts.

## Acceptance evidence (Linux)

Formatting and strict workspace/all-target Clippy pass. The full offline/locked
workspace suite with `sift-server/live-mssql` passes: 1,852 passed, zero failed,
six ignored across 97 result groups. This includes native SQL Server/SQLite
paired checking and encrypted save/artifact round trips, CLI saved inspection,
Vim review freezing, ordered/unordered duplicate handling, type/null/JSON rules,
bounds, late native errors/raw EOF, timeout/cancellation and source replacement.
The separate `live-pg` paired acceptance passes, including matching empty results
and preservation of PostgreSQL zero-column row descriptors. Legacy contexts and
invalid verification/paired comparison contexts are covered. Focused benchmark
and Vim tests also pass (six tests).

Validation kept the existing 10% host resource ceiling: shared/service
CPUQuota=160% on the 16-thread host, CPU affinity 0, one Cargo job/test thread,
MemoryMax=3,287,567,155 bytes and no swap. Passing final unit
`sift-pair-final.service` exited successfully with peak memory 3,283,070,976 bytes.
Earlier compiler attempts hit the unchanged memory cap. Transient validation
settings used server 4,096 codegen units (desktop 512, UI 256), test debug info off,
plus compiler `MALLOC_CONF=narenas:1,dirty_decay_ms:0,muzzy_decay_ms:0` to release
freed allocator memory. Project profiles and CI remain unchanged. No native
Windows/macOS acceptance is claimed; those hosts remain deferred.
