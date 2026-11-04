# Reviewed SQL Server database users

Design before implementation. Extend the existing typed security preview/apply
workflow with create-for-existing-login, remap-to-login, change-default-schema
and drop-user actions. Keep the current session/connection, native database,
actor and principal identity in every review. Existing Operations audit the
actions; no Driver signature or SQLite secret-storage change is needed.

The native acceptance target is ordinary SQL-authenticated server logins and
instance-authenticated SQL database users. SQL users WITHOUT LOGIN may change
default schema or be dropped, but native SQL Server forbids remapping them.
Contained password users, Windows/external identities, certificates, asymmetric
keys and built-in principals retain explicit refusals for this initial native
family; their credentials and identity providers need separate accepted designs
and evidence. Login creation/password/reset/enablement is not part of these
user actions and remains tracked as work.

Create names an existing visible SQL login and explicitly chooses dbo as its
initial default schema. A separate action changes that schema to an existing
visible schema. Remap repairs an orphaned instance-authenticated SQL user or
moves its existing direct/inherited rights to another existing SQL login. Keep
the database user name explicit in ALTER USER so native auto-rename cannot
change the reviewed name. Refuse no-ops, duplicate names/SID mappings, built-ins,
owner-login mappings and remapping/dropping the current actor. Never create a
login implicitly or change a password. Disabled logins remain disabled and are
identified in review rather than silently enabled.

Review queries bind native IDs, SIDs, creation/modification dates, database
identity/owner/collation, actor identity, selected login state, selected schema,
current user memberships, explicit permissions involving the user, and known
owned securables. Return only bounded counts/flags and a SHA-256 digest. Require
complete database security metadata for create/remap/drop and reject effective
metadata DENYs. Default-schema editing can use narrow ALTER-on-user/self
rights and selected visible identity without requiring whole-database catalog
access. Native permissions are checked afresh for each action: CREATE USER or
ALTER ANY USER for create, database CONTROL for remap, ALTER-on-user/self for
default schema, ALTER ANY USER for drop. The apply path re-previews, compares
identity, checks Execute and Apply policy, then dispatches exactly the reviewed
quoted SQL through supervision. No lock or transaction is held across review;
native SQL Server remains final authority under concurrent DDL.

Drop discloses affected role memberships/explicit permission rows, refuses known
owned securables, uses native DROP USER without cascade or automatic ownership
transfer, and leaves the server login intact. SQL Server validates any remaining
ownership families. Remap preserves principal ID, membership, grants, ownership,
name and default schema; it changes SID mapping, so existing authenticated
sessions can retain old tokens and broader inherited/fixed-role rights are not
expanded into an effective-privilege matrix. Hash the direct affected state and
state these limits in review.

Expose resolved login/default-schema fields in the bounded principal catalog;
null login can mean no login, an orphan or metadata visibility limits. Add Vim
form actions to the existing administration editor, with exact-source review,
confirmation and stale-response guards. Keep destructive actions explicit.

Use a disposable native SQL Server database and generated fixture logins to
verify create, default-schema resolution, remapping/orphan repair, preserved
rights, drop effects/login survival, narrow authority and denial, metadata DENY,
no-op/protected/unsupported refusals, reused-name login/user/schema identity,
changed actor/database/permissions and production confirmation. Clean up only
fixture-owned objects; never print generated passwords or raw setup SQL.
Run formatting, strict workspace Clippy and all workspace tests, graduate a
stable ADR and commit. Windows/macOS host acceptance remains deferred.

Native contracts: Microsoft Learn [CREATE USER](https://learn.microsoft.com/en-us/sql/t-sql/statements/create-user-transact-sql?view=sql-server-ver16),
[ALTER USER](https://learn.microsoft.com/en-us/sql/t-sql/statements/alter-user-transact-sql),
[DROP USER](https://learn.microsoft.com/en-us/sql/t-sql/statements/drop-user-transact-sql?view=sql-server-ver17).

SQL Server security inspection and edits require a database-wide Sift connection
scope; a schema-only profile, including one scoped to sys, cannot authorize
these database-wide actions. A read-only Sift profile rejects Apply before
catalog work or SQL parsing. Native permissions remain independently required.

## Acceptance evidence

Native HTTP/SDK acceptance passed on SQL Server 2022 16.0.4250.1 on Linux in the
existing disposable security/ownership fixture. It verifies quoted creation,
initial dbo schema, disabled-login preservation, schema resolution, remapping,
genuine orphan repair, preserved principal ID/membership and real SELECT access,
login survival and object preservation after drop. It rejects duplicate mappings,
owner-login mappings, no-ops, missing schemas, built-ins, roles, certificate/key
users, WITHOUT LOGIN remapping and current-actor remap/drop even under CONTROL.

Recreated login/user and schema-owner changes, changed direct permissions,
changed actor/database context and changed login enablement invalidate review.
Editor ROWCOUNT 1 produces the same complete review hash and remains in effect
after preview. A real narrow SQL login proves CREATE USER without CONTROL,
remap/drop denial, later ALTER ANY USER authority, revocation and metadata-DENY
refusal. The fixture cleans its own database, logins and temporary server grant;
generated credentials are absent from the Sift operation trail.

GPUI acceptance covers all four typed Vim forms, preview before confirmed apply,
and refusal after the native connection changes. API validation rejects a SQL
user name shaped as a Windows account before driver work. Policy tests reject
schema-only security scope (including sys) and reject read-only Apply before
catalog work. Formatting, strict workspace Clippy, native-target Clippy and the
full workspace suite pass.

```sh
cargo fmt --all -- --check
cargo clippy --locked --offline --workspace --all-targets -- -D warnings
cargo clippy --locked --offline --workspace --features sift-server/live-pg,sift-server/live-mssql --test sqlserver_ownership_live -- -D warnings
cargo test --locked --offline --workspace --features sift-server/live-pg,sift-server/live-mssql --test sqlserver_ownership_live -- --test-threads=1
cargo test --locked --offline --workspace
```

This validates the stated SQL identity family, not Windows/external/contained
credential management, effective privileges or the complete security editor.
Windows/macOS host acceptance remains deferred.
