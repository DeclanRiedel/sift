# Portable transfer text expressions

Status: implemented and verified on Linux with SQLite, PostgreSQL and SQL Server.

## Contract

Add bundled upload-import recipe format version 3 for CSV, XLSX and Parquet.
`column_expressions` is an ordered array of `{target, expression}` outputs, mutually
exclusive with legacy rename/projection options. Version 1/2 behavior stays stable.
Direct column references preserve Parquet primitive types; all computed expressions
produce nullable text. Internal CSV preparation retains computed text types,
including numeric-looking values such as `"001"`; checkpoint identity includes
computed-text column metadata without changing legacy digests. Portable expressions cover column, literal, null, text,
trim, upper, lower, concat, coalesce, replace and Unicode substring. They never
execute SQL or external code. Existing Vim recipe JSON editing is the editing and
review surface; dedicated non-CSV visual grids and numeric expressions remain
open parts of the full mapping goal.

Resolve every referenced header exactly once; reject unknown/ambiguous selected
headers. Preserve output order and explicit source reuse/drop. Limit output count
to 256, source names/literals to 4096 bytes, expression depth to 12, nodes per output
to 128 and total expression nodes to 4096. Limit computed cells to 1 MiB and the
whole transformed payload to the existing 64 MiB limit. A per-import evaluation
budget limits node visits and copied text bytes, including intermediate values.
Evaluate and validate the entire upload before opening importer/native write work.
Malformed input, unsupported types, output expansion and budget exhaustion fail
without database writes. Error messages omit source values and expression literals.

Text operations propagate NULL. Concat propagates any NULL; coalesce chooses the
first non-NULL value. Scalar text rendering is deterministic for supported finite
numeric/boolean/text values; binary requires a direct reference. Substring indices
count Unicode scalar values from zero. Empty text remains distinct from NULL.

CSV/XLSX version 3 accepts an explicit nonempty `null_value` marker (1–128 bytes),
defaulting to the existing NULL token. The same marker interprets source fields,
encodes expression NULL and configures the native importer. Refuse computed text
that equals that reserved marker rather than silently turning text into NULL;
the user can choose another marker. Parquet keeps typed NULL and text separately.
Legacy recipes cannot silently opt into this new null policy.

Parquet evaluates typed rows after reading only referenced roots. Output fields
retain source type/nullability for bare references and use nullable UTF-8 for
computed text. Literal-only expressions preserve metadata row counts without
requiring unrelated unsupported roots. Bound source decoding and transformed
output separately. Existing provider-specific destination/type/policy/native
validation stays authoritative.

CSV resume binds original upload digest for expressions, in addition to recipe
identity/revision and transformed payload, just like version-2 projection. Retrying
changed dropped fields must fail even if transformed output is identical. Preserve
logical row order/count for offsets and quarantine.

## Acceptance

Extend shared selection/evaluator, server format and native transfer fixtures.
Cover Unicode, NULL/empty/literal-marker collisions, multiple references and copies,
constant-only rows, typed Parquet preservation, malformed/ambiguous expressions,
bounded expansion and evaluation work. Preview, create-table inference, import,
quarantine and saved recipes must share the same outputs. Exercise PostgreSQL,
SQL Server and SQLite native transfer acceptance and CSV resume changed-source
refusal. Keep format 1/2 regressions green and extend Vim recipe validation/help.
Run formatting, strict workspace/all-target Clippy and workspace tests under the
10% host CPU/RAM cap. Native Windows/macOS acceptance stays deferred. Protocol
stays pure serde; locked Driver signatures stay unchanged.


## Validation evidence

- Formatting and strict workspace/all-target Clippy passed.
- Focused expression tests passed, including grammar, Unicode, NULL/empty values,
  bounded work/expansion, typed Parquet references and numeric-looking CSV text.
- Full Linux workspace: 1767 passed, 6 ignored. Existing SQLite transfer acceptance
  now exercises v3 CSV/XLSX/Parquet preview/apply, literal NULL, computed `"001"`,
  unknown-source refusal, quarantine and checkpoint replay/source-change refusal.
- Rust/Cargo used one build job and one test thread, with aggregate 160% CPU quota
  on this 16-thread host, 3,287,567,155-byte memory cap and no swap (10% host limits).
- Strict workspace/all-target Clippy also passed with `sift-server/live-pg` and
  `sift-server/live-mssql`. Four existing feature-gated native DDL assertions were
  simplified to satisfy current Clippy without changing their behavior.
- Live transfer acceptance: all 3 SQLite/PostgreSQL/SQL Server fixtures passed,
  covering v2 regressions plus v3 CSV/XLSX/Parquet preview/apply, numeric-looking
  text, Unicode/NULL/empty values, quarantine and CSV checkpoint behavior.
- Native Windows/macOS acceptance remains explicitly deferred.
