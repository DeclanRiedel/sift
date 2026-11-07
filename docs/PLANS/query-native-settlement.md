# Native receipts for HTTP, room and WebSocket SQL

Design: extend ADR-180's captured native observations to all public direct SQL
execution, including HTTP bound rooms and versioned WebSocket statements. Result
observation audit/history and native mutation receipts have separate lifetimes.

Generalize the request-scoped native observer and receipt writer used by exports.
Propagate observations explicitly through HTTP's nested drain task as well as the
independent native worker; room routing must observe the server-owned connection.
Keep source/actor/tenant/profile/configuration, room and validated versioned query
provenance from existing preparation. Never substitute the caller's editor source
for a room's native source. Preserve original correlation through asynchronous
settlement, even after HTTP/socket loss, cancellation, timeout or result retention
failure. Share bounded settlement capacity and one shutdown lifetime through native
EOF and durable receipt append. Close admission atomically after startup ends so
late queued work cannot turn an unattempted refusal into an invented effect.

Classify possible effects using the existing dialect-aware classifier, including
comments, CTE writes, SELECT INTO, mixed batches and opaque/control SQL. A classified read has
no mutation receipt; an unattempted preparation/admission refusal has none either.
A completed ordinary statement on a known autocommit source can be Committed only
after raw EOF. A known ordinary statement in a still-known managed transaction is
Partial, carrying its actual transaction ID, never a claimed commit. Raw native
transaction uncertainty, mixed/unsupported grammar, missing completion and native
errors remain OutcomeUnknown. Preserve native commits despite later result/socket
failure. Observe every native page even after result observation stopped at Done.

Mutation counts use only checked native affected counts. Returned rows, empty
result sets and history row counts are never substitutes. Keep absent counts
absent, explicit native zero distinct, and overflow absent. Audits/history continue
to describe observed result production/delivery; they do not attest client receipt,
rollback or native commit. Receipts can settle after the public result returns.
Native failure is not proof that an attempted statement changed nothing.

Diagnostic code redaction is already shared by the two query paths; prove it for
extensible Other errors in both query audit/history and native receipts. Persist
neither SQL/binds/rows nor native diagnostic echoes in audit/ledger. Do not invent
transaction IDs, query revisions or room provenance. Existing transactional query
receipts are observations, not retrospective updates when the caller later commits.

Acceptance extends current HTTP, WebSocket, room and native fixtures: missing counts
with returned rows; explicit zero/overflow; raw control before/during execution;
managed transaction writes; caller/ack loss, timeout and raw EOF; late native errors;
preparation refusal; room source and versioned provenance; CTE/mixed effects; native
SQL Server NOCOUNT and PostgreSQL/SQLite affected counts; Other-code privacy.
Run formatting, strict all-target workspace Clippy and full workspace tests under
the persistent 10% Rust/Cargo cap, using the established lower-memory 2,048-unit
server test configuration. Native Windows/macOS acceptance remains deferred.
No Driver signature, protocol, dependency version or CI changes.


Verification (2026-11-07): formatting and strict locked/offline workspace all-target
Clippy passed with `sift-server/live-mssql`. The full workspace suite passed 1,848
tests, zero failed and six ignored across 97 result groups. Additional native
PostgreSQL projection/resume/export/query receipt acceptance passed with
`sift-server/live-pg`. Focused acceptance passed all 29 HTTP integration tests,
99 API tests (including source/room/versioned and expanded WebSocket cases),
12 shutdown tests and native SQLite/SQL Server transfer/query checks.

HTTP receipt cases cover comments, data-changing CTEs, absent native counts despite
returned rows, zero/overflow, late native error, extensible Other errors, managed
transactions and raw settings inside those transactions. WebSocket cases prove
returned-row history stays separate from absent mutation counts, late native errors
remain visible after observed Done, Other diagnostics stay private, and actual
native completion can outlive failed socket/ack/cancel/retention observation. HTTP
shutdown cases prove the same distinction for caller loss, timeout and response
retention failure. Existing source/room/versioned acceptance continues to bind
receipts to the executed configuration, actual room source and validated workspace
context, refusing preparation changes before native dispatch.

Native PostgreSQL/SQLite/SQL Server checks use fresh sources and separately verify
row-producing writes, plain counted writes and explicit zero; SQL Server NOCOUNT
retains absent counts and uncertain transaction confidence after raw SET. No
mutation receipt substitutes the result/history row count.

Every Rust/Cargo command used the persistent shared 10% slice: CPUQuota=160% on
this 16-thread host, MemoryMax=3,287,567,155 bytes, MemorySwapMax=0, one Cargo job,
one test thread, single-CPU affinity and no incremental compilation. Test-only
settings retained 2,048 server codegen units, 512 desktop units, 256 UI/workspace UI
units, zero package test debuginfo and existing UI optimization. The largest unit
target was checked before integration tests; task-cgroup cache reclaim between
final phases reduced pressure without host-wide cache changes. Repository profile
defaults are unchanged. The successful final validation service recorded a
3,283,345,408-byte memory peak. Native Windows/macOS and ignored external-provider/
benchmark acceptance remain outside this evidence.
