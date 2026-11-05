# Scoped SQLite statistics and index maintenance

Status: implemented; accepted as ADR-146.

Extend the managed SQLite maintenance contract with ANALYZE of one ordinary main
table and REINDEX of one main index on an ordinary table (including native
constraint indexes). No database-wide, collation-name, attached/TEMP, virtual or
shadow target is admitted. Names are bounded identifiers and generated SQL always
qualifies main. Full ANALYZE can change plans; no automatic optimize/approximation
or index/data-equivalence claim is introduced.

Reuse audited ManageSqliteDatabase, current root/tenant/read-write authority,
idle-connection admission, source file identity, one-use source/action-bound
preview, explicit write confirmation and verified-backup acknowledgment. Report
exact SQL additively. Native proof also binds schema_version and the bounded
target/owning-table definitions and index metadata. Recheck under BEGIN IMMEDIATE
before execution, preventing concurrent DDL/writers during apply. A preview is
not a native lock; intervening changes require fresh review.

Run on the existing dedicated SQLite worker, with durable journal/synchronous
guards, original-open-file validation, the existing 1 GiB source ceiling including
WAL, native progress/deadline/close/caller-cancel checks and bounded busy waits.
Commit one native transaction, or roll back on failure/cancellation. Always restore
internal-authority, progress and busy handlers; poison/close a connection whose
rollback cannot be completed. Timeout alone does not establish commit outcome.
Caller abandonment signals cancellation while an owned HTTP supervisor retains
shutdown tracking and records the final audit outcome.
No external file or extension/function/collation loading is enabled. No Driver
signature changes or extra UI dependencies in shared crates are needed.

The Vim Maintenance panel adds table/index choices and fields, exact target/SQL
review and source-specific typed confirmation. Edits or source switches invalidate
review; late responses retain existing generation guards. Shared pure rendering
prevents server/driver SQL disagreement. Test real files through HTTP/SDK and the
worker: statistics actually change; index rebuild preserves values; quoted names,
stale schema, read-only/root/tenant boundaries, active transactions, competing
writers, cancellation, rollback and subsequent connection usability. Extend the
actual Vim workflow test. Run fmt, strict workspace Clippy and workspace tests,
graduate stable decisions and commit. Windows/macOS acceptance remains deferred.

## Linux verification

All 1,730 workspace tests pass (five existing ignored), with strict workspace/
all-target Clippy and formatting. Real-file HTTP/SDK acceptance proves full
statistics only for the selected quoted table, native writes on quoted ordinary
and constraint indexes, preserved values and integrity, one-use tokens, backup
acknowledgment, active-transaction refusal, stale schema and competing writers.
Read-only, virtual/shadow/view, wrong-schema and injected-name targets are refused.

The worker regression cancels after observing its native write transaction on a
200,000-row fixture, verifies statistics rollback and restored authority/connection
use, then unwinds a real uniqueness failure after externally changed index
semantics. A separate real-file HTTP-handler regression drops the caller during
native work and proves rollback plus a final failed-request audit. The actual
GPUI confirmation regression now includes ANALYZE and REINDEX, their exact SQL,
source/object phrase, required backup acknowledgment and existing stale guards.

Proof size is bounded before application allocation and only its SHA-256 digest
is retained in review leases. Native inspection has a separate ten-second
deadline. Physical ENOSPC, broader recovery concurrency, arbitrary/attached schema
maintenance and native Windows/macOS evidence remain outside this milestone;
the full product goal remains active. No smoke script or CI workflow was added.
