# Reviewed PostgreSQL role session defaults

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#postgresql).

Status: implemented; accepted as ADR-145.

Add public workload defaults to the existing role workbench: work_mem and
maintenance_work_mem (integer KiB), statement_timeout, lock_timeout and
idle_in_transaction_session_timeout (integer milliseconds), jit and
default_transaction_read_only (on/off), and max_parallel_workers_per_gather
(integer workers). This fixed typed set excludes secret-bearing/custom GUCs,
search paths, libraries and arbitrary settings. Numeric native ranges are checked
before review. Reset removes only the named setting; no RESET ALL or FROM CURRENT.

Scope is explicitly all databases or the connected database, never a supplied
remote database or ALL roles. Public role catalog detail shows only these eight
defaults at those two scopes, with bounded values. It does not return arbitrary
rolconfig/setconfig or claim effective inherited/current-session configuration.

Typed actions reuse audited PreviewPostgresObject/ApplyPostgresObject, their
production confirmation, schema-policy exclusions and supervised Driver query
path. Preview binds exact SQL, original session/connection, current database OID
and name, actor/session-actor identity and authority, target role identity and
attributes, selected scope and observed setting. Apply repeats the native snapshot;
role replacement, changed value, changed actor or lost authority refuses dispatch.
This is optimistic review, not a native catalog lock.

Follow native authority: superusers may manage any non-reserved named role;
ordinary actors may set their own defaults; CREATEROLE plus ADMIN may manage
other non-superuser roles. No SET ROLE or privilege elevation is performed.
Values affect new logins, not current sessions or SET ROLE. Database-specific
role defaults override global role defaults; RESET falls back to other defaults.
These are workload defaults, not durable security or resource-limit enforcement.

Vim Roles adds set/reset modes to the existing source-bound role form. Show scope,
setting, native units and the observed value in exact-SQL review; dispatch remains
explicit. Tests must prove native set/reset, role-vs-database precedence on fresh
logins, unchanged current sessions/SET ROLE, own-role and delegated authority,
denial/revocation, stale values/identities, and no arbitrary GUC disclosure. Real
GPUI mode/preview/confirmation tests and fmt, strict workspace Clippy and full
workspace tests remain required. Windows/macOS acceptance stays deferred.

PostgreSQL pool reuse is not a fresh native login: logical close/open can retain
the prior native session defaults. Review discloses this; changes do not recycle
active/idle pool sessions. Native acceptance uses new psql processes to prove
login behavior, alongside SDK checks for unchanged existing sessions.

## Verification

Linux PostgreSQL 17.10 acceptance passes through the existing HTTP/SDK fixture:
all eight settings set/reset with production confirmation; ordinary self and
CREATEROLE+ADMIN authority; denial and ADMIN revocation; actual new-login global/
database precedence; preserved existing sessions and SET ROLE; reset fallback;
stale value, authorized actor change and role replacement; custom-GUC privacy.
Native login checks use new psql processes to avoid confusing pool reuse with
authentication. The existing real GPUI regression now exercises set/reset modes,
scope toggle, typed previews, explicit confirmation and changed-source refusal.

Formatting, strict workspace/all-target Clippy and all 1,727 workspace tests pass
(five existing ignored). Public GUC validation rejects unsupported names, SQL/
unit expressions and out-of-range native values before querying. Credentials,
arbitrary GUC management and automatic pool recycling remain outside this slice;
the broader product goal remains active and Windows/macOS acceptance is deferred.
