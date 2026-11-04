# Ordered XLSX and Parquet upload projection

Design before implementation. Extend ordered import selection beyond CSV through
format-version-2 XLSX and Parquet upload recipes. Keep existing version-1 imports
and CSV v1 rename/v2 projection behavior. Existing audited recipe save/validate/
execute operations and generic Vim JSON editor/upload runner remain the workflow.
No Driver signature, wire shape, schema migration, environment or CI change.

Move pure option parsing and exact source-header resolution into core. An import
v2 column_projection is an ordered array of {source,target}; omission drops fields,
repetition copies, order is output order. Legacy column_mappings remains partial
rename without projection. The options are exclusive; cap explicit selections at
256 outputs, source names at 4096 bytes, distinct trimmed target names at 128 bytes.
Resolve one exact source name; refuse missing/ambiguous selected sources. Preserve
legacy CSV semantics and reject mapping on exports, unsupported formats or wrong
versions. Apply the same contract in backend and local Vim recipe validation.

XLSX retains explicit sheet selection and its existing text-cell extraction/
CSV conversion semantics, including existing importer null/type rules. Run ZIP/XML
conversion off Tokio. Bound encoded input, total declared expanded parts and CSV
emission to 64 MiB, bound ZIP entry count/XML nodes/cell positions, and limit actual
part reads. Project before inference, dry-run, quarantine or target writes.
Shared-string expansion must fail at the writer bound rather than allocate an
unbounded intermediate. No formula evaluation or new Excel type interpretation.

Parquet retains typed flat-value import, including NULL versus empty text/binary,
integer width, floating values and field metadata. Resolve selection against the
original Arrow schema, project unique source roots in the reader, then duplicate/
reorder values according to outputs. Do not round-trip through CSV or decode omitted
unsupported nested fields. Validate all source names and original source resource
bounds, and require supported scalar types only for selected fields. Bound decoded
input/output and copy expansion to 64 MiB. Preserve original row order, nullable
metadata and existing transactional apply/rollback behavior. Dry-run returns the
same projected columns/row count without target writes. Reject Parquet type
overrides/resume/quarantine rather than ignore them. SQLite target names must be
unique under its native ASCII identifier folding, including CSV-backed XLSX, to
prevent multiple outputs binding one physical column. Other providers retain
native destination validation.

Metadata and API builtin format-version gates admit import v2 for all three formats;
export v2 stays refused. Generic Vim recipe save validates the shared options and
shows examples plus typed/text differences. Existing revision/source/request
fences, local upload bounds, dry-run controls and durable CSV checkpoint identity
remain. Parquet resume and XLSX durable resume stay unsupported; existing stateless
XLSX resume through the CSV importer remains available.

Acceptance covers shared option/version/source failures, rename compatibility,
selected/copy/reorder semantics, typed Parquet NULL/text/binary values and omitted
nested columns, copy expansion, bounded shared-string XLSX output and invalid cell
positions, native SQLite dry-run/apply/rollback and target-alias refusal, API format
validation and actual Vim recipe save/dispatch. Run formatting, strict workspace
Clippy and full workspace tests, graduate ADR-139, update inventories and commit.
Expressions, direct non-CSV column-grid UI and wider sources remain separately
tracked. Native Windows/macOS host acceptance remains deferred.


Implementation evidence (2026-11-05): native Linux acceptance passes against
PostgreSQL, SQL Server and real-file SQLite, including persisted v2 recipes,
projected preview/apply and rollback after a later-row conflict. Actual Vim keys
exercise options focus, Escape back to modal commands, and save dispatch for both
formats. Codec tests cover typed NULL/empty/copy behavior, omitted nested roots,
copy expansion and XLSX shared-string/cell bounds. Direct PostgreSQL live coverage
also verifies terminal errors before and during row production. SQLite close
completion releases the connection slot before an immediate reopen.

Final validation: cargo fmt, strict workspace/all-target Clippy and the complete
workspace suite pass (1,714 passed, 5 ignored). The live import fixture passes on
all three engines, and the direct PostgreSQL terminal-error regression passes.
