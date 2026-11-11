# SQL Server effective-permission matrix

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#sql-server).

Design: add a distinct audited ReadSqlServerPrivileges operation, SDK method and
source-bound Vim Security view. Use the existing supervised disposable inspection
connection, including connection quota, bounded cleanup and source-policy rechecks;
never EXECUTE AS, SET ROWCOUNT or create inspection variables on the editor handle.
Require database-wide Sift scope and current native catalog authority.

Inspect either the original connected actor or an explicitly selected ordinary
SQL database user (instance-authenticated or WITHOUT LOGIN). Selected-user reads
require native IMPERSONATE authority and use database-scoped EXECUTE AS USER only
inside the disposable connection. Built-in/role/group/external/contained identity
families remain explicit refusals for selection. The native connected actor is
allowed to inspect itself without impersonation. A database-user token does not
include login/server-level rights and is not a successful-login assertion.

Capture the actor, database identity and a bounded page of database, schema,
ordinary table/view, column, sequence and routine identities before impersonation.
Require full database VIEW DEFINITION without effective metadata DENYs; refuse
ambiguous/incomplete catalog authority rather than labelling omissions as denials.
Use only quoted native identifiers and bound user/page inputs. HAS_PERMS_BY_NAME
checks represented applicable permission families under the selected native token.
Preserve true/false/NULL separately; NULL is unavailable, not denied. Native
inheritance, ownership, fixed roles and DENY exceptions are evaluated by SQL Server,
not reconstructed from explicit ACL rows. Grant-option/chain provenance, execution
of protected data, ownership-chain/module behavior and external authorization are
not inferred. Do not run selected routines or read selected table data.

Return one bounded JSON report with actor/subject identity and explicit scope,
page/truncation state and native checks. Cap limit at 100, offset at 10000 and
response at 1 MiB; inspect limit+1 identities. Pages are separate changing native
observations. Catalog/principal identity changes during a read must refuse the
report. Refuse unexpected result shapes. Source, read-policy and native permission
failures propagate through the owned audited handler and never publish a success.

Vim v in SQL Server Security opens a subject form (blank means connected actor),
then the matrix. Navigation, paging, reload and close bind exact captured instance,
profile/session/connection/request identity. Stale replies and source switches
cannot replace the matrix. Keep database-token limitations and unavailable checks
visible. The existing grant editor remains separate from inspection.

Verify a disposable SQL Server database with direct, inherited/fixed-role,
PUBLIC/ownership and DENY/column-override examples; no-permission and NULL handling;
restricted catalog/impersonation denial; changing identity; quoted names, pagination,
editor ROWCOUNT/identity preservation and owned cleanup. Add meaningful API/policy
and Vim dispatch/stale-source tests. Run native acceptance plus formatting, strict
workspace Clippy and workspace tests under the existing 10% Rust/Cargo cap, then
record a stable ADR and commit. Native Windows/macOS acceptance stays deferred.

Native contracts: [EXECUTE AS](https://learn.microsoft.com/en-us/sql/t-sql/statements/execute-as-transact-sql?view=sql-server-ver16),
[HAS_PERMS_BY_NAME](https://learn.microsoft.com/en-us/sql/t-sql/functions/has-perms-by-name-transact-sql),
and [metadata visibility](https://learn.microsoft.com/en-us/sql/relational-databases/security/metadata-visibility-configuration?view=sql-server-ver16).

## Implemented interface and evidence

The API is `GET /v1/sessions/{session}/connections/{connection}/sqlserver/privileges`
with optional principal, offset and limit. The SDK exposes `sqlserver_privileges`.
In SQL Server Security, v opens a subject form; empty means the fresh connected
actor. Enter inspects; j/k navigate, n/p page, r reload, e chooses another subject,
and Escape returns. The virtualized node list has a separate content scroller
for long scope/counter details; navigation reveals the selected object's details.

The local SQL Server 2022 fixture reports 16.0.4250.1. An exact native-query probe
covers a principal containing an apostrophe/closing bracket, a schema containing
a closing bracket and spaces, a dotted table and a column containing a closing
bracket. On this host the raw column name returns NULL while its QUOTENAME form
returns the native grant; production uses QUOTENAME and the fixture preserves
this regression. The hidden column remains false under the table DENY, while
an explicit column grant is true. No NULL is coerced to false.

The real HTTP/SDK native fixture also covers inherited schema access, fixed-role
SELECT, PUBLIC INSERT, owner CONTROL, bounded paging, unsupported/missing subjects,
metadata/impersonation denial and recovery, and preserved editor ROWCOUNT/identity.
Deterministic native DDL inserted between identity capture and evaluation renames
a captured table or drops/recreates a captured user. Both exact production-query
identity guards reject the report with native code 50003; no timing sleeps or
production hooks are used. Fixture DDL and credentials stay in disposable test
resources and are cleaned up by the existing native ownership fixture.

The final authorization gate runs after owned cleanup. A blocked native-close
fixture closes the source after the native report has been computed and before
cleanup completes; the report must be refused rather than published. UI tests
cover subject/blank-actor dispatch, exact request/page bindings, native NULL labels,
source changes while awaiting a reply or editing a form, and stale reply refusal.
Native execution still has its database-version and token scope above; no actual
protected-data access or successful-login claim follows from an allowed check.

## Final acceptance — 2026-11-06

Formatting, strict workspace/all-target Clippy and the full locked/offline
workspace suite with `sift-server/live-mssql` pass: 1,798 tests, zero failures,
six existing ignored tests. The published OpenAPI operation is included in the
SDK coverage manifest. The focused matrix UI tests also pass independently.
Subject submission uses an owned TextInput Submitted subscription bound to the
current input entity; a discarded field cannot submit its replacement form.
The unchanged keyboard scenario verifies actual Enter dispatch, paging, source
changes and stale replies through the executor channel.

Earlier full-suite builds reached the shared memory cap. Final validation keeps
normal UI optimization and the same resource limits, using temporary test-profile
codegen-units=512 for sift-server/sift-desktop and 256 for sift-ui/
sift-workspace-ui, with debug=0 for all four. One build job/test thread and
single-core affinity remain in force, with the shared 160% CPU quota on this
16-logical-CPU host, MemoryMax=3,287,567,155 bytes and MemorySwapMax=0. Final
validation peaks at 3,276,189,696 bytes and exits 0. Only test-owned GUI scratch
files/caches were removed and the task cgroup's retained cache was reclaimed;
the caps were never increased. No test was skipped or weakened to fit the cap.

These are Linux/native SQL Server and GPUI behavior receipts. The earlier
packaged graphical artifact evidence still identifies its exact older source
and package hash; this milestone does not regenerate or promote a desktop
package or establish Windows/macOS acceptance. No CI workflow is enabled.
