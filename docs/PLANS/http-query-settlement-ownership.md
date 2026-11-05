# HTTP query settlement ownership

Status: implemented and verified; decision graduated to ADR-155.

## Required behavior

ADR-153 owns native execute and raw page channels independently of observers,
but the HTTP query handler still owns operation audit, history, change ledger,
room result retention and shutdown admission. Dropping the handler can therefore
leave accepted work without terminal bookkeeping. Auxiliary retention errors can
also bypass the operation audit after native execution has already settled.

After metadata admission, move the complete HTTP query workflow into a bounded
server-owned task. Retain actor, sanitized request correlation and shutdown
admission when its HTTP observer disconnects. Preserve the existing query/drain
deadline rather than extending it when the caller leaves. Record the observed
execution outcome once before auxiliary room retention or response construction
can fail; independent history/ledger settlement and inactive-room cleanup retain
their existing authority and privacy rules. A failed response delivery must not
rewrite a successful native observation as a native failure. Timeout, cancellation,
connection loss/invalidation and internal driver errors leave write outcomes
partial; a lost observation does not prove rollback or absence of writes.

Bound admitted HTTP settlement owners independently of native driver tasks, so
slow metadata or disconnected callers cannot create unlimited detached tasks.
Use the existing server-wide admission bound; refusal happens before execution.

Share one shutdown guard across the HTTP workflow and its original native stream
owner. The counter remains one logical query. Capture its reference explicitly
across the application-drainer spawn; keep it through native execute and raw EOF,
including after an HTTP timeout or an already-observed terminal page. Owner
bookkeeping completion must not claim native EOF. Existing bounded shutdown drain
deadlines remain authoritative for wedged native work.

Task-local scope may carry the shared guard on the current server task, but spawned
native/application workers receive an explicit captured reference. Capture and
restore correlation explicitly around the owned HTTP workflow. Driver signatures,
public wire shapes and Operation variants remain unchanged. Protocol stays pure
serde and UI dependencies stay out of shared crates.

## Evidence and scope

Extend existing HTTP/shutdown fixtures with controlled native execute/page-channel
lifetimes. Abort a caller before cursor publication, then prove the original
operation settles once with retained correlation and original actor. Exercise
success and native failure, shutdown admission through terminal page versus raw
EOF, timeout ownership, history/ledger privacy and auxiliary retention failure.
Keep ordinary HTTP response and room-routing acceptance green.

This milestone covers the synchronous HTTP ExecuteQuery workflow and its native
lease. WebSocket/export terminal audit/history ownership remains separate work;
this is not a claim that every transport is complete. Native provider certification
and Windows/macOS native acceptance remain separate, with Windows/macOS deferred.
Run format, strict workspace/all-target Clippy and the complete workspace suite
under the shared 10% host CPU/RAM cap with existing low-memory settings.

## Completed evidence

Formatting and strict workspace/all-target Clippy passed. The complete workspace
suite passed 1,751 tests with six existing ignored cases. The controlled HTTP
fixture passed all four cases: abandoned success, abandoned native failure,
abandoned timeout with late completion, and response quota refusal after native
success. It verified durable actor/correlation, one audit/history/ledger record,
SQL privacy and shared shutdown tracking until native EOF. Saturated settlement
admission refused before dispatch and recovered capacity. Validation used the
shared 10% host CPU/RAM cap, one Cargo job/test thread and existing low-memory
codegen settings. Windows/macOS native acceptance remains deferred.
