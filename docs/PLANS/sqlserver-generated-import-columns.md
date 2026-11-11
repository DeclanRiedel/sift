# SQL Server reviewed generated-column imports

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#sql-server).

Status: implemented (ADR-142), with shared late-open ownership from ADR-141.
CSV, XLSX and typed Parquet native acceptance passes on the Linux host; native
Windows/macOS acceptance remains deferred. Final workspace validation is recorded
with the implementing commit.

Implemented contract. Complete the SQL Server import contract across
CSV, CSV-backed XLSX and typed Parquet without changing the locked Driver trait.
Keep the existing default import paths compatible. The new workflow reviews
native target metadata, handles identity/computed inputs explicitly, and runs
writes on a supervised disposable connection so editor session settings cannot
be contaminated by IDENTITY_INSERT or cancellation.

## Native modes and review

Typed identity choices: reject explicit input, omit input and generate, or preserve
explicit values. Typed computed-column choices: reject input or omit it and let
the server compute it. Do not silently omit generated-always, hidden, rowversion,
column-set or other unsupported native families. Require at least one writable
output, unique native target bindings, an existing ordinary table and supported
native scalar types. Preserve typed Parquet values and CSV null rules. Imported
and omitted names, actual database/schema/table, identity effect and exact native
catalog observations must be visible in the dry-run result.

Bind the precondition to the reviewed input digest, output names/types and modes,
native target/database/actor identities, selected column metadata and required
permissions. Recheck before apply and before commit. Explicit confirmation must
accompany apply; transient confirmations never become durable recipe options.
Changes in source, projection, mode, identity or schema invalidate review. Native
concurrent DDL can still reject apply; previews are not catalog locks. Disclose
that identity counters and trigger/external effects need not roll back even when
rows do. Do not claim data equivalence, trigger isolation or recoverable IDs.

Use one transaction, bounded parameterized batches and supervised native calls.
IDENTITY_INSERT is enabled only for preservation, only on the disposable
connection, and cleaned up by closing it even after failure. Never replay
arbitrary startup batches; retain the established literal-session setup allowlist
and explicit unsupported state. Never interpolate data into SQL, log row values
or place credentials in metadata. Native failures must remain terminal.

## Ownership, progress and cancellation

The coordinator must survive observation timeouts without duplicating work, stop
future batches and commit after caller/source cancellation or current authority
loss, and clean up the disposable connection. Opening ownership must be safe even
when a driver finishes opening after its caller has stopped waiting: late handles
must be closed, not lost. Setup and cleanup stay separately bounded; a wedged
driver cannot freeze an HTTP handler or consume unbounded task/connection slots.

Active progress uses a caller-selected run identity, original session/connection,
current policy and at most one active import per source. Stage, validated/attempted
rows and committed rows stay separate; attempted rows are never reported as
committed before native COMMIT. Read/cancel are typed audited operations. Cancel
binds the original run/source and acknowledges a request, not proven rollback.
Cancellation or timeout during native COMMIT must report an uncertain outcome;
never fabricate successful cancellation or safe retry. Source close cancels its
owned import. Bound registrations and poll/backpressure in the desktop.

## Product surfaces and acceptance

API and SDK carry typed modes, transient review confirmation and active progress.
Recipe options retain modes only; each execution has a fresh run identity and
confirmation. Vim CSV review exposes modes, imported/omitted fields, invalidation,
dry-run then explicit apply, progress and cancellation on the original source.
Generic XLSX/Parquet recipe execution shares the backend contract and preserves
source/request fences. Unsupported combinations, including resumability where
an atomic generated-column contract is not proven, are refused explicitly.

Native SQL Server acceptance must cover preservation, generated IDs, computed
recalculation, native permissions and target replacement/schema changes, exact
review binding, rollback after later-batch failure, identity counter disclosure,
editor-session preservation, cancellation/cleanup and unknown commit outcomes.
Use real HTTP/SDK and actual Vim interaction tests. Keep test scaffolding minimal,
run fmt, strict workspace Clippy and workspace tests, update inventories/ADRs and
commit verified coherent milestones. Native Windows/macOS acceptance remains
deferred by the user.

Foundation evidence (2026-11-05): shared opening ownership passes deterministic
SessionStore timeout, caller-drop and session-close tests, pending-close admission
retention and normal adoption. Formatting and strict workspace Clippy pass; all
1,717 workspace tests pass (5 ignored). Existing real SQLite, PostgreSQL and SQL
Server transfer acceptance passes through the updated opening path. Generated
column modes and active import progress/cancellation remain to be implemented.

## Delivered scope and evidence

The API/SDK and Vim CSV review expose typed modes, transient input/metadata-bound
confirmation, native type and imported/omitted details, active progress and
cancellation. Saved upload recipe options retain modes only; the backend also
binds the saved recipe and actor to review. Modes remain explicitly limited to
existing ordinary tables, supported scalar conversions and atomic Abort without
resume/type overrides. Legacy import and durable checkpoint paths remain available.

The existing native transfer fixture now proves generation, preservation,
computed recalculation, editor ROWCOUNT/IDENTITY_INSERT preservation, stale mode,
input and schema refusal, rollback after a later-batch constraint failure, and
CSV/XLSX/typed-Parquet recipe review/apply, including binding changes in
unselected upload fields. An SDK request blocked by an actual
SQL Server table lock proves active progress, exact-run cancellation and no
committed cancelled rows. PostgreSQL and SQLite transfer regressions pass too.
A programmable driver proves lost COMMIT acknowledgement is OutcomeUnknown;
the metadata regression preserves historical hash/sequence/append-only behavior.
Actual Vim tests cover review/mode invalidation, stale-progress acknowledgement,
explicit apply and retaining the original outcome after Escape cancellation.

Verification: `cargo fmt --check`, strict workspace/all-target Clippy and
`cargo test --workspace` pass (1,720 tests, five existing ignores). Native
transfer acceptance with `live-pg,live-mssql` passes all three engine fixtures,
including the actual HTTP/SDK and Vim regressions described above. CI remains
manual; Windows/macOS host acceptance remains deferred.
