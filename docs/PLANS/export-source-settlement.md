# Export source capture and independent native receipts

Design: complete direct HTTP export startup ownership and source attribution,
and record SQL mutations routed through direct and recipe exports. Keep body
observation, artifact production and native database effects distinct.

Reserve bounded settlement capacity, capture the authorized connection generation
and bind a request-local store before asynchronous preparation. A supervised owner
retains original actor/correlation through startup after HTTP caller loss. Pass the
resulting stream to the existing paced body observation audit; dropping an unpolled
body is still a failed export observation. Pacing uses the captured source tenant.
Native policy and generation checks remain active; active caller transactions and
reconnect on captured stores remain refused.

A scoped native observation is captured by the existing independent execution
worker. Admission/dispatch closure is atomic: preparation refusals and late timeout
admissions cannot invent attempted changes. Observe only dispatch, checked native
affected counts, terminal codes and actual native EOF, never rows, SQL or binds.
Observe all native pages even after the encoder stops at its first result or the
HTTP body disappears. A bounded independent settlement task retains capacity and
shared shutdown tracking until native completion and ledger append. Native errors,
missing completion, cancellation uncertainty and unsupported statement shapes must
not become rollback or committed claims. Missing counts stay absent, without
falling back to encoded row counts. Preserve confirmed effects if later encoding,
artifact persistence, pacing or body delivery fails.

Use the existing dialect parser and structural read-only rules for receipt
classification, including comments, CTE writes and SELECT INTO. Ordinary single
DML/DDL with native completion and EOF can be Committed only while the captured
connection retains known autocommit state. Raw opaque/control SQL makes that confidence unknown until reconnect, including
settings executed inside managed transactions that may survive COMMIT. Observe this at native
admission and EOF, so earlier or concurrent raw transaction control cannot create
a false commit receipt. Mixed batches, explicit
transaction control, procedure/dynamic/unclassifiable SQL get conservative
BulkMutation/OutcomeUnknown receipts rather than disappearing from the ledger.
Structural SELECT classification is not attestation that arbitrary functions are
side-effect free. Do not fabricate a transaction ID or query workspace revision.
Recipe exports attach their authorized workspace; direct exports have none.

Acceptance extends existing HTTP/native test fixtures: startup caller loss,
source/profile/generation/authority changes, exact actor/correlation, missing counts,
late native error after first Done, missing EOF completion, encoding/body loss after
confirmed writes, mixed and unclassifiable SQL, recipe export workspace receipts,
read-only exports without mutation rows, active caller transactions and admission
refusals. Native SQLite and PostgreSQL prove actual write receipts. Run formatting,
strict all-target workspace Clippy and full workspace tests under the existing 10%
Rust/Cargo resource cap. No Driver/protocol signatures, dependency versions or CI changes. Native
Windows/macOS acceptance remains deferred.

Extensible `Code::Other` diagnostic text is reduced to the persisted label `other`;
HTTP body failures expose a generic streaming error. Acceptance includes both
ledger/audit code redaction and streaming-error redaction.

Preserve TDS DONE_COUNT presence in the existing vendored Tiberius ExecuteResult.
Add an optional checked observed total without changing legacy total()/iterator
behavior. Missing command counts and overflow remain None. The Sift SQL Server
plain-DML path uses that observation, so NOCOUNT cannot become zero. Native
acceptance distinguishes an explicitly counted zero-row update from a successful
NOCOUNT update with an absent count and verifies the actual database changes.


Verification (2026-11-07): workspace formatting, Rust 2021 formatting of the two
vendored TDS files, and strict locked/offline workspace all-target Clippy passed
with `sift-server/live-mssql`. The final full workspace suite passed 1,847 tests,
zero failed and six ignored across 97 result groups. Additional native PostgreSQL
projection/resume/export acceptance passed with `sift-server/live-pg`. Native
SQLite and SQL Server transfer/export acceptance passed on the final source too.

Acceptance covers startup/body caller loss, captured profile configuration,
stale generation and closed admission, current authority after admission wait,
active caller transactions, prior and concurrent raw transaction control, persistent
settings inside managed transactions, fresh-source recovery of commit confidence,
late native errors, missing completion/counts, overflow, auxiliary artifact failure,
private extensible error codes and streaming errors. Native PostgreSQL and SQL
Server preserve absent counts for row-producing DML; plain DML proves positive
and explicit zero counts. SQL Server NOCOUNT stays absent while a separate native
connection confirms the actual committed update.

All Rust/Cargo work used the shared capped slice: CPUQuota=160% on this 16-thread
host, MemoryMax=3,287,567,155 bytes, MemorySwapMax=0, one Cargo job, one test thread,
single-CPU affinity and no incremental compilation. Validation-only test settings
used 2,048 server codegen units, 512 desktop units, 256 UI/workspace UI units and
zero package test debuginfo, retaining existing UI optimization. The 1,024-unit
server unit-test build reached the memory limit; compiling that target first with
2,048 units completed within the same cap. Task-cgroup cache reclaim between final
phases reduced pressure without changing host-wide caches or raising limits.
Repository profile defaults remain unchanged. The final successful full-workspace
service recorded a 3,261,956,096-byte memory peak. Native Windows/macOS and ignored
external-provider/benchmark acceptance remain outside this evidence.
