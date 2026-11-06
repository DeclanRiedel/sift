# SQLite native column removal

Design: extend catalog diff/migration and the Vim table designer with native
ALTER TABLE main.table DROP COLUMN for catalog-proven ordinary main tables.
Use the existing complete main/TEMP simple-table proof and declaration witness;
retain explicit refusal of indexes, constraints, generated/virtual/hidden fields,
STRICT/WITHOUT ROWID tables, filtered/truncated/stale catalogs and dependency
coverage gaps. Never rebuild a table in application code or edit sqlite_schema.
The SQLite native command rewrites stored records and permanently removes the
selected column's cells. A backup is needed to recover committed removed data.

Add a pure-serde DropColumn diagram intent, initially supported only for SQLite.
Derive its private target by verifying the full original declaration, removing
only the selected column and its containment edge, preserving sibling identity
and order while renumbering remaining ordinals, and regenerating the structured
canonical declaration. Refuse the last column and any non-containment edge to or
from the target. No live SQL is executed during target construction.

Migration rendering must couple removed columns, the table declaration change
and only the induced ordinal shifts. Prove both complete declarations and compare
all surviving column facets with original identity/name/order, ignoring only the
expected ordinal compression. Refuse mixed rename/add/type/nullability/reorder
changes. A whole-table/ordinal companion selection requires every associated
removal. A column-only selection may remove a subset, retaining unselected
columns. Deterministically emit quoted native DROP COLUMN statements, each with
DataLoss risk and no inverse SQL; the plan explicitly warns that data cannot be
recreated by adding a column. Transactional validation still rolls its own work
back, and supervised transactional apply preserves the existing timeout/cancel,
policy, live-revision and one-use plan guards. Cancellation is checked again
before COMMIT so cancellation during the final statement rolls the transaction
back at its next safe boundary.

The Vim designer provides an explicit preview-removal action for a selected
existing SQLite column. It does not dispatch apply. Exact SQL and data-loss
warnings remain visible in DDL review. Applying a data-loss plan requires an
explicit acknowledgement bound to that plan before the existing Apply action;
changing source/review invalidates acknowledgement. Retain all existing pending,
request-generation, source/session/connection and production confirmation guards.
No Driver signature or new Operation is needed: typed designer intent uses the
existing audited migration preview/validation/apply operations.

Acceptance: real-file HTTP/SDK coverage for preview without mutation, native
validation rollback, quoted first/middle/last column removals, rowid/remaining
value/storage-class preservation, multi-column selection/ordinal coupling,
last-column and mixed-shape refusal, independent dependency/filter refusal,
missing data-loss acknowledgement, read-only and stale plans. Exercise actual Vim
preview/acknowledgement/apply and stale/duplicate dispatch guards. Run formatting,
strict all-target workspace Clippy and full tests under the persistent 10%
Rust/Cargo resource cap; record native evidence and update product/SQLite docs.
Native Windows/macOS acceptance stays deferred.

Reference: [SQLite ALTER TABLE DROP COLUMN](https://www.sqlite.org/lang_altertable.html).


## Acceptance evidence (2026-11-07)

Implemented typed DropColumn targets, complete simple-catalog absence proof,
coupled migration rendering and the source-bound Vim preview/acknowledgement/
Apply path. Broader dependency-bearing/general alter/rebuild shapes remain open.

The real-file HTTP/SDK migration fixture passed first, middle and last quoted
column removals. Preview left the file schema unchanged; native validation
executed the removal and restored it by transaction rollback. Apply retained
rowids and exact remaining values/storage classes across TEXT, INTEGER, BLOB
and NULL cells. Native snapshot targets proved deterministic multi-column
removals, selective column-only validation, whole-target ordinal coupling and
refusal of mixed surviving-column type changes. Last-column removal, read-only
validation/apply, stale/index/view dependencies, missing DataLoss acknowledgement
and replay were refused. No automatic inverse SQL was emitted.

The core behavior test proved immutable target derivation, surviving identities
and ordinal compression, DataLoss/Lossy classification and unknown absence for
filtered, truncated, stale, unparsed or missing-witness catalogs. The Vim test
used Enter for preview, Space for explicit acknowledgement and Tab/Enter for
Apply. It refused inherited acknowledgement after plan replacement, changed
sessions and duplicate pending dispatch. Initial keyboard acceptance exposed
workspace Space interception; focused controls now own these keys before the
workspace Vim handler.

A migration lifecycle regression holds the dispatched final statement on an
explicit Notify gate until cancellation is acknowledged. It passed with both
statement outcomes rolled back, Canceled state, a rollback invocation and no
COMMIT invocation. This verifies the new safe boundary before COMMIT without a
timing assumption. Cancellation after commit dispatch remains subject to the
existing uncertain-outcome contract.

Rust 1.96.1 formatting and strict all-target workspace Clippy passed with locked,
offline dependencies and `sift-server/live-mssql` enabled. The full workspace
suite passed 1,808 tests in 97 groups, with zero failures and six existing ignored
tests, including the real-file SQLite and enabled native SQL Server fixtures.
All Rust/Cargo work used the shared `app-sift-rust.slice`: CPUQuota=160% on the
16-logical-CPU host, MemoryMax=3,287,567,155 bytes, no swap, one CPU affinity,
one Cargo job, one test thread and nonincremental compilation. Test package
codegen units remained server/desktop=512 and UI/workspace-UI=256 with debug=0;
normal UI optimization was retained. The final service exited successfully with
status 0 and memory peak 3,267,006,464 bytes. Phase evidence is retained under
`~/.cache/sift-goal-logs/sift-sqlite-drop-verified-*.log`.

No desktop artifact was regenerated, and no Windows/macOS native acceptance is
claimed. Those hosts remain explicitly deferred. The parent SQLite and broad
product inventories remain partial.
