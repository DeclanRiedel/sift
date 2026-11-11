# Exact decimal transfer expressions

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#imports-and-transfers).

Status: implemented and verified on Linux with all three database engines.

Extend bundled upload-import recipe v3 expressions with deterministic exact decimal
arithmetic. All computed outputs remain nullable text, preserving the v3 type and
CSV/XLSX null-marker contracts. Bare references remain unchanged. Native destination
types and supported explicit type overrides govern storage. No SQL or Driver API
changes are required.

Add `decimal`, `negate`, `abs` with `input`; `add`, `subtract`, `multiply` with
`left`/`right`; `divide` with `left`/`right` and required `scale`; `round` with
`input` and required `scale`. Scales range from 0 through 18. Decimal inputs accept
signed base-10 text with optional fractional part, and native signed integers.
Reject whitespace, exponents, NaN/infinity, booleans, binary and implicit float
conversion. Explicit `text` may render a float before decimal parsing if desired.
NULL propagates; empty text is invalid. Errors omit operands and literals.

Canonical results remove redundant leading and fractional trailing zeros and
normalize negative zero. Bound each decimal to 38 significant coefficient digits
and scale 18 after normalization. Parsing accepts at most 128 bytes. Use bounded
arbitrary-precision intermediate coefficients (no larger than roughly 76 digits)
to avoid spurious machine-integer overflow during aligned addition, multiplication
or division. Exact add/subtract/multiply refuse unrepresentable results. Divide
and round use the requested fractional scale, with ties rounded away from zero.
Division by zero and precision/scale overflow fail before native writes.

Reuse existing expression depth/node/copy/output budgets. Enforce scale at recipe
validation and bound parsed/generated coefficients independently of text cell size.
CSV checkpoints already bind original data, recipe revision and computed-text
metadata, so arithmetic recipes use the same resume identity rules.

Acceptance covers signed ties, division, exact decimal addition, cancellation to
zero, integer limits, scale/precision overflow, invalid values and NULL behavior.
Extend existing CSV/XLSX/Parquet native acceptance to verify computed decimal text,
pre-write invalid operand refusal, quarantine and checkpoint replay through SQLite,
PostgreSQL and SQL Server. Extend Vim JSON help and shared recipe validation.
Run formatting, strict workspace/all-target Clippy and full workspace tests, then
live transfer acceptance, under the persistent 10% Rust/Cargo resource cap. Native
Windows/macOS acceptance remains deferred. Dedicated non-CSV review grids and
additional source formats remain open.


## Validation evidence

- Focused exact-decimal evaluator test passed. Cases cover exact 0.1 + 0.2,
  positive/negative ties, all division sign combinations, integer boundaries,
  canonical zero, precision/scale overflow, invalid inputs and NULL propagation.
- Formatting and strict workspace/all-target Clippy passed.
- Full Linux workspace after native schema fix: 1769 passed, 6 ignored. SQLite transfer acceptance covers
  computed decimal preview/apply, native decimal target values, invalid-operand
  refusal without writes or operand disclosure, quarantine and CSV resume.
- Extended existing Vim recipe-save acceptance preserves division and scale in
  CSV/XLSX/Parquet v3 recipes.
- Rust/Cargo validation uses one build job/test thread, aggregate CPU quota 160%
  on the 16-thread host, memory 3,287,567,155 bytes and no swap (10% host limits).
- Strict workspace/all-target Clippy passed both with and without live database
  features. Live SQLite/PostgreSQL/SQL Server transfer fixtures all passed after
  correcting native SQL Server decimal type preservation. Assertions retained
  decimal fractions through actual destination storage and checkpoint replay.
- Existing SQL Server native DDL acceptance: all 3 selected fixtures passed,
  including advanced numeric columns/types and constraint-state migration.
- Windows/macOS acceptance remains explicitly deferred.

## Native acceptance finding and design amendment

Live SQL Server checkpoint acceptance exposed loss of decimal scale: schema
introspection flattened decimal(38,18) to Primitive Decimal, and resumed inserts
cast text through bare `decimal` (SQL Server defaults scale to zero). Direct bulk
imports retained the fraction. Fix native schema fidelity rather than weakening
the arithmetic fixture or accepting rounded checkpoint values.

Both SQL Server deep schema paths must preserve decimal/numeric precision and
scale as the existing TypeRef::Native Numeric shape. Retain money/smallmoney as
native numeric names too. Validate native precision/scale before constructing the
SQL type. Query-result primitive rendering and Driver signatures stay unchanged.
Existing target-type casting and checkpoint identity then include the actual
native decimal definition. Old checkpoints whose digest used the lossy generic
type safely refuse replay rather than silently changing their interpretation.
Add meaningful type rendering checks and rerun workspace and native acceptance.
