# Provider and operations completion

Requested 2026-10-27. Deliver bounded, independently validated milestones and
commit each one. This plan supplements the canonical product inventory; a
milestone does not graduate its entire parent feature.

## Milestones

- [x] PostgreSQL extension updates: version-pinned preview/apply and Vim Monitor
  action, with stale-state rejection and explicit script-execution warning.
- [x] PostgreSQL partition attach and dependency previews: typed bounds, catalog
  identity checks, affected dependencies, reviewed SQL and native acceptance.
- [x] PostgreSQL historical deadlocks: operator-configured bounded log source,
  timestamp/session/resource projection, redaction, retention and audited reads.
- [ ] PostgreSQL rich-schema migrations: extend supported shapes individually
  with native round trips and inverse/explicitly irreversible plans.
- [x] SQL Server unsupported-value hardening: typed refusal before decoder panic,
  disposal of partially consumed connections, and native bulk rollback checks.
- [ ] SQL Server values: lossless sql_variant/UDT representation and binding
  design before driver changes; retain explicit refusals for unproven families.
- [x] Shared late-open ownership: abandoned opening results close their native
  handle exactly once and retain bounded admission, tunnel and tenant quota
  through pending cleanup (ADR-141), before disposable import writes.
- [x] SQL Server imports: reviewed Generate/Preserve identity and computed Omit
  modes, owned atomic writes, bounded progress, supervised cancellation and
  explicit unknown-COMMIT outcomes for CSV/XLSX/typed Parquet (ADR-142).
- [ ] SQL Server advanced DDL/migrations: round-trip each added native shape.
- [ ] SQLite schema changes: broader dependency-bearing rename/drop/alter and
  designer workflows, preserving dependencies, native constraints and root
  authority; rebuilds need rollback.
- [x] SQLite native table rename: accepted mappings, native declaration proof,
  transactional inverse, row preservation and stale/dependency refusal (ADR-118).
- [x] SQLite native column-rename backend: accepted mappings, coupled complete
  table proof, selective plans, chain/swap scheduling, transactional inverse
  and real-file preservation/refusal evidence (ADR-123). Designer controls and
  dependency-bearing shapes remain separate.
- [x] SQLite existing-column designer rename and typed table-rename previews:
  derive targets from verified live proof, exact SQL review, source/request
  binding and native preservation/refusal acceptance (ADR-124).
- [x] SQLite native ordinary-column removal: typed preview, coupled declaration/
  ordinal proof, transactional validation, plan-bound data-loss acknowledgement
  and native preservation/refusal acceptance (ADR-174). Broader dependency and
  general alter/rebuild shapes remain separate.
- [x] SQLite connected-main restore: same-root verified backup, atomic native
  copy, one-use review and explicit overwrite/backup acknowledgment, Vim
  review and native preservation/refusal acceptance (ADR-125).
- [ ] SQLite operations: cross-root recovery/delete, larger native streaming
  transfers, then individually designed ATTACH/extensions/functions/virtual-table write scopes.
- [x] Source-bound administration forms for existing PostgreSQL role/schema ACL/
  ownership actions and SQL Server role/membership/schema SELECT actions;
  broader login and privilege administration remains partial.
- [x] SQL Server ownership milestone: current user-database and schema transfers,
  native authority/permission-loss reviews, source-bound forms, stale identity
  refusal and native acceptance (ADR-121). External/implicit server-owner
  families remain a provider follow-up.
- [x] PostgreSQL RLS editor: typed policy definitions/roles and table controls,
  owner-state fingerprints, source-bound production review, native enforcement
  and audit privacy (ADR-120).
- [x] SQL Server role/grant native identity reviews: recreated principal,
  actor/database and schema-owner staleness, quoted permission names and
  multiple-grantor refusal, verified through real HTTP/SDK (ADR-122).
- [ ] Recovery: unified provider-aware UI, cross-instance identity mapping,
  externally orchestrated recovery and PostgreSQL cluster/PITR tooling.
- [x] External-provider savepoints: Driver RPC v2 negotiation, typed acknowledgments,
  serialized host state and transaction boundaries, uncertain-generation fencing,
  caller-abandonment ownership and executable host/server acceptance (ADR-151).
  This does not graduate native provider quality or the broader extension system.
- [x] Desktop savepoint synchronization: authoritative active names and failed
  condition, serial executor snapshots, pending ownership and stale-source refusal
  (ADR-152). Transaction query/end fencing remains a separate follow-up.
- [ ] Extensions/MCP: reconcile remaining runtime/contribution and governance
  gaps against shipped behavior, then complete each missing contract and UI.
- [ ] Distribution: platform builds, installers, signing and validation matrix;
  publisher identities and platform evidence must come from real artifacts.
- [x] Windows native build foundation: pinned compiler, strict workspace checks,
  native tests and generated desktop version/icon metadata (837ea9a7).
- [x] Windows portable development bundles: clean source archive, six native
  executables, payload/ZIP hashes and signature-status receipts (e45cca93).
- [x] Windows per-user development installer: verified staging, native
  install/uninstall and preservation/refusal acceptance (5f9f3c33).
- [x] Fresh personal desktop startup and typed activation readiness: explicit
  local migration authority and real-router regression acceptance (d2d8c4fe).
- [ ] AI execution: explicit run authority, side-effect policy, durable execution
  receipts and recovery; native Windows/macOS isolation requires native tests.

## Platform scope

The user supplied a separate Windows host for native packaging/isolation
acceptance on 2026-10-27 and explicitly deferred macOS. Earlier Linux acceptance used the selected remote host through the pinned Nix
flake; native Windows commands use the documented MSVC/Rustup workflow on its
own host. Current goal validation runs in the active Linux workspace with the
pinned Rust 1.96.1 toolchain. Host addresses
and credentials are operational inputs, not committed configuration. Deferred
macOS work must not be counted as validated cross-platform coverage.

Native Windows/macOS acceptance was explicitly deferred by the user on
2026-11-04. Continue Linux implementation and validation; retain unverified
native packaging/signing/isolation items as pending evidence, without claiming
cross-platform acceptance from Linux results.

## Gates

Design coupled protocol/server/desktop changes before implementation. Preserve
the locked Driver trait, pure serde wire types, supervised driver execution,
opaque secret handles, audited Operations and Vim-only controls. Use existing
preview/apply and production-confirmation paths. Keep unsupported cases explicit.

Each implementation commit records focused behavior coverage, formatting,
strict workspace Clippy and workspace tests. Native provider behavior also
needs existing opt-in live fixtures when available. Platform/signing, operator
log sources and external recovery environments are evidence prerequisites,
not results that Linux mocks can establish. CI remains manual-dispatch only.

## Extension update design

Add a typed UpdateExtension action with an explicit target version. Preview
reads installed state and the catalog-provided update path to that version;
missing installations, no-op targets and absent paths fail closed. Render the
version as a quoted identifier, never a SQL fragment. Fingerprint extension
identity, owner/schema/version and the update path so a changed catalog rejects
apply. PostgreSQL retains final ownership and script privilege enforcement.

Monitor `u` chooses the selected extension's displayed default version and
opens the existing exact-SQL preview. Enter confirms through the existing
audited apply operation; changing connection invalidates the pending preview.
The target remains explicit if the server's default changes after inspection.
Updates may execute extension scripts and acquire locks; no automatic update,
cascade, package download or version ordering inference is introduced.

Reference: https://www.postgresql.org/docs/current/sql-alterextension.html

## Evidence

Extension updates: a disposable PostgreSQL 17.10 cluster executed the exact
catalog-state query and updated hstore 1.4 to 1.8. The final installed version
matched the target; a non-owner role was denied by PostgreSQL. The cluster was
removed after the check. HTTP tests cover pinned SQL, explicit confirmation,
missing update paths, no-op targets and ownership changes between preview and
apply. Formatting, strict workspace Clippy and the complete workspace test suite
passed. ADR-111 records the stable contract. Broader PostgreSQL management stays
partial in the canonical inventory.

## PostgreSQL deadlock history design

Operators register server-local PostgreSQL JSON log files in driver config,
pinning each source to a managed tenant/profile, connection host/port and
database. HTTP callers cannot supply paths. Sources are disabled by default;
missing/mismatched sources fail explicitly. Schema-restricted profiles cannot
read history. The PostgreSQL identity must be superuser or have effective
pg_read_all_stats membership before a source is opened.

Read only the final 4 MiB of a regular file, skip partial/malformed/oversized
lines, select SQLSTATE 40P01 for the exact current database, and retain at most
50 summaries with at most 16 participants. Parse only known numeric resource
forms from deadlock DETAIL; never return message, statement, context, arbitrary
DETAIL or source paths. Accept UTC/numeric-offset timestamps; other log-time
zones are unsupported until an unambiguous conversion contract exists.

Use the existing audited ListDeadlocks operation, SDK and Monitor History view.
File work runs on bounded blocking workers with an absolute timeout, retaining
worker permits after timeout until IO ends. Recheck connection authority after
the read. No raw logs or summaries enter SQLite, no automatic server-log setup,
remote file fetching, log rotation management or durable archive is claimed.

Reference: https://www.postgresql.org/docs/17/runtime-config-logging.html#RUNTIME-CONFIG-LOGGING-JSONLOG

History validation: four focused tests cover source/privilege/database matching,
policy restriction after an allowed read, file/line/event/participant bounds,
timestamp decoding and SQL redaction. The opt-in native fixture starts a
disposable socket-only PostgreSQL cluster, creates a real deadlock using
explicit lock barriers and reads its generated JSON through managed history;
it passed. Formatting, strict workspace Clippy and the full workspace test suite
passed. ADR-112 records the stable source and projection contract.

## SQL Server unsupported-value boundary design

The vendored TDS decoder currently panics before it can describe sql_variant or
CLR UDT metadata. Add a dedicated UnsupportedType error before consuming their
type-specific metadata, map it to UnsupportedResultShape, and invalidate/drop
the physical connection after query or native bulk metadata failure. Never
restore an unread, partially decoded TDS response to the driver map or pool.
The failed logical handle reports ConnectionInvalidated until explicitly closed.
Other connections remain usable. Native acceptance covers variant (including
NULL) and geography metadata; no casts or stringification substitute for lossless
support. Full base-type tagging, collation/precision preservation and symmetric
bind encoding remain a separate protocol/driver design.

Hardening validation: all 12 live SQL Server driver tests passed serially,
including non-NULL/NULL sql_variant and geography refusals, stable invalidated
handles, independent connection success, and CSV-trigger result failure with
zero committed rows. Formatting, strict workspace Clippy and workspace tests
passed. ADR-113 records the boundary. Lossless values remain unchecked.

## PostgreSQL partition attachment design

Add a typed AttachPartition action for an existing ordinary child table and
partitioned parent. Bounds are default, range, list or hash; literal values are
data strings escaped as PostgreSQL E literals, with explicit MINVALUE/MAXVALUE
and NULL alternatives. No caller SQL fragments or casts enter generated DDL.
Bound counts, text sizes, hash modulus/remainder and parent strategy are checked.

Preview requires ownership of both tables, matching native column signatures,
and an unattached child. Fingerprint catalog identities, ownership, key shape,
table attributes, constraints, indexes, triggers and current parent attachments.
Capture role/search-path and date/time/interval interpretation settings too, so
literal coercion cannot silently follow a changed session configuration.
Return a bounded read-only impact list including indexes, constraints, triggers,
inbound foreign keys and the default partition; include its full digest in the
precondition even when displayed details truncate. Apply rereads state through
the existing audited preview/apply path. Native PostgreSQL still validates data,
overlap, dependencies and privileges; preview is not a lock or data snapshot.

Vim Monitor gets an attachment form with parent/child schema and table fields,
bound mode and values. Exact SQL, impact and lock/scan warnings precede explicit
apply. Clear drafts/previews on connection/view changes. The first supported
child shape is an ordinary table; foreign and partitioned children stay refused.
Strip bound values and native error text from operation audit; detailed native
errors remain available to the requester.
Native acceptance must exercise attach/detach, routing, changed-state rejection
and quoting without replacing the locked Driver trait.

The initial remote validation workflow used the user-selected SSH host and
pinned Nix dev shell. Current local validation uses the pinned Rust toolchain
and an external Cargo target cache so repository target cleanup does not remove
an active build. Source snapshots exclude local .env files, credentials, runtime
state and target artifacts. Native provider fixtures remain explicit gates;
Linux results do not establish native Windows/macOS acceptance.

Reference: https://www.postgresql.org/docs/17/sql-altertable.html

Partition attachment acceptance (2026-11-04): the disposable PostgreSQL 17.10
fixture passes typed attachment, insert routing, detach/data preservation,
stale catalog refusal, mismatched-column refusal and restricted-role ownership
refusal. The shared native deadlock fixture passes after its setup refactor.
Formatting, strict workspace Clippy and the complete workspace test suite pass.
This completes the ordinary-child attachment milestone, not general partition
creation, foreign/partitioned children or cross-object administration.

## Administration form design

Add source-bound Vim Monitor forms for existing typed PostgreSQL role creation,
schema USAGE/CREATE grant/revoke and database/schema ownership actions, plus
SQL Server role creation, role membership and schema SELECT grant/revoke.
Freeze instance, tenant, runtime connection and Monitor view at form open.
Invalidate on source/view changes, validate bounded names locally, and dispatch
only typed preview requests. Exact SQL and existing stale-state/production
confirmation gates precede apply. No direct SQL or credentials enter form state.
Tab cycles fields; Escape returns command focus then cancels; m cycles explicitly
labelled action modes and p previews. These forms do not imply login/password,
object/inherited privileges, RLS definitions or effective-permission support.

Monitor preview/apply commands and their replies carry frozen instance, tenant,
profile, query-session/connection identity and a unique request ID. The executor
checks the actual opened context before SDK access (administrative requests use
that context's separate metadata connection). The UI accepts replies only for
its current request and exact current source. View/connection changes invalidate
request identity, preventing late preview or apply replies from replacing a
newer review. An absent source fails closed before dispatch.

Administration-form acceptance (2026-11-04): tests prove typed names/native
bounds, exact target matching, stale request rejection, preview-only dispatch,
PostgreSQL explicit production confirmation, SQL Server membership review/apply,
and changed-source refusal. Compact scrollable forms retain the dock's usable
space. Formatting, strict workspace Clippy and the complete workspace suite
pass (601 workspace-UI tests passed, one existing test ignored). ADR-119 records
the contract. This adds no backend SQL/Driver capability or native acceptance
claim beyond existing administration APIs.

## PostgreSQL RLS editor design

Extend the existing audited PostgreSQL workbench action with typed policy
create, alter, drop and individual ENABLE/DISABLE/FORCE/NO FORCE controls.
Create selects ALL/SELECT/INSERT/UPDATE/DELETE and permissive/restrictive mode;
roles distinguish PUBLIC from quoted named roles. Require explicit bounded role
lists for create, optional replacement lists for alter, and one bounded parsed
PostgreSQL expression per USING/WITH CHECK input. No SQL fragments, bind values
or additional statements are accepted. Unsupported parser syntax fails clearly.
Policy editing requires standard_conforming_strings=on so canonical literals
retain their native interpretation; legacy backslash mode fails preview.
INSERT forbids USING; SELECT/DELETE forbid WITH CHECK. Alter preserves omitted
roles/conditions and native immutable command/mode; changing those properties
uses separately reviewed drop/create, never an implicit destructive replacement.

Owner-checked preview fingerprints table identity/ownership/columns, all native
policies, RLS flags, selected role identities and session interpretation state.
Create rejects existing names; alter/drop require an existing policy. Native
PostgreSQL performs final expression typing, dependencies and privileges. Every
action requires reviewed exact SQL and production confirmation. Warnings call
out access broadening, default deny, owner bypass and effects on future reads/
writes. Expression bodies and native error echoes are excluded from durable
audit; requesters retain detailed native errors. Reads/execution use existing
supervised deadline/cancel and policy paths, without changing Driver signatures.

Vim Monitor gets create/alter/drop/configuration forms bound to the same source
and request-identity mechanism as other administration. Never feed truncated
catalog expression/role displays back into edits. Native acceptance must prove
restricted-role row visibility and write checks, stale/owner refusal, SQL input
containment, production confirmation, table controls and audit redaction.

References: https://www.postgresql.org/docs/17/sql-alterpolicy.html and
https://www.postgresql.org/docs/17/ddl-rowsecurity.html

Linux acceptance uses PostgreSQL 17.10 through the real HTTP/SDK path:
`cargo test --workspace --features sift-server/live-pg --test postgres_rls_live`.
It verifies every command, permissive/restrictive composition, restricted reads
and checked writes, owner forcing, default deny, stale and owner refusal,
legacy string-mode refusal, quoted literal/statement containment and correlated
durable-audit privacy. Source-bound Vim form tests also verify blank ALTER
preservation, truncated-label safety and production confirmation.


## SQL Server ownership editor design

Add database/schema ownership to the existing audited security workbench and
source-bound Vim forms. Database ownership uses an existing server login;
schema ownership uses an existing database principal. They are different typed
kinds and never accept SQL fragments or credentials. The database action is
bound to the connection's current user database; system database changes and
native protected schema changes are explicitly refused. Show current database
owner when metadata can resolve it, otherwise label it unknown.

Preview verifies native TAKE OWNERSHIP and target-principal authority:
IMPERSONATE for users/logins, membership or ALTER for roles, ALTER for application
roles. Require database VIEW DEFINITION/security metadata for complete schema impact
review. Non-dbo/non-sysadmin contexts with effective metadata DENYs (including
role-token DENYs) are refused: a database grant does not prove every affected
object or grantee is visible.
Native SQL Server remains final authority. Do not quietly create users/logins,
regrant permissions or change TRUSTWORTHY/chaining settings. Existing login types
SQL and Windows-user are supported database owners; unresolved/external owner
families require separately proven provider handling.

Schema transfer can remove permissions on the schema and contained objects
whose owner is inherited. Count both explicit permission sets in the review,
warn about ownership chains and EXECUTE AS OWNER behavior, and fingerprint
schema identity/owner, contained object identities/ownership/modify state,
all affected permissions and target principal identity. Database reviews bind
database identity/current owner/target login identity and trust/chaining state.
Session identity and current authority participate in the state
fingerprint. Any relevant change requires a fresh review. Native DDL remains a
single supervised statement after fresh preview and production confirmation;
preview holds no locks. Existing read-only/schema policy, authorization and
audit paths remain authoritative. No Driver signatures change.

Acceptance uses a disposable native database and principals, with cleanup on
failure. Prove schema and database owner changes, schema/inherited-object ACL
loss versus explicitly owned objects, production refusal, stale identity/ACL
rejection, insufficient authority, protected/wrong-database refusal and quoted
identifier containment. Vim form tests prove correct principal kinds, source
binding and reviewed confirmation.

Reference: https://learn.microsoft.com/en-us/sql/t-sql/statements/alter-authorization-transact-sql?view=sql-server-ver16

Native SQL Server 2022 acceptance:
`cargo test --workspace --features sift-server/live-mssql --test sqlserver_ownership_live`.
The fixture uses `.env` connection settings and creates/disposes its own database
and logins. Passwords stay in process memory; setup failures report native codes
without credential SQL. All product calls use real HTTP/SDK and Driver paths.


## SQL Server role/grant review identity design

Existing role-creation, membership and schema-SELECT actions must bind the
native identities they review. Existence and permission flags alone are
insufficient: dropping/recreating a role or user can preserve those flags while
changing the principal SID, and USE/EXECUTE AS can retarget the same Sift
session/connection. Keep existing typed actions, authorization, production
confirmation and supervised execution; no Driver or wire changes are needed.

Extend catalog state with a native SHA-256 fingerprint of current server,
database identity/owner/collation, actor/login identity and the selected
principal identities (IDs, SIDs, types, owners and create/modify metadata).
Membership reviews also include the exact native membership. Schema-grant
reviews include schema owner identity and every metadata-visible SELECT ACL
entry for the selected grantee, including grantor identity. No credential,
password hash or principal secret is selected. Bound catalog predicates use
exact input names; native permission functions receive QUOTENAME identifiers.

Retain independent state checks for existence, authority and no-op transitions.
Refuse ambiguous multiple-grantor ACLs rather than choosing an arbitrary TOP
row or implying that a default-grantor REVOKE removes every grant. DENY and
unsupported states still need separate review; CASCADE remains explicit future
work and is never introduced implicitly. Native SQL Server remains final
authority, and preview holds no locks. Native evidence must prove refusal of
recreated members/roles/grantees and changed actor/database contexts while
normal reviewed mutations and least-privilege quoted-role actions still work.

Native regression uses the existing isolated security/ownership fixture:
`cargo test --workspace --features sift-server/live-mssql --test sqlserver_ownership_live`.
The initial run reproduced a recreated-member mutation under an old review;
the final run refuses it and covers all five role/grant action paths, native
context changes, least-privilege quoted-role editing and multiple grantors.

Catalog references: [principal identity and ID reuse](https://learn.microsoft.com/en-us/sql/relational-databases/system-catalog-views/sys-database-principals-transact-sql?view=sql-server-ver16)
and [permission states and grantors](https://learn.microsoft.com/en-us/sql/relational-databases/system-catalog-views/sys-database-permissions-transact-sql?view=sql-server-ver15).
