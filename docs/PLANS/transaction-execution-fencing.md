# Transaction execution fencing

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#ai-execution).

Status: implemented and verified; decision graduated to ADR-153.

## Required behavior

A query naming a managed transaction must never execute after that transaction
ends and silently enter autocommit. A nontransactional query admitted before
begin must finish its native stream before begin can change connection state.
Commit/rollback/savepoints must wait for admitted native query work; queued work
must recheck current policy, connection generation, transaction identity and
ending state before dispatch. Abandoning an observer must not release native
ownership, connection/tunnel quota or a bounded driver-work permit prematurely.

The Driver trait and its engine extension signatures remain unchanged. Public
operations and Driver RPC v1 query compatibility remain unchanged. This is host
execution ownership, not provider-native certification or an OS sandbox claim.

## Connection barrier and identity

Each connection owns a shared asynchronous read/write barrier. Nontransactional
queries hold read leases, preserving ordinary query concurrency. Explicit
transaction queries, begin and all transaction controls hold write leases.
Leases span native dispatch and the driver's complete page-channel lifetime,
through EOF rather than only the first cursor or terminal page. Result delivery
from already buffered pages does not extend native execution ownership.

Recheck authority and expected transaction after admission. Admission is bounded
by five seconds and a shorter configured request deadline, with an explicit
post-lock deadline check for an already-ready lock. Work whose source connection
was removed or replaced is refused. Ending transactions cannot accept queries.
Begin publishes metadata while holding its write lease; late begin handles whose
source disappeared roll back under their owner instead of becoming orphaned.
Controls settle metadata under the same owner, including late completion after
an observation timeout. Close retains cleanup ownership and waits for admitted
leases after requesting cursor cancellation. Close observers remain bounded. Cleanup never dispatches a second terminal
command using a handle already consumed by an end owner; native close settles
remaining connection state after that owner releases the barrier.
Reconnect must not replace a connection underneath admitted work or preserve an
obsolete transaction. Uncertain/fenced connections require explicit cleanup.

Native and external driver transaction IDs have independent allocators. Allocate
opaque host transaction IDs across both runtime adapters so one connection's
begin cannot replace another's transaction entry. Keep native/remote IDs inside
their original handles; engine savepoint calls use native IDs. No wire shape or
locked Driver signature changes follow from opaque host IDs.

## Owned query execution

A server-owned worker acquires the lease and owns driver.execute, independent
of HTTP/WS observers. It delivers a rebound page receiver through a oneshot and
forwards pages through a bounded proxy. If the observer disappears before a
cursor exists, keep native execution alive; when it returns, cancel/dispose its
cursor and drain its original channel. Once a stream exists, consumer drop,
registry eviction or cancellation requests bounded driver cancellation while the
owner continues draining to native EOF. Dropping the application-facing receiver
must not drop the original native receiver and free the barrier early.

Record native error pages, execution failure and early cancellation before
releasing ownership. Missing terminal pages, owner loss/panic or uncertain
transaction completion fence the connection. Native presentation failures or
pre-dispatch refusal do not invent a native transaction failure. Explicitly
bounded HTTP draining continues to enforce its deadline when its caller leaves;
WS streams retain their existing long-lived streaming policy. A stalled driver
can retain its connection and leases, but bounded global/tenant admission and
bounded observers prevent it from freezing the server or spawning unlimited
replacement work. Never claim native rollback from a lost acknowledgment.

RPC begin/commit/rollback failures after dispatch invalidate their process
generation, matching savepoint uncertainty fencing. RPC deadlines cover writer
admission and frame writes as well as responses; partial/timed-out writes stop
the generation. Cancellation and shutdown writes are bounded so a provider
that stops reading stdin cannot prevent process termination. A dispatched native terminal
failure leaves the transaction fenced; no speculative commit/rollback retry or
query reuse follows. Default unsupported/capability/provenance checks still fail
before native dispatch.

## Verification and scope

Extend existing behavioral fixtures: delayed execute before cursor, streaming
rows held across a concurrent end, already-queued queries after end, begin versus
nontransactional execution, failed query/recovery ordering, source close, caller
abandonment and admission expiry. Exercise native SQLite row preservation and
rollback plus the executable RPC lifecycle corpus, including uncertain terminal
acknowledgments. Verify that independent connections and ordinary nontransaction
queries remain usable and that quota/ownership survive cancellation.

Run format, strict workspace/all-target Clippy and the full workspace suite under
the user's 10% CPU/RAM budget. Keep test code minimal and reuse executable/native
fixtures; no new smoke scripts or workflows. Native Windows/macOS acceptance stays
deferred. Desktop begin/end command source binding and broader native extension
workflows must retain their own source/transaction proofs; this milestone covers
SessionStore SQL execution, transaction controls and their cleanup lifecycle.

## Completed evidence

Formatting and strict workspace/all-target Clippy passed. The full workspace
suite passed 1,748 tests with six existing ignored cases. The existing executable
savepoint lifecycle case also passed explicitly, including malformed begin and
uncertain terminal responses. Native SQLite managed transaction/catalog/import
and snapshot migration acceptance passed in the workspace run. Rust/Cargo used
one build job and one test thread under a shared 10% host CPU/RAM cgroup cap;
low-memory codegen settings kept the cap unchanged. Windows/macOS acceptance
remains deferred.
