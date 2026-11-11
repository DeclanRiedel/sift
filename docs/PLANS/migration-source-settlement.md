# Migration apply source attribution and owned settlement

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#ai-execution).

Status: implemented and verified on Linux; graduated to ADR-175.

Design: bind the existing HTTP ApplyMigration action to one server-captured
connection before asynchronous metadata preparation. Derive actor, tenant,
profile and configured database target from that captured native entry, never
from an edited vault profile. Validate versioned workspace references against
the initiating actor and the captured managed tenant; a caller who owns two
tenants must not associate one tenant's migration with the other's workspace.
Configured target describes connection input, not all objects a statement can
access. Keep migration plan scope, policy/revision, production checks and
one-use consumption unchanged.

Create the existing typed ApplyMigration audit before preparation. Refused source,
actor, plan, quota and metadata requests must leave one failed audit with original
actor/correlation and generic private diagnostics, without native SQL or ledger
entries. Freeze prepared source fields for eventual settlement. Recheck captured
connection-generation identity at migration admission, transaction begin and each
statement by supplying the captured entry directly to the native execution path;
retain the existing independent human-reviewed AI dispatch guard. A closed/reconnected/replaced source cannot
silently change the operation's native destination after preparation.

One bounded owned task retains apply execution, ledger settlement and terminal
audit after HTTP consumer loss. Share the existing query-settlement semaphore and
logical shutdown guard; propagate the same guard into native work through raw EOF.
Caller loss is not automatic rollback or a reason to cancel a known database
outcome. A late observed run can be settled only by the invocation that won the
atomic plan claim; a competing retry cannot reuse that outcome. Keep admission until metadata settlement ends; no unbounded background
work or independent second mutation is introduced. Preserve correlation across
spawned work and best-effort metadata errors without exposing SQL, bind/row values,
profile credentials or native diagnostic echoes in audit messages.

Audit status must reflect terminal run state: Applied succeeds; canceled,
rolled-back, failed, partial or incomplete runs fail with stable codes. Preserve
migration outcome/rollback ledger semantics and durable run records. Aggregate
row counts stay unavailable if any native count is missing or the checked sum
overflows; missing observations never become zero. Do not
claim atomic metadata/database commit, delivery to the client, or rollback of
uncertain effects. Other grid/bulk/transfer and export attribution remain work.
No public wire, locked Driver signature, dependency or CI changes are needed.

Acceptance extends migration lifecycle and existing managed/native fixtures:
original actor/correlation, successful captured target after a profile edit,
valid same-tenant versioned source, cross-tenant/stale source refusal before
execute/ledger, replaced native generation refusal, one-use plan preservation
on preparation failure, failed/rollback/canceled audit status, caller loss during
held native execution and retained shutdown/admission through terminal ledger,
plus ledger privacy. Run formatting, strict all-target workspace Clippy and full
workspace tests under the persistent 10% Rust/Cargo cap. Native Windows/macOS
acceptance remains deferred.


## Focused evidence

The existing reconnect regression now proves refusal before query, migration and
migration-transaction dispatch on a replaced generation. A bounded-admission test
exhausts the shared settlement permits and observes one failed ApplyMigration
audit before source preparation, with no shutdown lifetime started. A claim-fence
test proves that an unclaimed attempt cannot recover a competing run, even when
that run is already Applied, and retains session/connection boundaries.

All seven Driver-backed migration lifecycle cases pass. The initiating principal
owns both test tenants. Foreign and stale workspace references fail without
native execute, ledger changes or consuming the valid plan; a subsequent valid
same-tenant reference applies. After the vault profile configuration changes, the
ledger still records the opened connection's original configured database and
profile/tenant/workspace identity. The production audit sink verifies original
actor/correlation for both refusals and successful apply.

The caller-loss case holds the final native result on an explicit gate, refuses a
replay while the original run is active without extra execution or ledger rows,
and drops the HTTP waiter. Releasing Done lets the owned nontransactional run
settle its durable receipt, committed ledger and one audit for each request.
Keeping the original producer open retains the one shared shutdown lifetime until
raw EOF. This is a server ownership/control fixture, not provider certification or
proof of network delivery. Transactional COMMIT continues to require its existing
native admission/EOF fence.

Existing rollback, partial, cancellation and digest/stale-plan cases remain green.
Failed/partial/canceled runs produce failed audits without native diagnostic
sentinels; unknown row counts remain absent, and complete known counts sum exactly.
These cases use the existing programmable Driver fixture; the full workspace run
separately covers the existing enabled real-provider fixtures.


## Workspace verification (2026-11-07)

Rust 1.96.1 formatting and strict all-target workspace Clippy passed with locked,
offline dependencies and `sift-server/live-mssql` enabled. The complete workspace
suite passed 1,812 tests in 97 groups, with zero failures and six existing ignored
tests. This includes the existing real-file SQLite and enabled native SQL Server
fixtures; the new source/caller-loss/claim tests are programmable Driver control
acceptance, without a provider-certification or client-delivery claim.

All Rust/Cargo work remained in the shared `app-sift-rust.slice`, CPUQuota=160%
on the 16-logical-CPU host, MemoryMax=3,287,567,155 bytes and no swap. Cargo used
one CPU affinity, one build job, one test thread and nonincremental compilation.
Test codegen units remained server/desktop=512 and UI/workspace-UI=256, debug=0,
with normal UI optimization retained. The final workspace service exited
successfully with status 0 and memory peak 3,268,689,920 bytes. Evidence is retained
under `~/.cache/sift-goal-logs/sift-migration-source-verified-*.log`.

No public wire, Driver signature, dependency, secret storage, CI or desktop
artifact changed. Native Windows/macOS acceptance stays deferred. Migration
apply completes this source/settlement boundary; other mutation/export attribution,
broader security work and the parent product goal remain open.
