# SQLite native table rename milestone

Design before implementation, 2026-10-27. Extend the existing audited schema
comparison and migration lifecycle with explicit table renames, preserving data
and an exact inverse. Do not rebuild a table or change file/root authority.

Only native-proven simple ordinary `main` tables qualify. Reuse the verified
canonical CREATE statement and existing column checks. Normalize only the
quoted table-name prefix for rename compatibility; retain column declarations,
metadata and all other catalog properties. A caller must accept the rename
mapping explicitly. No structure-changing rename or database/schema move is
inferred.

Initially require unfiltered, untruncated captures of main/temp with only simple
tables and columns. Refuse views, triggers, constraints, indexes, virtual tables,
unknown nodes and incomplete dependency projections. This keeps native rename's
implicit dependency rewrites out of the first contract. Empty temp is allowed;
objects outside main and definitions lacking native proof are refused.

Render quoted ALTER TABLE main.old RENAME TO new and its inverse inside the
existing forced transactional group. Existing live-revision checks, test rollback,
production confirmation, read-only policy and supervised cancellation apply.
Native acceptance must preserve row values, roll validation back, apply the
rename, reject a stale plan and refuse dependency-bearing shapes. Broader
column rename/drop/alter and the designer remain separate milestones.

Reference: https://www.sqlite.org/lang_altertable.html

## Acceptance (2026-11-04)

The real-file HTTP fixture passes validation rollback and native rename/apply
with quoted names, unchanged rowids/values, stale-index rejection, and explicit
view/kind-filter refusal. Shared projection now keeps ancestors for selected
view columns instead of returning dangling graph references. All four SQLite
HTTP provider tests, formatting, strict workspace Clippy and the full workspace
suite pass. ADR-118 records the graduated boundary; broader migration work
remains partial.

## Column rename design (2026-11-04)

Extend the same backend migration contract to explicitly accepted column
renames within an unchanged ordinary main table. Preserve the full-table,
unfiltered native-proof and dependency refusal boundary above. Neither rebuild
nor implicit dependency rewrite is admitted by this milestone; broader shapes
and designer controls remain tracked separately.

Column rename matching ignores only the duplicated metadata name after checking
it agrees with the node name; type, ordinal, nullability, facets, completeness,
definition proof and other metadata must still match. The table's canonical
CREATE proof necessarily changes too. Couple selected column renames to that
table alteration and re-prove both complete declarations. When consuming the
companion table change, compare every column by ordinal: all changed names
must be accounted for by selected mappings and all other structure must match.
Column-only selections validate and render their individual mappings
without claiming that the entire target table definition was applied. Refuse
incomplete selections that claim the whole table change, simultaneous structure
changes to a selected column, table/schema moves and unproven captures. Never
rewrite SQL with string replacement.

Selecting the column mappings is enough: the verified table-proof change is
carried by native ALTER TABLE RENAME COLUMN and is not a second DDL statement.
When selected, the companion table change is consumed only after the same
proof succeeds. Multiple mappings share the table proof and render scheduled
quoted native statements; inverse statements run in reverse order. Existing
SQLite forced transactions, live revision checks, validation rollback,
confirmation, file/root authority, read-only and supervised cancellation remain.

Real-file HTTP acceptance must prove explicit mapping, multi-column quoting,
validation rollback, unchanged rowids/runtime values/storage classes, applied
rename and exact inverse, plus incomplete whole-table selection, stale-plan and
view/index/kind-filter refusal. This adds native migration support; standalone
SQLite designer and dependency-bearing rename workflows remain outstanding.

Column rename acceptance uses the existing real-file HTTP/SDK fixture:
`cargo test --workspace --test sqlite_provider`.
It proves two accepted quoted-name mappings with the companion table proof,
validates column-only selections and refuses incomplete whole-table selection,
validates by rollback, applies and reverses native changes, and compares rowids,
values and typeof storage classes
for text, blob and NULL cells. It also proves read-only refusal, stale-index
protection and independent view/index/kind-filter refusal (ADR-123).

### Overlapping rename names

Accepted, native-proven same-table SQLite column mappings reserve their column
identities before implicit path matching. This lets explicit logical mappings
represent chains and swaps instead of comparing a reused name to the wrong
old column. Other providers retain their existing matching precedence.

Render a deterministic schedule against the source column namespace. Rename
a column only when its destination is free (or its own case-only identity).
If a required source column is not selected, refuse with its missing rename
prerequisite. Break selected cycles with a deterministic collision-free
intermediate column name, visible in the reviewed SQL. Record every native step
and render its inverse in reverse order under the mandatory transaction. No
unselected column is renamed to make space. Native acceptance covers both a
chain and a swap, preserves logical values and
storage classes through their inverses, and refuses incomplete overlapping
selections.
