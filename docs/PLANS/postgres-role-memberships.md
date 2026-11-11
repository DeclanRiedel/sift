# PostgreSQL role membership workbench

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#postgresql).

Design precedes implementation. Add a paged explicit-membership catalog and
reviewed grant/update/revoke workflow to API, SDK and Vim Monitor. PostgreSQL 16+
is required for native ADMIN/INHERIT/SET options and grantor identities; older
servers receive an explicit unsupported response before those catalogs are used.
Existing roles and other workbench actions retain their existing version support.

Wire additions stay pure serde: membership rows include role, member, grantor and
three option booleans. Typed grant actions require all three option values and an
explicit named grantor; revoke actions also name the grantor. Existing audited
List/Preview/ApplyPostgresObject Operations, authorization, managed profile schema
and read-only restrictions, supervision and production confirmation apply. Driver
signatures, metadata, secrets, env and CI remain unchanged.

Resolve native role/member/grantor identities and actor/session/database identity
for every preview. Capture public role attributes, exact membership edge OID and
options, other grants for this pair, and the grantor's ADMIN memberships. Cap
captured edge sets at 200 and refuse overflow. Never select password/config fields.
Bind exact SQL and observed state into the review hash. Repeat capture on apply;
replaced roles, changed actor/grantor, changed options or authority invalidate it.
Quote identifiers, including GRANTED BY. Native PostgreSQL owns final permission,
cycle and concurrency checks; a preview is not a catalog lock.

Grant explicitly sets ADMIN, INHERIT and SET. It can create or change the selected
grantor's edge; unchanged values are refused. Revoke removes only that grantor's
edge with RESTRICT, refusing dependent grants rather than applying an unreviewed
cascade. Other grantors can preserve effective membership; disclose that in the
review. Changes are cluster-wide, may confer administrative or inherited rights,
and do not claim an effective privilege matrix or live-session revocation.

The Memberships Monitor view pages bounded rows with grantor and all options.
Its requests/replies bind instance, tenant, profile, session, connection, request
identity and an invalidated read generation. Vim forms edit role/member/grantor,
choose grant or revoke, toggle the three grant options, and stage exact SQL for
explicit production confirmation. Reconnects, view changes and stale responses
cannot restore an old preview or overwrite the current catalog.

Acceptance: native PostgreSQL grant/update/revoke and effective membership,
restricted administrator versus unprivileged actor, explicit alternate grantors,
no-op/nonexistent/cycle/dependency errors, role replacement and membership or
actor changes after preview, pagination/version fences, read-only/schema policy
and confirmation refusal. Actual Vim keys must save drafts into preview commands,
show all options/grantor and reject stale replies. Run formatting, strict workspace
Clippy, workspace tests and live engine acceptance; graduate ADR-140 and update
the canonical partial feature inventory. Passwords, role attributes/lifecycle,
effective object permissions and reviewed cascade remain separately tracked.

References: PostgreSQL 17 [GRANT](https://www.postgresql.org/docs/17/sql-grant.html),
[REVOKE](https://www.postgresql.org/docs/17/sql-revoke.html), and
[pg_auth_members](https://www.postgresql.org/docs/17/catalog-pg-auth-members.html).

The bootstrap grantor check uses PostgreSQL's fixed BOOTSTRAP_SUPERUSERID (10),
not a role name; see the upstream [catalog bootstrap data](https://github.com/postgres/postgres/blob/REL_17_STABLE/src/include/catalog/pg_authid.dat).

Implementation evidence (2026-11-05): formatting and strict workspace/all-target
Clippy pass; workspace tests pass (1,716 passed, 5 ignored). All three native
PostgreSQL 17.10 membership/RLS/partition acceptance tests pass on Linux. The
existing actual Vim administration test now exercises membership catalog request/
context/generation refusal, three native focus targets, all option toggles, grant
and revoke submission, and explicit production confirmation. Native membership
acceptance verifies normal and restricted actors, options and effective usage/SET,
explicit alternate grantors, dependency/cycle refusal, member and equally
authorized grantor replacement, changed options and changed current actor.
Resource overflow and the older-version fence have focused regression coverage.
Native Windows/macOS acceptance remains deferred.
