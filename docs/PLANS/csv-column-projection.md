# Ordered CSV recipe column projection

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#imports-and-transfers).

CSV import recipes at format version `2` gain `column_projection`: an ordered nonempty array of
`{"source":"exact CSV header","target":"destination column"}` records.
Only listed columns survive, in array order; repeated source entries explicitly
copy a field to distinct targets. Version 1 remains valid for legacy imports. Existing `column_mappings` retains rename-only
behavior and cannot be combined with projection. Reserve both options for CSV
upload imports; reject unsupported direction/format rather than ignoring them.
No expressions, arbitrary SQL or implicit value coercion are introduced.

Validate shape, counts and target names on create/update/validate and again on
execution. Cap projections at 256 outputs, source names at 4 KiB and destination
names at 128 bytes. Source lookup is exact and must resolve one header; unselected
ambiguous headers can be dropped. Explicit targets must be distinct. All source
records must still parse at the original width before any importer starts.
The existing destination/type validation remains authoritative per provider.

Parse and rewrite CSV on a blocking worker. Preserve record order, field text,
empty values, quoting semantics and multiline/Unicode fields. Bound the writer
as bytes are emitted, so source duplication cannot allocate an expanded result
above 64 MiB. Reject malformed input or output expansion before database work.
The existing importer then sees only projected columns and values: dry-run
inference, sample review, type mapping, conflict handling and quarantine all use
the same projection. Dropped row values must not appear in samples/quarantine.

Durable projection imports bind the original upload digest into checkpoint scope
alongside existing recipe identity/revision/actor and mapped-payload validation.
Changes to dropped source fields must invalidate resume, even if mapped output
is identical. Legacy rename/no-mapping checkpoint identity stays unchanged.
Column projection does not alter record offsets or row order.

Expose the option through the existing Vim recipe JSON editor, with multiline
editing and an inline schema example; preview shows projected target columns.
Native acceptance must cover PostgreSQL, SQL Server and SQLite, dry-run without
writes, typed application, quotation/NULL/empty fields, dropped-value privacy,
resume idempotence and changed original-source refusal. Keep broader expression
mapping, other formats and dedicated visual mapping controls tracked separately.
Run formatting, strict workspace Clippy and all workspace tests, then commit.
## Acceptance evidence

The native `transfer_columns` integration suite passed on PostgreSQL 17.10,
SQL Server 2022 and managed SQLite on Linux. It checks dry-run column order and
zero writes; quoted multiline Unicode, NULL and empty values; transactional
application; quarantine columns and dropped-value privacy; identical-source
replay without duplicate rows; HTTP 409 after changing only a dropped field;
and unchanged destination rows after that refusal. Fixtures use unique tables
and clean them up. HTTP creation rejects malformed and version-1 projections.

The server unit suite covers explicit source copies, unknown/ambiguous selected
headers, unselected duplicate headers, inconsistent record widths, option scope,
unknown fields, duplicate/invalid destinations and bounded output expansion.
Legacy rename behavior remains covered by the existing transfer regression.

Validation commands:

```sh
cargo fmt --all
cargo clippy --locked --offline --workspace --all-targets -- -D warnings
cargo clippy --locked --offline --workspace --features sift-server/live-pg,sift-server/live-mssql --test transfer_columns -- -D warnings
cargo test --locked --offline --workspace
cargo test --locked --offline --workspace --features sift-server/live-pg,sift-server/live-mssql --test transfer_columns -- --test-threads=1
```

Native Windows/macOS acceptance remains deferred. Broader mapping stays partial
in the canonical inventory for broader formats and larger sources. Portable
text/decimal expressions, XLSX/Parquet projection and direct Vim mapping
controls have shipped in later milestones (ADRs 133/139/160/161/166).
