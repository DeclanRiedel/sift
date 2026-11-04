# Private benchmark retention

Add an owner-private, tenant-scoped editable policy for saved benchmark runs.
Default is disabled: retain existing snapshots until explicitly deleted. Optional
maximum age is 1–36500 days since the server save timestamp; optional count is
1–500 newest saves, ordered by save timestamp then stable ID. A run is removed
if either criterion excludes it. Annotation edits never reset age. Notes and
source context share the snapshot lifetime. Hard snapshot/owner byte quotas stay.

Design before implementation: persist only numeric policy values, revision and
maintenance timestamps in SQLite. Policy rows reference current membership;
revocation removes the policy, so rejoining does not silently resume deletion.
Snapshot payloads remain in SecretStore. A V056 migration adds policy storage and
DELETE/handle-replacement triggers that enqueue obsolete benchmark secret handles
in the existing durable cleanup queue, including cascades. The cleanup worker
accepts the benchmark namespace, refuses to delete currently referenced handles,
retries backend failures, and never stores secret bytes/errors in SQLite.

Read and preview require current owner membership. Preview takes the desired
policy and expected revision and returns the exact current removal count/bytes,
a server cutoff timestamp and a bounded five-minute state digest. Applying needs
explicit confirmation and the same desired policy, revision, cutoff and digest.
Recompute against current IDs, timestamps, handles and annotation revisions under
one transaction; changed saves/annotations/policy return conflict, not a changed
cleanup. The digest is a change fence, not authentication or an authority token.
Only the authenticated owner can configure their policy. Activation atomically
updates the policy and removes the reviewed entries, enqueueing payload cleanup.

Enforce enabled policies during save in the same insert transaction, checking
hard quotas after pruning so a full retained library can accept a newer run.
Do not report a newly saved record as retained if pruning would remove it.
A bounded server worker visits enabled policies fairly every minute, rechecks
membership and revision under transaction, expires entries and writes a typed
PruneBenchmarkRuns audit in that transaction. Policy maintenance never opens a
database driver. The shared encrypted-secret cleanup worker drains queued blobs;
logical removal may precede physical deletion, and backups retain their copies.

GET, preview and policy update each have typed audited Operations and thin SDK
methods. No SQL, names, notes, tags, context or secret handles enter policy
responses/audits. Full metadata backups preserve policies; tenant recovery must
classify the new table and preserve same-ID ownership. Original save timestamps
remain authoritative after recovery, so restored enabled policies can expire
restored runs. Document this rather than claiming archival retention.

The disconnected Vim saved-run library adds a policy editor: empty age/count
means no limit, preview displays removal count/bytes, and explicit activation
uses the reviewed state. Scope drafts/replies to instance, tenant and request;
edits invalidate review, conflicts retain drafts, and server/tenant changes
refuse dispatch. Preserve active annotation drafts by refusing to open the
retention editor until that draft is closed. After an accepted change, refresh
the library and clear stale opened detail. Show ongoing automatic deletion and
the snapshot/notes/context lifetime before activation; no new external approval
flow or database connection is needed.

Acceptance covers disabled defaults, deterministic age/count boundaries,
transactional save quotas and newest retention, stale/concurrent previews,
owner/tenant/revocation boundaries, automatic maintenance fairness/auditing,
secret cleanup failure/retry/current-handle guards, file-backed reopen, migration
fixtures and same-ID recovery. Verify typed SDK and actual Vim preview/edit/apply
and stale replies. Run formatting, strict workspace Clippy and full workspace
tests, graduate ADR-135, update canonical checklists and commit. Native
Windows/macOS acceptance remains deferred.

## Bounded maintenance selection

A row limit alone does not bound a scan/sort over all policies or future cleanup
entries. Use a partial ordered index containing only enabled policies and walk
its native NULL-first timestamp order. For encrypted cleanup, query each supported
namespace through a `(namespace, julianday(not_before), handle)` index with the
same per-query limit, merge at most three bounded candidate batches by due time,
then process at most the original total limit. Runtime SQL explicitly names these
indexes. Verify the actual runtime statements with bundled-SQLite query plans so
accidental temporary sorting cannot invalidate the maintenance work bounds.
SQLite selection, reference checks and cleanup acknowledgments run in owned
blocking jobs. A current-reference check and stale-queue acknowledgment share a
write transaction so a concurrent final reference removal can enqueue cleanup
afterward without losing it. Secret backend I/O never holds the SQLite lock.

## Acceptance — 2026-11-04

Implemented as ADR-135. File-backed metadata acceptance covers disabled defaults,
age/count boundaries and stable ordering, preview expiry and concurrent revisions,
annotation races, ownership/revocation, quota rollback, fair maintenance, encrypted
cleanup failure/retry and current-handle guards, persistence and same-ID recovery.
Bundled-SQLite query plans verify indexed bounded maintenance selection; a
current-thread executor test verifies responsiveness during SQLite contention.
The thin SDK integration exercises preview, confirmation, stale-review conflict,
removal and audit privacy. The actual Vim interaction test exercises field focus,
preview/edit/activation, conflict draft recovery and stale server/request replies.

Validation on Linux: `cargo fmt --check`,
`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`, and
`cargo test --workspace --locked --offline` passed (1,696 tests passed, five ignored).
The initial full run exposed stale V055 lifecycle expectations; fixtures now
expect V056 and both lifecycle checks pass in the full rerun. No Driver signature,
CI workflow or environment-variable changes. Native Windows/macOS acceptance
remains deferred as agreed. Attached profiling plans and advanced benchmark tools
remain open in the canonical inventory.
