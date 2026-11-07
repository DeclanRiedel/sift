# Reviewed PostgreSQL default privileges

Design: add the missing future-object privilege editor to the existing typed
PostgreSQL administration preview/apply and Vim Roles form. Cover one explicit
creator role and recipient, one privilege, global/current-database scope or one
named schema, for TABLES, SEQUENCES, ROUTINES, TYPES and SCHEMAS. Include grant,
grant with option, REVOKE RESTRICT and grant-option-only revoke. PUBLIC is an
explicit tagged choice, distinct from a role named PUBLIC. SCHEMAS is global-only;
MAINTAIN requires PostgreSQL 17. No ALL, multi-role/schema or CASCADE action.

Use native pg_has_role(..., USAGE) authority, matching has_privs_of_role, without
SET ROLE or stronger credential replay. Creator determines future defaults; role
membership does not make a member's newly created objects inherit those defaults.
Per-schema additions cannot subtract global defaults. Existing objects, schema
USAGE, owner/inherited access and current connections are not changed by this
operation. Explain these distinctions in review; show bounded global and selected
schema ACLs, including expanded built-in global defaults when no stored row exists.

Bind database/actor/creator/grantee/schema identities and current native authority,
server version, both ACLs, SQL and source session/connection to review. Bound each
ACL to 256 edges and native JSON to 128 KiB. Fail closed on unavailable identities,
system schemas/system creator roles, invalid kind/right pairs, absent schema, PUBLIC grant options,
no-op edges, truncation and stale native state. Use catalog-qualified reads, typed
binds and quoted identifiers. Global/schema choice is an Option, never a magic name.

Apply uses current policy and captured native generation, original actor/correlation
and existing owned HTTP audit/shutdown supervision. Re-read state before dispatch;
execute one statement-atomic native block that checks reviewed state, runs exact
DDL, then checks the expected ACL transition plus unchanged identity/other scope.
Raise on mismatch to roll back that statement, including warning-only no-ops or
concurrent changes observed during execution. New/deleted pg_default_acl rows are
normal: compare semantic ACLs rather than inventing future catalog row OIDs.
Optimistic checks do not lock future object creation or attest concurrent DDL.
Native permissions and transaction semantics remain authoritative.

Reuse source-bound Vim administration review: Roles a/m selects default-privilege
intent; creator/schema/grantee inputs, explicit PUBLIC, family and right controls,
preview then explicit production confirmation. Freeze requests, invalidate review
on edits/source changes and accept only current request generations. API/SDK reuse
typed PostgreSQL object endpoints. No Driver signature/dependency/CI changes.

Acceptance: global and schema additive behavior on newly created objects while
old objects stay unchanged; all five families, grant options, explicit PUBLIC vs
named role, delegated/revoked native authority, stale actor/schema/creator/grantee/
ACL and source identities, quoted names under nonstandard strings, no-ops and
bounded refusal. Include Vim frozen request/confirmation and existing owned audit
behavior. Run native PostgreSQL, formatting, strict all-target Clippy and full
workspace checks under the unchanged Rust/Cargo 10% host cap. Native Windows/macOS
acceptance remains deferred. Reconcile stale object-privilege checklist text.

Native reference: [PostgreSQL 17 command semantics](https://www.postgresql.org/docs/17/sql-alterdefaultprivileges.html)
and [REL_17_STABLE native authority and ACL transition](https://github.com/postgres/postgres/blob/REL_17_STABLE/src/backend/catalog/aclchk.c).

## Linux acceptance

Formatting and strict workspace/all-target Clippy pass. The offline/locked
workspace suite with `sift-server/live-mssql` passes: 1,855 passed, zero failed,
six ignored across 97 result groups. New unit coverage proves transition/no-op/
bounds handling and refuses replay after native source replacement even with
identical catalog data, before DDL dispatch. Vim acceptance exercises all four
future-default intents, frozen creator/schema/recipient requests, source fencing
and production confirmation; focused validation covers PUBLIC and scope/kind rules.

The separate `live-pg` native membership/administration acceptance passes. It
covers all five default families, direct ACL evidence for routines/types despite
built-in PUBLIC access, actual future-table/sequence/schema access, unchanged old
objects, additive global/schema privileges, grant-option removal, no-ops,
production refusal, delegated then revoked authority, changed actor, replaced
creator/recipient identities and schema, PUBLIC versus the named role, and names
with dollar parameters/quotes/backslashes under nonstandard strings. Shadow
functions and a raising user jsonb equality operator under an explicit untrusted
search path do not intercept the qualified native review/guard.

Native validation corrected zero-dimensional empty ACL handling and explicitly
casts PostgreSQL JSON OIDs to numeric fields where required. Final validation unit
`sift-default-privilege-final-2.service` exited successfully with peak memory
3,283,054,592 bytes. The unchanged resource cap used CPUQuota 160% on the 16-thread
host, affinity CPU 0, MemoryMax 3,287,567,155 bytes, no swap, one Cargo job/test
thread, server 4,096/desktop 512/UI 256 test codegen units with debug info off, and
`MALLOC_CONF=narenas:1,dirty_decay_ms:0,muzzy_decay_ms:0`. Project profiles and CI
remain unchanged. Native Windows/macOS acceptance remains deferred.
