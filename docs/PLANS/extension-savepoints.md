# Driver RPC v2 savepoints

Status: host wire/lifecycle milestone implemented and verified (ADR-151).

Add typed savepoint/create, rollback-to and release operations to Driver method
family v2, preserving the v1 core/schema/transaction contract and public Driver
trait signatures. Extend the internal DatabaseProvider adapter with default
unsupported methods. Existing public Savepoint/RollbackToSavepoint/
ReleaseSavepoint operations, SDK and Vim controls remain the user surface.

Negotiate the highest common Driver version supported by host, manifest and
Hello, retain the selection on the generation, and send it in Welcome. A
provider must advertise its driver family. Invalid/duplicate ranges or missing
mandatory families fail handshake before readiness. Reject startup without
leaking its child process. Action-only packages need not implement Driver.
The host still accepts v1-only packages. Savepoint capability requires an explicit
v2-only minimum plus driver.core@1 and driver.transactions@1, preventing downgrade
from advertising unsupported dispatch. Unknown future capabilities remain refused.
Both archive and development validation enforce this contract.

Requests carry the remote transaction handle and bounded non-control name.
Acknowledgments echo the exact transaction and name; never treat a generic JSON
value as proof. Check capability, negotiated generation and handle provenance
before dispatch. Provider errors, transport/timeout failures and malformed/wrong
acks make the generation unsafe for this mutating state and stop it, with an
explicit invalidated/uncertain result. This isolates the tenant/package process,
not an OS sandbox; other connections in that generation are affected. Reconnect
is required. No native rollback/completion guarantee follows from a failed call.

Serialize host savepoint metadata transitions per transaction, including its
commit/rollback boundary. Recheck current policy, active transaction, original
connection and savepoint state after admission. Use bounded admission and owned
supervised work so caller abandonment or an observation timeout cannot release
ownership while the remote operation is pending. Only an exact successful ack
updates the host stack. Rollback-to invalidates younger savepoints; release
releases the selected point and younger points. Failure cannot fabricate
recovery. A query failure reported after recovery admission remains failed even
when the older rollback acknowledgment succeeds.
Provider implementations must serialize native per-connection/transaction work
and define native savepoint semantics; host conformance is not database quality
certification. Existing Driver and built-in engine implementations are unchanged.

Retain quota/tunnel leases through the pending control and independently own
connection close cleanup after removing its source from the session. Retain an
owned HTTP audit/shutdown supervisor for admitted transaction-control
requests. Late native outcomes remain uncertain to an expired observer; host
metadata must still settle under its owner. Credential bytes never enter request
arguments, environment, metadata, diagnostics or audit. Names are public labels,
not SQL; the host never synthesizes provider-native savepoint statements.

Extend the existing executable corpus, without new smoke scripts/workflows:
v1 compatibility, v2 negotiation and downgrade/missing-family refusal; names and
ack validation; nested create/rollback/release; wrong connection/transaction,
failed transaction recovery, current-policy refusal, delayed/failed callbacks,
caller abandonment and generation restart. Prove audit and resource ownership.
The explicit server acceptance case reuses the existing separately built
sift-conformance-provider binary; see
[Extensions](../EXTENSIONS.md#fault-and-graduation-matrix) for its invocation.
Run fmt, strict Clippy and the full workspace suite. Native provider/database
semantics require provider-specific evidence; Windows/macOS acceptance remains
deferred and no provider quality label is awarded by this milestone.


## Verification

Linux with pinned Rust 1.96.1: formatting and strict workspace/all-target Clippy
pass; full workspace tests pass (1,740 passed, six ignored). The explicit server
executable acceptance case passes separately, including an actual RPC deadline,
already-expired ready-lock admission, queued durable-policy revocation, recovery
races, abandoned HTTP audit/shutdown ownership and retained close quota/cleanup.
The standard suite leaves that case ignored because it requires separately
building the existing host fixture binary.

Validation respected the requested 10% host CPU/RAM caps, with one Cargo job and
one test thread. Temporary package-only test profile overrides used 256 codegen
units and no debug data for server/UI/desktop builds; assertions and test scope
remained enabled. Native third-party database semantics and deferred native
Windows/macOS acceptance are not certified by this result.

## Desktop follow-up

The existing Vim controls dispatch the same public operations, but desktop
savepoint display still mirrors acknowledgments locally: rollback-to does not
remove younger names, release removes only the named point, and transaction-state
refresh ignores the server's savepoint vector. Follow this host-wire milestone
with authoritative desktop refresh after savepoint actions and streamed queries.
Render only active server savepoints, clear names when the transaction ends,
and preserve failed state if concurrent query failure survives recovery. Extend
the existing GPUI transaction test with nested points and server state refresh;
keep native SQL Server release refusal and capability checks unchanged.

Bind desktop refreshes to their session and full transaction identity. Ignore
late replies after a target changes, and prevent an older query refresh from
overwriting a newer transaction-control outcome. Keep an admitted control pending
until its own authoritative refresh settles; concurrent query refresh must not
prematurely unlock it. This is desktop state synchronization, not a new public
wire operation or a provider quality claim.

## Transaction execution follow-up

Existing ExecuteDriverRequest uses implicit connection transaction state. Query
admission currently checks the selected transaction ID, but does not hold the
transaction-control lock through dispatch/drain or reject an ending transaction.
Design and verify query/end fencing before claiming a fully serialized external
transaction execution lifecycle: admitted queries must not escape into autocommit
when a terminal control overtakes them. Include uncertain commit/rollback outcomes,
stream/cancel/close ownership and v1 compatibility in that separate proof. The
current milestone serializes transaction controls and savepoint metadata only;
provider-native connection serialization remains required.

Proposed desktop ordering: query workers emit refresh requests rather than apply
asynchronous transaction snapshots. Route those requests through the same serial
executor that owns savepoint controls. Bind queued actions and replies to session,
connection, transaction ID and opened-at identity; recheck before execution and UI
application. Read authoritative snapshots in that executor, in command order.
Savepoint acknowledgment alone must not unlock the control; its own subsequent
snapshot settles pending state. Query-triggered snapshots preserve a concurrent
control's pending state. This keeps old query snapshots from racing newer control
outcomes without adding public protocol fields.
