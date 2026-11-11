# SQLite native table rename milestone

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#sqlite).

Implemented contract. Extend the existing audited schema
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

## Designer rename design (2026-11-04)

Connect the existing typed RenameObject designer intent to the proven SQLite
migration path. Derive an ephemeral target only from a live, unfiltered native
capture. Verify the original simple table declaration against its ordered
column metadata before changing names; rebuild only its canonical proof text
from structured names and existing facets, never by replacing SQL substrings.
Keep duplicate column metadata names consistent. Never execute DDL to discover
a preview target. The migration renderer must still validate both catalogs
under the existing dependency/refusal boundary before storing a plan. Other
SQLite mutation kinds remain refused until separately implemented.

The Vim table designer exposes existing-column renames with read-only type and
nullability displays; adding/reordering columns is unavailable for SQLite.
Retain exact SQL review and explicit apply. Bind preview/apply requests to the
actual instance, tenant, profile, session and connection with a unique request
identity, and ignore superseded replies. Changes to the draft invalidate its
review. Load table definitions with the same default graph options used by
preview: filtered or definition-enriched captures have distinct revisions.
SQLite therefore retains the same complete main/TEMP evidence. Native tests must cover
preview without mutation, apply/inverse preservation, stale/read-only refusal
and dependent/unsupported-shape refusal; UI tests must exercise typed rename
dispatch, unsupported edits and stale request refusal.

Designer acceptance extends the existing real-file HTTP fixture and focused
Vim designer tests. Native table and column previews leave data untouched,
validation rolls back, and apply/exact inverse preserve rowids, values and
storage classes. Quoted SQL-looking names and case-only renames round-trip;
ASCII-insensitive collisions, NUL names, unsupported mutations/constraints,
views, indexes, read-only writes and stale plans are refused. UI acceptance
proves typed preview-only dispatch, unavailable add/reorder/nullability edits,
exact reviewed apply, changed-session refusal, superseded reply rejection and
duplicate-apply prevention. Existing additive and object-designer tests pass.

Linux validation passed: `cargo fmt --all`, strict workspace Clippy and
`cargo test --workspace`, plus the focused native SQLite and designer runs.
Native Windows/macOS acceptance remains deferred by the user.


## Subsequent nullable-addition milestone

ADR-149 adds reviewed appended nullable columns under the same native simple-table
proof and source guards. The Vim new-column row is now available with a fixed
nullable state and a whitelisted native type. Historical statements above that
all additions are unavailable describe the earlier rename milestone; existing
column type/nullability/reorder and dependency-bearing shapes remain refused.
See [addition design and acceptance](sqlite-add-columns.md).
