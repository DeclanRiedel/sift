# PostgreSQL role lifecycle and public attributes

Status: implemented and verified (ADR-143). This extends the role editor
beyond NOLOGIN creation and explicit memberships (ADR-140), without changing the
Driver trait or handling password material.

## Contract

Add typed rename, drop and public-attribute patch actions, and creation with a
public-attribute patch. Preserve the existing CreateRole NOLOGIN request. Patches
cover LOGIN, INHERIT, CREATEDB, CREATEROLE, SUPERUSER, REPLICATION, BYPASSRLS,
connection limit and password-valid-until (typed UTC timestamp or infinity).
Unspecified attributes stay unchanged on update and use native defaults on create.
Empty/no-op updates and invalid connection limits are refused. Catalog rows expose
public details only; never select password hashes or accept password SQL.
New lifecycle actions require PostgreSQL 16+, matching the native authority model
already used for explicit memberships. Basic NOLOGIN creation remains compatible.

## Native review and authority

Use supervised Driver queries for a bounded, secret-free native snapshot: database
OID/name, server version, current/session actor OIDs and public flags, target and
rename-destination OIDs/attributes, native ADMIN authority, and up to 200 relevant
membership edges and shared dependency addresses each. Incomplete snapshots refuse
review. Hash source session/connection, typed SQL and the complete native snapshot;
apply rechecks source policy, identities and catalog state before execution.

Superusers or CREATEROLE plus native ADMIN authority can manage the relevant
non-superuser roles. Match the specific native action: ordinary attribute changes
cannot target replication roles; setting CREATEDB/REPLICATION/BYPASSRLS requires
the actor's corresponding authority. SUPERUSER changes require native superuser
rights, and the bootstrap superuser's SUPERUSER property is immutable. Native
PostgreSQL remains authoritative and may reject concurrent changes. Reserved
pg_ roles, PUBLIC and rename collisions are refused; rename/drop of current or
session actors requires a different source connection. No name-based privilege
inference, automatic SET ROLE, ownership reassignment or DROP OWNED is performed.

All actions use existing audited PreviewPostgresObject/ApplyPostgresObject variants
with typed actions, exact SQL, current confirmation and production acknowledgement.
Public attributes and names can be audited; passwords and hashes never enter the
wire, operation log, metadata or plans.

## Effects and limits

Roles are cluster-wide. Attribute changes affect authentication/authorization;
NOLOGIN and limits do not terminate existing sessions, and password expiry does
not disable every authentication method. Rename can clear an existing MD5 password
(native behavior); disclose this without inspecting hashes. DROP ROLE removes
memberships automatically and fails while native ownership/ACL dependencies remain.
Show bounded membership/dependency impact and preserve native dependency failures;
never imply cascading deletion, session termination, or full effective-privilege
analysis. Credential changes, per-role GUC defaults and dependency remediation
remain separately tracked work.

## Vim and evidence

Roles view shows public attributes and starts source-bound create, rename, update
and drop drafts. Patch controls distinguish keep/on/off and retain typed validation;
all mode/input changes invalidate previews. Reuse existing request/source/reconnect
fences and production confirmation. API/SDK native acceptance must cover lifecycle,
attribute persistence and preservation, non-superuser ADMIN/CREATEROLE authority,
privileged flags, dependency refusal, stale identities and explicit confirmation.
Actual Vim tests must exercise mode selection, patch construction, review and
source fencing. Run fmt, strict workspace/all-target Clippy and workspace tests,
gradually update ADRs and feature inventories, and commit verified work.

Native semantics: [ALTER ROLE](https://www.postgresql.org/docs/17/sql-alterrole.html),
[CREATE ROLE](https://www.postgresql.org/docs/17/sql-createrole.html),
[DROP ROLE](https://www.postgresql.org/docs/17/sql-droprole.html), and
[role privilege inquiry](https://www.postgresql.org/docs/17/functions-info.html).

## Delivered evidence

The existing isolated PostgreSQL native membership fixture now exercises the
lifecycle through actual HTTP/SDK calls: public attribute persistence and omitted
attribute preservation; timestamp no-op detection; ordinary and privileged role
creation; native CREATEROLE/ADMIN authority and privileged-attribute denial;
rename; dependency-blocked drop; and stale attribute, dependency and recreated
role identities. No password hashes are selected. Native OIDs are decoded as
JSON strings. Finite expiry snapshots normalize to UTC ISO timestamps.

The actual Vim administration test now cycles rename/attribute/drop forms,
constructs keep/on/off patches and optional values, previews each typed action,
requires explicit production confirmation and rejects changed source contexts.
The role catalog exposes public details without password fields.

Verification: `cargo fmt --check`, strict workspace/all-target Clippy and
`cargo test --workspace` pass (1,721 tests, five existing ignores). The extended
native PostgreSQL HTTP/SDK fixture passes with `live-pg`. Credentials, per-role
session defaults and ownership/grant dependency remediation remain separate
tracked work; native Windows/macOS acceptance remains deferred by the user.
