# Reviewed SQL Server index maintenance

Status: index-maintenance design in progress; feature is not implemented. The
owned-session cleanup prerequisite below is implemented and verified.

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
prerequisite does not expose any index-maintenance operation or UI control; the
design and implementation requirements above remain current work.
