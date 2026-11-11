# Reviewed SQL Server index maintenance

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#sql-server).

Status: implemented for the bounded ordinary-rowstore scope below (ADR-147).
Native Linux acceptance passes; Windows/macOS acceptance remains deferred.

Expose explicit single-index offline REBUILD, REORGANIZE and index-statistics
UPDATE STATISTICS through a typed audited API/SDK and Vim Maintenance review.
Target schema/table/index names are quoted sysname identifiers, not SQL. Start
with ordinary disk rowstore tables and enabled non-hypothetical clustered or
nonclustered indexes, including constraint and filtered/computed-column indexes
where strict SET options can be proven. Wider partition/columnstore/memory/temporal
families need explicit native proof before admission. No ALL-index, database-wide,
online/resumable, plan-forcing or automatic fragmentation-threshold action.

Preview must inspect bounded native object/index/partition/column metadata,
ALTER and metadata authority, native actor/SIDs and database identity. Bind exact
SQL, original source/session, native target identity including physical partition/
HoBT identity, actor and relevant shape/options to review. Apply repeats source
policy, native identity and authority. Native checks remain authoritative under
concurrent DDL; no claim that optimistic review locks the catalog or attests data.

Use a disposable connection and the existing strict startup replay allowlist.
Normalize required index SET options and ROWCOUNT only there; preserve the editor.
Compare native source/dedicated actor identity, refusing unreplayable impersonation
instead of using a stronger original login. Active source transactions are refused.
Queries go through the supervised Driver trait with deadlines/cancellation;
opening and native close retain quota/tunnel ownership beyond observer timeout.
The existing SQL Server inspection helper's aborting close observer must be fixed
before its owned-session machinery is reused.

REBUILD/statistics completion means native command acknowledgement. REORGANIZE
can retain partial work after cancellation, and timeout/disconnect cannot prove
completion or rollback. Review discloses locking, log/temp/disk load, required
backup expectations, statistics/plan changes and unsupported index states such as
page-lock-disabled REORGANIZE. No query-performance improvement is promised.

Acceptance must use the live SQL Server Driver and actual HTTP/SDK/Vim dispatch,
with restricted ALTER/VIEW DEFINITION principals, revocation, source actor changes,
stale/replaced targets, quoted names, unsupported states, actual statistics/native
changes, preserved values and editor ROWCOUNT/SET state, cancellation and owned
resource cleanup. Run fmt, strict Clippy and full workspace tests; graduate stable
decisions and update inventory. Native Windows/macOS acceptance remains deferred.

## Verified ownership prerequisite

Existing Agent/settings inspection used a five-second timeout owning the close
future, so expiry could drop native cleanup and release quota prematurely. Both
normal completion and destructor cleanup now schedule one owned native close
task; the observer borrows its handle and expiration leaves that task alive.
Scheduling disarms the guard. Connection-entry quota/tunnel ownership survives
until native close settles. ADR-129 records the corrected contract.

Paused-time tests wedge native close and cover explicit observation plus owner
drop, proving quota retention beyond five seconds and one release after native
completion. Live SQL Server Agent/configuration acceptance passes, including
restricted roles, permissions and preserved editor ROWCOUNT. All 1,731 workspace
tests pass (five existing ignored), with strict Clippy and formatting. This
prerequisite established the ownership contract used by index maintenance.

## Delivered implementation and acceptance

Typed `MaintainSqlServerIndex`, API and SDK preview/apply support `rebuild`,
`reorganize` and `update_statistics`. Exact SQL uses offline REBUILD or FULLSCAN
statistics with MAXDOP 1. The shared renderer validates UTF-16 sysname bounds and
quotes every identifier. Native review uses bounded catalog metadata and a
SHA-256 digest; only the digest and permission/support flags leave the snapshot.
Current source policy, native identity and authority are checked again on apply.
Read-only and schema-restricted managed profiles are refused centrally.

A disposable connection preserves source provenance, quota and late-close
ownership, normalizes strict index SET options and refuses actor/database drift.
Apply requires the current review digest, production confirmation and explicit
verified-backup acknowledgment. The owned HTTP audit supervisor survives caller
abandonment while cancellation requests native cleanup. No catalog lock or
completion/rollback proof is implied by a review or failed request.

The Vim Maintenance panel selects `e` rebuild, `o` reorganize and `u` statistics;
`p` (or `r`) previews, `k` acknowledges the backup and `a` applies after exact typed
confirmation. Results and apply bind instance, tenant, profile, session, query
connection and request generation. Changed source/forms invalidate review.

Live SQL Server HTTP/SDK acceptance uses a restricted ALTER/VIEW DEFINITION user
without SELECT, a quoted filtered computed-column index and PAGE compression.
It proves all three actions, changed physical HoBT identity after rebuild,
advanced statistics timestamp, unchanged values/compression and preserved editor
ROWCOUNT/SET options. It refuses missing acknowledgments, ALTER revocation,
page-lock-disabled reorganize, replaced indexes, disabled indexes, impersonation
and active transactions. Cancellation is requested only after a second native
session observes the DDL blocked behind a write lock; the editor remains usable.
GPUI acceptance exercises all three Vim actions, backup/phrase guards, stale
responses and changed query connections. Partitioned/columnstore/memory/temporal,
replicated/CDC/graph/filetable, online/resumable and database-wide actions remain
outside the delivered scope.


Verification: all 1,733 workspace tests pass (five existing ignored), including
Vim confirmation/source guards and centralized profile-policy refusal. Native
SQL Server ownership/index acceptance passes with terminal audit assertions.
Formatting and strict workspace/all-target plus live-feature Clippy pass.
