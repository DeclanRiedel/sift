# Recipe execution capture and terminal receipts

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#ai-execution).

Design: complete HTTP transfer-recipe execution ownership across CSV, XLSX,
Parquet, extension conversion, checkpoint imports and query-to-artifact exports.
Direct streaming export attribution remains a separate boundary.

Create an exact recipe-execute Operation before preparation, using the known
recipe ID without inventing a workspace ID. Reuse the existing ExecuteTransferRecipe
operation kind and authorization. Capture the native connection and reserve the
shared settlement permit before asynchronous authentication/recipe work. A bounded
owner retains original actor/correlation and one shared shutdown lifetime through
conversion, native EOF, artifact production and terminal audit after caller loss.

Bind a request-local store to the captured source, preserving ADR-178 guarantees
through format conversion and dedicated SQL Server connections. A managed native
source must match the recipe workspace tenant even when the actor owns both.
Current workspace execute authority and saved-recipe revision are rechecked at
native admission. Cleanup may bypass that recipe guard solely for an owned rollback;
connection generation and native policy checks still apply. Revocation cannot
redirect cleanup or force a claim that unconfirmed work rolled back.

Record attempted import data and DDL for every format from native observations.
Preview, conversion and preparation refusals create no change rows. Preserve
confirmed commits when later quarantine/artifact persistence fails; the recipe
operation fails while its known database outcomes remain committed. Record partial
checkpoint/per-row progress, uncertain effects, exact DDL targets and actual
per-phase transaction IDs. Attach the authorized workspace without fabricating a
versioned query source. Factor the existing CSV receipt writer for reuse.

Parquet ordinary imports must use the captured source, owned transaction tracking,
observed affected counts (a SQL Server row-count witness under NOCOUNT), confirmed
rollback and uncertainty fencing. Do not infer inserted rows merely from input
length. Reviewed generated-column modes retain their accepted-input semantics.
Checkpoint DDL identifies its checkpoint table, not the imported data table.

Exports succeed only after server artifact production/persistence, with generic
failure diagnostics. This does not assert client receipt or add database-mutation
receipts for arbitrary write SQL routed through export; that export boundary
remains separate work. Audit and ledger never contain SQL, binds, upload bytes,
formatter options, rejects or native diagnostic echoes.

Acceptance: capture before metadata/conversion, profile edits, generation and
cross-tenant refusals, source-bound native admission, caller loss and native EOF,
current recipe revision/authority, CSV/XLSX/Parquet/checkpoint receipts, committed
imports followed by failed auxiliary persistence, Parquet missing-count/rollback
uncertainty and export production failure. Run formatting, strict workspace
all-target Clippy and full workspace tests under the persistent 10% Rust/Cargo cap.
No Driver signature, dependency, workflow or smoke-script changes. Native
Windows/macOS acceptance remains deferred.


Verification (2026-11-07): formatting and strict locked/offline workspace
all-target Clippy passed with `sift-server/live-mssql`. The full workspace suite
passed 1,838 tests, zero failed, six ignored across 97 result groups. Additional
native PostgreSQL projection/resume acceptance passed with `sift-server/live-pg`.
Native SQL Server and SQLite projection/resume acceptance checks workspace-bound
receipts, actual checkpoint DDL targets, transaction identities and observed counts;
SQL Server Parquet runs under NOCOUNT. Focused HTTP checks cover caller loss,
profile edits, recipe revision/access loss, owned rollback, cross-tenant refusal,
committed imports followed by auxiliary persistence failure, Parquet missing counts
and terminal uncertainty, export failure and native EOF retention.

All Rust/Cargo verification used `app-sift-rust.slice`: CPUQuota=160% on this
16-thread host, MemoryMax=3,287,567,155 bytes, MemorySwapMax=0, single-CPU affinity,
one Cargo job and one test thread. Test-only overrides used 1,024 server codegen
units, 512 desktop units, 256 UI/workspace UI units and zero package test debuginfo;
existing UI optimization was retained. Repository profile defaults are unchanged.
The final validation service exited successfully with a 3,264,737,280-byte memory
peak. Native Windows/macOS and ignored external-provider/benchmark checks remain
outside this evidence.
