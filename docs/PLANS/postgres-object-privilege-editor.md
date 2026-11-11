# Reviewed PostgreSQL object privilege editing

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#postgresql).

Status: implemented and verified on Linux with native PostgreSQL 17.10.
Windows/macOS acceptance remains deferred.

Add typed grant/revoke editing from the selected-role native privilege matrix for
all currently represented families: connected database, user schemas, tables/
views/foreign tables, live columns, sequences and routines. Target identity is
kind/OID/optional column number, not client SQL or routine-signature text. Resolve
native names and exact routine input types on the server. Grantees are typed role
names or PUBLIC. One explicit privilege per action; grant with/without grant
option, revoke the privilege or only its grant option. Revokes use RESTRICT.
Cascading revocation and additional privilege families remain separate work.

Use existing audited PreviewPostgresObject/ApplyPostgresObject APIs and explicit
production confirmation. New privilege preview/apply workflows retain a bounded
owned supervisor, original principal/correlation, shutdown guard and exactly one
typed terminal audit after HTTP observer loss. Bind preview to session/connection, native database and
backend identity, actor/session role identities, target/schema/owner/grantee
identities, native ACLs and relevant table/column metadata. Bound native state to
128 KiB; refuse excessive state, system/TEMP objects, unavailable identities,
invalid privilege/kind combinations and version-incompatible MAINTAIN. Preserve
schema-restricted-profile refusal and operation/read-only policies.

Native has_*_privilege WITH GRANT OPTION determines authority. PostgreSQL selects
the owner when acting with owner privileges; otherwise choose the sole accessible
native ACL grant-option holder. Refuse ambiguous eligible grantors instead of
claiming a predictable grant edge. The user can explicitly SET ROLE in their
connection and reopen review. Do not impersonate or alter role/search-path state.
Bind exact grantor/grantee privilege edges. Refuse no-ops; ordinary GRANT does not
remove existing grant options. PUBLIC cannot receive grant options. A native
revoke can leave access through other grantors, PUBLIC, memberships or ownership.
Table revocation may affect corresponding column grants; disclose this and include
column ACLs in the reviewed state.

Display ordinary GRANT/REVOKE SQL for review. At apply, prepare fresh native state
and compare the preview fingerprint, then execute one atomic native DO statement
that rechecks the captured state, performs exactly the reviewed command and checks
post-command identity and the complete expected native ACL transition (including
corresponding column grants for table revocation). Any failed guard or
native RESTRICT error rolls back the statement. This avoids silent PostgreSQL
warning-only no-ops and refuses name/role replacement between review and execution.
Use escaped SQL string literals for generated bodies and commands; no fixed dollar
quote delimiter may contain untrusted names. Existing owned native query lifetime,
timeouts/cancellation and terminal audit apply to the entire statement.

Vim a opens the selected object form; grantee defaults to the selected matrix
role. u toggles PUBLIC explicitly for all-role changes; role fields always mean
literal role names, including a quoted role named PUBLIC. m cycles grant/grant-option/
revoke/revoke-option modes. Tab edits grantee/privilege; p requests native review.
The existing review/explicit-confirm flow binds the original instance, tenant,
profile, session and query connection. Display the frozen target and invalidate
review on changed context. The matrix stays an inspection surface; editing does
not reinterpret effective checks as permission to administer objects.

Extend existing PostgreSQL native membership acceptance with all six families,
PUBLIC, grant options and delegated authority, native RESTRICT dependencies,
no-op and unauthorized refusal, column-vs-table distinctions, routine overloads,
quoted names, actor/grantee/object identity changes, stale ACLs and audited terminal
outcomes. Exercise native guard rollback/postconditions directly when useful.
Extend GPUI source/keyboard/production-confirmation acceptance. Run formatting,
strict workspace/live-feature Clippy and full workspace tests under the existing
10% Rust/Cargo CPU/RAM limits, using normal UI optimization settings. Driver
signatures and protocol purity remain unchanged. Windows/macOS acceptance remains
deferred.

Native semantics: https://www.postgresql.org/docs/17/sql-grant.html and
https://www.postgresql.org/docs/17/sql-revoke.html.


Validation evidence (2026-11-06): formatting and strict workspace/all-target
Clippy passed with both default features and `sift-server/live-pg`. The existing
native PostgreSQL membership acceptance passed with all six privilege families,
PUBLIC versus a literal role named PUBLIC, delegated/ambiguous grantors, RESTRICT,
quoted names, stale identities and transactional event-trigger rollback. Seven
focused privilege tests passed, including the Vim review/confirmation flow and
owned audit completion after observer loss. Full workspace tests passed: 1,773
passed, six existing ignores, zero failures.

Rust/Cargo ran in the shared capped slice: CPU quota 160% of one core on the
16-core host (10% aggregate), memory maximum 3,287,567,155 bytes (10% host RAM),
no swap, one Cargo job and one test thread. The initial test build reached the
memory ceiling; retrying with one-core affinity reduced compiler-internal
parallelism while retaining the same normal UI optimization settings and build
cache. The successful focused/full test service peaked at 3,281,387,520 bytes.
Cascading revocation, membership/grant-chain paths and additional privilege
families remain separate inventory items.
