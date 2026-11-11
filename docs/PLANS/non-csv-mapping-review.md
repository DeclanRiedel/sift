# Direct XLSX/Parquet column mapping review

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#imports-and-transfers).

Status: implemented and accepted on Linux (2026-11-06).

Deliver a direct Vim mapping grid for existing XLSX/Parquet upload recipes. Begin
with a revision-bound audited server source preview and SDK contract, then wire
structured output rows into the desktop. Reuse native conversion/projection and
portable expression implementations; do not reimplement Arrow/XLSX semantics in UI
or introduce UI dependencies into shared crates. Saving a mapping uses existing
revision-checked recipe updates. It never imports rows automatically; existing
explicit dry-run/write review remains authoritative.

Source preview takes expected recipe revision, upload bytes, explicit XLSX sheet,
format version and candidate options. It requires current authenticated workspace/
recipe read authority and a distinct PreviewTransferSource operation. Only bundled
XLSX/Parquet upload imports are supported. There is no database connection, query,
artifact creation, upload retention or external formatter invocation. Audit records
IDs/action/outcome only, never upload, cells, expressions or file/sheet names. An
owned supervisor retains terminal audit after observer loss. Limit concurrent decode
workers without queued admitted uploads; retain each permit until blocking decode
actually ends, including timeout/observer loss.

Return original source column names, nullable/type support, source row count and
SHA-256 digest, plus mapped output schema and at most 20 sample rows from the exact
candidate projection/expressions. Source schema must remain available when a
candidate cannot resolve or selects unsupported Parquet roots, so the grid can
drop them and retry. Unsupported source cells are never fabricated as NULL.
Projection failure has a distinct unavailable state with a bounded reason; malformed
archives/schema/input remain hard errors. Reuse import's 64 MiB source, expansion,
row/materialization and expression limits. Bound schema/preview response to 1 MiB,
source columns to existing import limits, and selected outputs to 256. A preview
validates the complete mapped source with existing converters before sampling;
it is not an assertion that a target table will accept or commit those rows.

The desktop picks a file on a background worker, binds original instance/tenant/
workspace/recipe/revision/sheet and holds bytes only in memory. Render source and
selected output rows, types and projected samples. Vim selection/drop/reorder/copy/
rename controls build the existing ordered projection/expression DSL. Preserve
unrelated recipe options, existing expressions and null markers; never silently
replace unsupported mappings. Explicit preview freezes candidate options and upload
digest. Edits invalidate review. Explicit save repeats source/revision/draft checks,
then existing optimistic recipe update authority applies. Scope switches, changed
recipes and late replies refuse without replacing another draft. Destination/native
write approval is still a separate transfer execution step.

Extend existing transfer acceptance with XLSX/typed Parquet headers/samples,
projection/copy/expressions/null rules, unsupported-root recovery, malformed/bounded
input, stale revision, authority and typed audit privacy. Desktop acceptance must
cover real Vim editing and source/draft/reply guards. Run formatting, strict
workspace Clippy and workspace tests within the 10% Rust/Cargo CPU/RAM cap. No new
smoke scripts/workflows; Driver signatures remain unchanged. Native Windows/macOS
acceptance stays deferred. Broader source formats remain separate work.

Source preview reserves one of two shared worker leases before buffering JSON,
with a 30-second request deadline. Wire intake allows the worst-case JSON byte-array
encoding of the 64 MiB source plus 1 MiB candidate options and bounded selectors;
worksheet names are limited to 4096 bytes. Detached decoders retain their lease
until actual completion.

## Implementation and acceptance

ADR-166 records the implementation. The pure protocol and SDK expose the source
preview; the server reuses import converters and owns admitted preview/update
settlement and audit. The desktop grid edits ordered outputs and portable
expressions, previews the exact upload/candidate, and explicitly saves through
optimistic recipe updates. Instance, tenant, workspace, recipe/revision, request,
immutable upload and original editor guards fence asynchronous work.

Formatting and strict default/native-feature workspace Clippy passed. Existing
transfer acceptance passed on PostgreSQL, SQL Server and SQLite (3/3), covering
source/sample identity, expressions/nulls, access denial, stale revisions, audit
privacy, exactly-once recipe-update outcomes and JSON uploads above 2 MiB.

Both source-preview unit tests passed: row/byte/non-finite/sample bounds and
worker admission/shutdown ownership after observer timeout. The GPUI Vim mapping
acceptance passed, covering real editing, explicit preview/save, late replies,
changed candidate/editor/instance guards and preservation of unrelated drafts.
The full workspace suite passed: 1,778 tests, zero failures, six existing ignores.

All Rust/Cargo validation used the shared 10% CPU/RAM limit, one build job, one CPU
affinity and one test thread. The final validation service exited successfully;
its memory peak was 3,281,182,720 bytes, below the 3,287,567,155-byte cap. Native
Windows/macOS acceptance remains deferred. Broader formats and larger sources
remain separate work.
