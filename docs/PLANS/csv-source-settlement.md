# CSV captured source and owned settlement

Design: finish HTTP ImportCsv source attribution and terminal ownership while
preserving ordinary COPY, skip/quarantine, SQLite/SQL Server atomic and reviewed SQL
Server generated-column workflows. Durable checkpoint imports also retain their
initial source; their enclosing file-transfer receipts remain separate.

Capture the authorized native entry before asynchronous HTTP authentication or
metadata work. A request-scoped SessionStore clone retains that source binding
across nested workers and all ordinary connection lookups; it shares underlying
session state but never installs the binding in the global store. Current policy
checks still run. A replaced/closed source is refused, never silently reopened or
redirected. Independent owned SQL Server connections remain supported; their
configuration/credential snapshot must be taken under a generation check against
the captured source. Metadata-free personal mode remains supported.

A bounded HTTP owner retains original actor/correlation, one shared shutdown
lifetime through native completion/EOF, terminal typed audit and change-ledger
settlement after caller loss. Reuse the shared settlement semaphore. Preparation,
dry-run and review refusals produce no change rows. Managed failures never fall
back to unscoped attribution. Native PostgreSQL COPY uses the same exclusive
admission, caller-transaction refusal, timeout/dispatch closure and late-native
ownership as ADR-177.

Observe data and DDL separately. An autocommitted CREATE TABLE stays Committed
when later data import fails. SQLite records both phases as rolled back only when
owned rollback is confirmed. Missing affected counts never become zero or skipped
rows. Skip/quarantine preserves fatal infrastructure failures. Confirmed earlier
per-row/chunk commits remain partial observations if later work fails; uncertain
native effects remain OutcomeUnknown. Reviewed SQL Server row counts retain their
existing accepted-input meaning, rather than claiming trigger/result equivalence.

Track actual owned transaction identity and never commit or roll back a caller's
transaction. Unknown terminal commands and failed rollback fence the mutated native
generation, and cleanup must not close a replacement by numeric ID. Dedicated
SQL Server uncertainty must not fence the editor connection or its caller-owned
transaction. Reviewed imports
must retain terminal observations even after dedicated-connection cleanup. Query
text, values, rejects and native diagnostic echoes stay out of audits/ledger.
Database effects, metadata and client delivery are not atomic.

Acceptance: captured target after profile edits, generation refusal, preparation
and dry-run without changes, native caller-loss and timeout lifetime, admission
before caller transactions, actual DDL/data outcomes, missing counts, SQLite
rollback and reviewed SQL Server refusal/commit semantics. Keep formatting,
strict all-target workspace Clippy and full workspace tests green under the
persistent 10% Rust/Cargo cap. No new wire/Driver signature, dependency, smoke
script or workflow. Native Windows/macOS acceptance remains deferred.

Ordinary SQL Server abort imports use a server-owned transaction with parameterized
batches and a returned native row-count witness, including under NOCOUNT. This
exposes positive rollback confirmation instead of inferring it from the legacy
native bulk method's opaque error. Table creation shares that owned transaction.

PostgreSQL keeps native COPY inside a server-owned transaction. A positively
confirmed rollback makes COPY failure or missing counts RolledBack; unknown
COMMIT/rollback remains unknown. Earlier autocommitted DDL keeps its separate
outcome and has no transaction ID from the later COPY transaction.

Verification: pinned Rust 1.96.1; formatting, strict locked/offline workspace and
all-target Clippy, and the full locked/offline workspace test suite with
`sift-server/live-mssql` passed. Full suite: 1,830 passed, zero failed, six ignored
across 97 result groups. Additional native PostgreSQL projection/resume acceptance
passed with `sift-server/live-pg`. Focused source, admission, reviewed-import,
atomic-count, HTTP mutation and native SQLite checks also passed.

Native SQL Server acceptance covers NOCOUNT row-count witnesses, rollback when
ROWCOUNT limits a batch, transactional table creation, ordinary projection/resume,
reviewed generated columns and explicit cancellation without poisoning the editor
transaction. Native SQLite proves rollback across table creation and multiple
batches; PostgreSQL retains native COPY under owned transaction settlement.

All Rust/Cargo work used the shared capped slice: CPUQuota=160% on this 16-thread
host, MemoryMax=3,287,567,155 bytes, MemorySwapMax=0, one Cargo job, one test thread
and single-CPU affinity. Test codegen used 1,024 server units, 512 desktop units and
256 UI/workspace UI units, with test debuginfo disabled and existing UI optimization
retained. The successful full run recorded a 3,271,626,752-byte service peak.
These validation-only profile settings leave repository defaults unchanged.
Native Windows/macOS and ignored external-provider/benchmark acceptance remain
outside this evidence.
