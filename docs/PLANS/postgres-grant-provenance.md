# Native PostgreSQL ACL provenance

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#postgresql).

Status: implemented and verified on Linux with native PostgreSQL 17.10.

Extend the selected-role privilege matrix with optional per-object ACL provenance.
Cover every currently listed family: connected database, visible schemas,
tables/views/foreign tables, columns, sequences and routines. Existing native
has_*_privilege checks stay authoritative; ACL rows are evidence, not a replacement
access evaluator. Preserve actor visibility, policy checks, source binding,
read-only audited operation and the existing object page/response limits.

Expose owner OID/name, whether the stored ACL is NULL and therefore expanded using
PostgreSQL's acldefault, and bounded aclexplode entries with grantor/grantee OIDs,
optional role names, privilege and grant option. Grantee OID zero means PUBLIC;
an unknown name remains unknown rather than guessed. A native grantee-access flag
uses PUBLIC/direct role equality/pg_has_role(subject, grantee, USAGE), including
native superuser semantics. It does not enumerate membership or grant-chain paths.
Owner inherent grant options and table-to-column privileges remain distinguished
from ACL rows. An explicit empty ACL must not become a default ACL.

Add a serde-default optional ACL object so clients can decode older servers and
show provenance unavailable instead of inventing an empty catalog. Native SQL
adds provenance only after selecting the existing bounded object page. Slice at
most 65 native ACL items before expansion, return at most 64 sorted grant rows per
object and disclose truncation if either bound is exceeded. Existing supervised
native timeouts and the 1 MiB response bound still apply; a user may reduce the
object page size. Do not return raw ACL strings, SQL definitions or protected data.

Vim g toggles a scrollable provenance view for the selected object, j/k chooses
objects and existing paging/refresh/Escape keep their source-bound behavior.
Display native/default/empty/unavailable states, owner, grantor, grantee, privilege,
grant option, subject-grantee access and explicit incomplete warnings. Refresh
must replace provenance with the same correlated response as native checks.

Extend existing PostgreSQL 17 live membership/matrix acceptance for direct,
inherited, non-inherited and PUBLIC grants; delegated grantors; NULL versus empty
ACLs; default function execute; table/column distinctions; bounded truncation;
native revocation and connected-actor visibility. Extend existing GPUI source and
keyboard acceptance. Run formatting, strict workspace/live-feature Clippy and full
workspace tests under the persistent 10% Rust/Cargo cap. Driver signatures and
protocol purity remain unchanged. Grant/revoke editors, explicit membership-path
and grant-chain analysis and additional privilege families stay open. Native
Windows/macOS acceptance remains deferred.

Native semantics reference: PostgreSQL 17 system-information functions,
https://www.postgresql.org/docs/17/functions-info.html (acldefault, aclexplode,
pg_has_role and native privilege checks).


## Validation evidence

- Direct PostgreSQL syntax/shape probe passed. Empty ACL arrays explicitly bypass
  aclexplode; a NULL input emits zero rows, preserving NULL/default versus stored
  empty state without a zero-dimensional-array error.
- PostgreSQL 17.10 HTTP/SDK membership acceptance passed with the existing
  disposable cluster. It covers grantor delegation, native grantee-role USAGE,
  PUBLIC and default EXECUTE, owner inherent options with an explicit empty ACL,
  table privileges without copied column ACL rows, database/schema/sequence and
  overload identities, 64-row truncation, cascading native revocation, actor
  visibility, unchanged SET ROLE context and successful audited inspection.
- Strict workspace/all-target Clippy passed with live PostgreSQL enabled.
- Final full Linux workspace: 1770 passed, 6 existing ignored. Formatting and
  strict workspace/all-target Clippy passed both with and without live PostgreSQL.
- GPUI acceptance passes g provenance toggling, shifted J/K grant
  navigation, j/k object selection, older-server unavailable states, escaped role
  names, source-change refusal and stale/late reply handling. Final acceptance uses
  the repository's normal UI optimization setting, with capped-build codegen/debug
  overrides.
- Rust/Cargo remained limited to one build job/test thread, aggregate 160% CPU on
  the 16-thread host, memory 3,287,567,155 bytes and no swap (10% host limits).
- Native Windows/macOS acceptance remains deferred.
