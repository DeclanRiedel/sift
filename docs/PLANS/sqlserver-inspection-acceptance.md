# SQL Server Agent and settings native boundaries

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#sql-server).

Native SQL Server 2022 probes show that SQLAgentUserRole can select
msdb.dbo.sysjobs_view but cannot select sysjobs or sysjobhistory directly.
The existing joined catalog query therefore denies legitimate Agent-role users.
Fix the read, preserving the stricter Sift rule: non-sysadmins see owned jobs
only, even when SQLAgentReaderRole/OperatorRole can see other jobs natively.

Read at most 101 metadata-visible jobs from sysjobs_view. Sysadmins retain the
bounded direct latest step_id=0 history query, including multi-server jobs.
For non-sysadmins, use documented sp_help_jobhistory SUMMARY for each selected
owned job, with ROWCOUNT 1, step_id=0 and the local server filter.
Remote-target history can remain unavailable; it is not reported as a local run. Capture its bounded row in a
native table variable and project only outcome/date/time/duration. Operator
names, messages, steps, descriptions and commands never cross the driver.
The final projection remains eight columns; response cap remains 100 plus a
truncation flag. A missing history row stays unavailable rather than zero.

Run both Agent and settings inspection on disposable, quota-accounted
connections cloned from the authorized source. Settings reset their private
ROWCOUNT too, preventing silently shortened catalogs. USE msdb, ROWCOUNT and local table
variables are inspection setup, not editor state or persistent writes. Native
procedures enforce role/owner rights; no broad catalog grant is requested.
Keep existing operation audit, source policy, schema-restricted refusal,
supervised execution, resource bounds and timeouts. Recheck source authority
before publication and close the private connection on every outcome, including
consumer cancellation. Do not expose Agent start/stop or settings mutations.

Native acceptance creates only uniquely named disabled unscheduled jobs and
disposable logins/users, records synthetic whole-job/step history, and cleans
its own fixtures on failure. Exercise public, SQLAgentUserRole, ReaderRole and
OperatorRole, ownership filtering, native permission loss, empty history,
latest whole-job selection, multi-hour duration and command/message redaction.
Verify source ROWCOUNT survives inspection, no jobs run, and SDK/API results
are uncached/audited. For settings, compare configured/effective numeric values
against the native catalog and exercise explicit native SELECT denial/recovery.
Do not infer permission needs solely from documentation: the tested 2022 build
allows a public sys.configurations read, while documentation names VIEW SERVER
PERFORMANCE STATE. Record the observed boundary and retain native enforcement.

References: [Agent roles](https://learn.microsoft.com/en-us/ssms/agent/sql-server-agent-fixed-database-roles),
[job history procedure](https://learn.microsoft.com/en-us/sql/relational-databases/system-stored-procedures/sp-help-jobhistory-transact-sql?view=sql-server-ver17),
[ROWCOUNT](https://learn.microsoft.com/en-us/sql/t-sql/statements/set-rowcount-transact-sql?view=sql-server-ver17),
[configuration catalog](https://learn.microsoft.com/en-us/sql/relational-databases/system-catalog-views/sys-configurations-transact-sql?view=sql-server-ver17).

## Native Linux acceptance — 2026-11-04

SQL Server 2022 16.0.4250.1 passes the end-to-end HTTP fixture. Public access to
Agent catalogs returns PermissionRequired. SQLAgentUserRole, ReaderRole and
OperatorRole each see exactly their two owned jobs, while sysadmin sees the
other-owner fixture too. Revoking each role restores PermissionRequired.
Synthetic step history cannot replace the latest step_id=0 whole-job result;
local calendar time and a 3,683-second duration decode correctly. A never-run
job has absent history. Descriptions, commands and history messages remain
outside reports and operation audit. No fixture job is enabled or scheduled.

A 101-owned-job fixture returns 100 plus truncation while the editor has
ROWCOUNT 2. Both job and settings inspection ignore that editor limit; its later five-row
query still returns two rows. The owned inspection
connections are gone after each read. Existing audit operations and no-store
headers are present. Fixture cleanup was verified separately: no generated
inspection jobs or logins remained.

Configured/effective max-degree-of-parallelism values match an independent
native catalog query. Explicit SELECT denial on sys.configurations returns
PermissionRequired with no settings; removing the denial restores the read.
A separate fresh public login reports sysadmin=0 and server performance-state
permission=0 while reading 97 configuration rows on this build. Native authority
therefore remains authoritative; UI guidance now mentions catalog access,
explicit denies and version-specific server-state permissions.

Passed native targets and strict lint:

- `cargo clippy --locked --offline --workspace --features sift-server/live-mssql --test agent_jobs --test server_settings -- -D warnings`
- `cargo test --locked --offline --workspace --features sift-server/live-mssql --test agent_jobs --test server_settings -- --test-threads=1`

Remote-target history for restricted readers, Agent control/execution, other
SQL Server versions, service execution and native Windows/macOS hosts are not
proven by this inspection fixture. The broader provider backlog remains open.

Inspection connection setup preserves typed session variables and replays only
supported literal SET startup statements: ROWCOUNT, LOCK_TIMEOUT, DATEFIRST,
DEADLOCK_PRIORITY and the listed boolean session options (NOCOUNT, ANSI_NULLS,
ANSI_PADDING, ANSI_WARNINGS, ARITHABORT, ARITHIGNORE, CONCAT_NULL_YIELDS_NULL,
QUOTED_IDENTIFIER, NUMERIC_ROUNDABORT and XACT_ABORT). Empty entries are ignored.
Arbitrary batches, expressions, impersonation and unknown setup are refused
before another connection opens; they are never silently discarded. Use typed
session variables for configured context. A source with a write-bearing startup
batch remains usable for its original explicitly opened connection, but these
read-only inspection endpoints will not replay it.

Final regression acceptance also keeps ROWCOUNT 2 in the configured literal SET
startup, exercises both inspection views without shortening their catalogs, and
proves the editor limit remains unchanged after settings denial/recovery. A
separate source executes its deliberately write-bearing startup once; both
inspection endpoints then return BadRequest before opening another connection,
and an independent native counter remains exactly one. Supported setup is
preserved rather than omitted; arbitrary startup authority is never dropped.
The pure setup validator rejects expressions, impersonation and unknown modes.

Final workspace gates pass on the complete implementation:

- `cargo fmt --all`
- `cargo clippy --locked --offline --workspace --all-targets -- -D warnings`
- `cargo test --locked --offline --workspace`

The native targets and strict native-target Clippy listed above were rerun and
passed after the shared inspection connection and startup replay guard were added.
