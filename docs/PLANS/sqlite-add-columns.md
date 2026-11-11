# SQLite native nullable column additions

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#sqlite).

Status: implemented and verified on Linux (ADR-149). Native Windows/macOS
acceptance remains deferred.

Extend the existing audited diff/migration and typed designer preview/apply with
native ALTER TABLE main.table ADD COLUMN for appended ordinary nullable columns.
Support INTEGER/REAL/TEXT/BLOB/NUMERIC declarations without defaults, keys,
constraints, generated expressions, hidden/virtual fields or type parameters.
Existing rows acquire NULL; no table rebuild, row-value conversion or file/root
change is introduced. The whole-catalog native simple-table proof, unfiltered
main/TEMP capture, dependency refusal, live revision check, supervised execution,
mandatory transaction, validation rollback and explicit apply remain required.

Couple added column changes to the verified table declaration change. Re-prove
both complete declarations, unchanged original columns and table identity/shape,
and appended ordinals. A selected companion table alteration requires all new
columns; column-only selection may add a proven prefix, never skip an ordinal.
Refuse mixed rename/type/nullability/reorder changes on that table. Derived
preview targets must verify the live canonical declaration before constructing
new column facets and its replacement proof from structured fields. No SQL text
substitution or native mutation is used to obtain a preview target.

Adding a column is not data-reversible after clients populate it. Mark SQLite
column additions lossy for rollback and emit no automatic DROP COLUMN inverse;
warn in the reviewed plan. Validation still rolls its owned transaction back
without deleting persisted user data. Dropping a populated column needs a
separate future destructive review. Do not infer an exact data rollback merely
because the original table definition could be reconstructed.

The Vim designer adds a new nullable column with a whitelisted type; existing
columns retain rename-only controls. Type/nullability/reorder changes to existing
SQLite columns remain refused. Existing source/request/form guards and exact SQL
review apply. Native real-file acceptance must prove preview without writes,
validation rollback, appended NULLs, rowid/value/storage-class preservation,
read-only and stale refusal, incomplete selections and dependency/shape refusal.
GPUI acceptance must prove typed add preview and unchanged existing edit guards.
Formatting, strict Clippy and the full workspace suite must pass. Native Windows/
macOS acceptance remains deferred.


## Acceptance evidence

The existing real-file HTTP/SDK migration fixture passes all five native type
choices with quoted SQL-looking names. Preview leaves column count unchanged;
validation executes then rolls its transaction back. Apply appends NULLs while
preserving rowids, original runtime values/storage classes and populated cells
from earlier additions. Read-only validation/apply, stale plans and independent
index/view dependencies are refused. Invalid names/type fragments and new NOT
NULL declarations are refused before execution.

A native two-column target snapshot proves coupled whole-table selection,
selective-prefix preview, refusal of skipped ordinals/incomplete whole-table
selection, ordered native statements, validation rollback and final NULLs.
Column additions report lossy data rollback and no automatic inverse SQL.
Existing rename exact-inverse acceptance remains green. The Vim designer test
retains stale/superseded/changed-session and duplicate-apply guards, proves typed
nullable add preview, and keeps nullability/reorder controls unavailable.

All 1,734 workspace tests pass (five existing ignored), including all four
real-file SQLite HTTP tests and the designer acceptance. Formatting and strict
workspace/all-target Clippy pass. General alter/rebuild/drop and dependency-bearing
schemas remain outside this milestone; the full product goal remains active.
