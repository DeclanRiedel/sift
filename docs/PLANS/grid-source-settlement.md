# Grid apply captured source and owned settlement

Design: complete the HTTP ApplyEdits boundary with the same captured-source and
bounded-owner guarantees as migration apply (ADR-175), while retaining row
identity, optimistic conflict and human-reviewed AI proposal checks.

Create the typed ApplyEdits audit before preparation. Capture the path's actual
native entry before asynchronous authentication/metadata work; reject a body
connection mismatch first. Derive ledger actor/tenant/profile/configured database
from captured provenance/configuration, never by rereading an edited profile or
falling back to an unscoped actor on authorization failure. Metadata-free personal
mode stays supported without inventing managed attribution. Current authorization
and object/SQL policy checks remain authoritative. No new wire fields are needed.

Carry the captured entry through schema-based plan construction, owned transaction
begin and every parameterized statement. Replaced/closed/reconnected sources fail
before mutation rather than redirecting to a new native handle. Keep the existing
independent AI dispatch guard and exact reviewed-plan comparison. Revalidate caller
transaction identity normally and never commit or roll it back on behalf of the
caller. Its successful edits remain uncommitted observations in the ledger.

One admitted task owns execution, per-kind ledger observations and one terminal
audit after HTTP caller loss. Use the existing settlement semaphore and shared
logical shutdown guard propagated through native EOF. Freeze actor/correlation,
source and edit intent; no raw SQL, binds, rows or native diagnostic echoes enter
operation audits or change-ledger messages. Preparation refusals before a proven
plan/write attempt create no change rows. Overflowing aggregates remain unknown.
Typed optimistic conflicts are Conflicted; uncertain driver/timeouts/connection
failures remain OutcomeUnknown unless owned rollback is positively confirmed.
Unconfirmed owned rollback fences the original generation; a failed terminal
COMMIT never becomes a claimed rollback. Record the actual transaction identity,
including server-owned transactions. Keep owned rollback and
explicit transaction semantics visible, without inventing atomic metadata/database
commit or client receipt.

UPDATE/DELETE require a known native affected count of exactly one. Missing
counts cannot be rewritten to zero. INSERT may use returned rows when they prove
its count; missing counts with no returned rows remain unavailable and fail closed.
An owned transaction rolls back on that refusal; a caller-owned transaction stays
under caller control. Do not claim commit from native Done alone.

Acceptance extends existing row-edit fixtures and managed/native integration:
captured target despite profile edits, body mismatch/refused preparation without
native execution/ledger, replaced source fencing, caller loss in gated native
execution, retained audit/ledger/shutdown through settlement, per-kind outcomes,
caller-owned transaction behavior, exact conflict classification, missing native
counts, native diagnostic/privacy and bounded admission. Run formatting, strict
workspace/all-target Clippy and full workspace tests under the persistent 10%
Rust/Cargo cap. Other bulk/CSV/export boundaries remain separate work; native
Windows/macOS acceptance stays deferred.


Verification: pinned Rust 1.96.1; `cargo fmt --all`, strict workspace/all-target
Clippy and full locked/offline workspace tests with `sift-server/live-mssql`
passed. Full workspace: 1,818 passed, zero failed, six ignored across 97 result
groups. Focused acceptance also passed reconnect refusal, AI dispatch revocation,
bounded grid admission and all nine HTTP edit tests. The existing native SQLite
row-edit integration remains green; PostgreSQL and SQL Server fixtures were
available during the full run.

All Rust/Cargo work used the shared capped slice: CPUQuota=160% on this 16-thread
host, MemoryMax=3,287,567,155 bytes, MemorySwapMax=0, one Cargo job, one test thread
and fixed single-CPU affinity. Existing bounded test codegen settings were kept.
Native Windows/macOS, external-provider login and ignored benchmark acceptance
are not implied by these results.
