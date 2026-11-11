# Bulk insert captured source and owned settlement

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#ai-execution).

Design: extend the captured-source/terminal-owner guarantees of ADR-175/176 to
HTTP BulkInsert, including preview-bound SQLite native batches. CSV imports have
additional resumable/quarantine/dedicated-connection paths and follow separately.

Create the typed audit before preparation; reserve the shared bounded settlement
permit and capture the authorized native entry before asynchronous authentication
or metadata work. Derive actor/tenant/profile/configured target from that entry,
not a reread saved profile. No managed authorization failure may become unscoped
attribution. Metadata-free personal mode stays supported. Preview and refused
preparation never create change rows.

One task owns execution and terminal ledger/audit after HTTP observer loss. Keep
original actor/correlation and one shared logical shutdown lifetime through native
completion/EOF, including nested SQLite workers, supervised driver futures and
transaction control. Track actual mutation dispatch and actual owned transaction
identity. Do not create ledger writes for unsupported formats, bad previews,
missing target metadata, rejected admission or other pre-dispatch refusals.

SQL Server native bulk uses bounded exclusive connection admission, current
operation/object authorization, captured generation validation, query resources
and refusal of any existing caller transaction. Hold the gate through actual
native completion, including late completion after a timeout. Timeout and native
dispatch share one atomic admission closure: a late worker must either have a
recorded attempt or be refused before native mutation. A timeout or uncertain
native failure fences the original generation and is OutcomeUnknown; neither
commit nor rollback is invented.

SQLite bulk keeps exact preview/value/schema checks and one-use confirmation.
Use the captured native source for schema, DDL, transaction begin and every batch.
Unknown affected counts fail closed rather than becoming zero. Confirmed rollback
is RolledBack; failed rollback fences the captured source and is OutcomeUnknown.
Never discard a known successful commit because of later delivery failure. Native
terminal-command uncertainty stays unknown. Database effects and durable metadata
are not atomic, and client receipt is not asserted.

Acceptance: managed captured target after profile edits, native dispatch blocked
by an existing caller transaction, replaced generation refusal, bounded admission,
caller-loss ownership during gated native work, original actor/correlation,
shutdown tracking through late native settlement, no values/diagnostics in audits,
preview/preparation refusal without change rows, SQLite known/missing counts and
rollback uncertainty. Keep fmt, strict workspace Clippy and workspace tests green
under the persistent 10% Rust/Cargo cap. No new workflow, smoke script, dependency,
wire field or locked Driver signature.

Verification: pinned Rust 1.96.1; formatting, strict locked/offline workspace and
all-target Clippy, and the full locked/offline workspace test suite with
`sift-server/live-mssql` passed. Full suite: 1,824 passed, zero failed, six ignored
across 97 result groups. Focused checks covered reconnect refusal, bounded
admission, atomic timeout/dispatch closure, batch count/terminal uncertainty,
all 12 HTTP edit/bulk cases, native SQLite bulk and the public bulk API. Native
PostgreSQL and SQL Server fixtures were available for the full suite.

All Rust/Cargo work used the shared capped slice: CPUQuota=160% on this 16-thread
host, MemoryMax=3,287,567,155 bytes, MemorySwapMax=0, one Cargo job, one test thread
and single-CPU affinity. A 512-unit server test build hit the memory ceiling;
server test codegen was increased to 1,024 units without increasing the cap.
Desktop used 512 units and UI/workspace UI used 256, all with test debuginfo off;
existing UI optimization remained enabled. The final full run exited successfully
with a recorded service memory peak of 3,264,708,608 bytes. These validation-only
profile settings do not change repository defaults. Native Windows/macOS and
ignored external-provider/benchmark acceptance are not implied.
