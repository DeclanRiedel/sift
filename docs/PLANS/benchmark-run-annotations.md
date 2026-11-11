# Private benchmark-run annotations

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#performance-workbench).

Add revision-checked notes and tags to private saved measurements. The stored
report, name, run ID and save timestamp remain immutable. The update API only
accepts an expected annotation revision plus notes/tags, never measurements.
Legacy saved payloads default to empty annotations at revision zero. Notes
are bounded to 8 KiB; at most 16 nonempty 64-byte tags are trimmed, sorted and
deduplicated. Reject controls in tags and unsupported controls in notes.

Keep all annotation text in the existing encrypted benchmark SecretStore
payload. Add only an annotation revision integer to the SQLite index (V055).
Copy the current JSON snapshot, preserve unknown measurement fields, replace
only annotations, write a fresh opaque
secret handle, then atomically swap it under tenant/owner/membership, old-handle
and revision checks. Preserve the 4 MiB snapshot and 64 MiB owner limits. Clean
uncommitted replacement handles on failure; post-commit old-handle cleanup
failure must not misreport an accepted edit as failed. Keep existing backup
inventory and tenant recovery working through the same indexed namespace.

Concurrent annotation saves return conflict instead of overwriting drafts.
Deletion must compare its selected handle so it cannot erase a concurrently
updated index or orphan the new payload. Missing secret payloads remain
browsable/deletable and cannot be annotated. No annotation bytes enter audit,
operation labels, logs, presentation storage or SQLite. The new update Operation
records only tenant/run/revision identifiers and uses existing authenticated,
owner-private audited persistence.

The Vim saved-run browser opens a full note/tag draft for the opened run,
checks its revision, and saves only on explicit request. Scope drafts and
responses to instance/tenant/request identity. Preserve drafts on conflicts;
opening another record or changing server/tenant cannot retarget an edit.
Display bounded scrollable note/tag previews and include tags in private list
summaries. Keep AI/MCP measurement projections explicitly annotation-free.
Do not alter portable measurement artifact exports or comparison inputs.
Workspace/Git context capture, retention policy and broader comparisons stay
tracked separately in the full performance plan.

Acceptance must prove immutable measurements, legacy defaults, stale/concurrent
save/delete handling, tenant/owner/revocation guards, encrypted payload and
SQLite/audit privacy, quotas/validation, full draft preservation and typed Vim
dispatch without database connections. Run all workspace gates before commit.

## File backend prerequisite

Concurrent saves exercise FileSecretStore's whole-map persistence. Serialize
mutations on a backend-instance writer gate owned by the blocking worker,
including after its async waiter is canceled. Clone the last committed map,
apply the mutation privately, persist/fsync/rename it, then publish it in memory.
Readers must not wait for filesystem work, and failed writes must leave the
committed map unchanged. Apply this same lane to put, delete and coordinated
AI tenant-key replacement. Offline multi-process rules remain unchanged.
Prove concurrent put/delete durability across reopen, canceled-waiter gate
retention, read availability during a blocked writer and failed-write rollback.
## Linux acceptance — 2026-11-04

Implemented through the pure protocol, encrypted metadata payload, audited HTTP
operation, thin SDK and disconnected Vim browser. ADR-126 records annotation
privacy and revision semantics; ADR-127 records the required file-writer fix.

The file-backed library test proves legacy defaults, preservation of known and
unknown measurement fields, validation and snapshot quotas, private pagination,
stale/concurrent saves, membership/owner boundaries, plaintext exclusion and
reopen. A gated delete race proves a stale deletion cannot erase a replacement.
Tenant-restore acceptance preserves the annotation revision with its remapped
secret handle. Existing SDK/API acceptance proves typed updates, HTTP conflict,
immutable reports, rejected report injection, tag summaries and audit redaction.
AI and standalone MCP acceptance prove private annotation text stays outside
measurement projections.

The Vim test uses actual keyboard dispatch for full drafts, Escape, save and
Enter submission, conflict refresh, instance binding and late replies. The
shared Vim interceptor routes saved-browser Escape through field/draft handling
before dismissal. Notes support wrapped multiline editing and Shift-Enter.

File-backend tests prove concurrent put/delete durability after reopen,
worker-owned serialization after waiter cancellation, reads during a blocked
writer, failed persistence leaving put/delete invisible, and successful retry.
Lifecycle fixtures advance to metadata V055 without changing their wire shapes.

Passed locally:

- `cargo fmt --all -- --check`
- `cargo clippy --locked --offline --workspace --all-targets -- -D warnings`
- `cargo test --locked --offline --workspace`

Native Windows/macOS acceptance remains deferred. Workspace/Git context capture,
retention policy, annotation history/sharing and the remaining performance
workbench inventory are not completed by this milestone.

## Source-context follow-up

ADR-134 adds optional client-reported workspace/Git context beside saved
measurements. Workload review freezes the observation; private saving requires
opt-in. Annotation updates preserve it, while AI/MCP projections and portable
measurement artifacts omit it. Reviewed retention is implemented (ADR-135). See
[the source-context design](benchmark-source-context.md) and the canonical
performance checklist for current scope.
