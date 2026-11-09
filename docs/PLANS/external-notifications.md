# External-provider notifications

Design before implementation. Driver RPC v4 inherits v1 core, v2 savepoints and
v3 estimated plans, and adds `driver.notifications@1`. Packages require minimum
v4 for this family. Public HTTP/WebSocket protocol 5 and the locked Driver/PgExt/
MssqlExt signatures stay unchanged. Channels and payloads are opaque data; this
family does not require a host SQL dialect or construct SQL. Schema-restricted
profiles are refused because no schema-to-notification mapping is proven; live
policy changes also terminate a subscription. Existing built-in
Postgres behavior remains available through its adapter. Native third-party
semantics and quality certification require separate evidence.

## Wire and bounded retention

`listen` is a streaming request with connection, a host-generated subscription
WireId, an exact ordered list of 1–16 distinct channels, a maximum event wire-byte
size and initial stream byte credit. Channel labels contain 1–63 UTF-8 bytes and
no control characters. Request must fit the negotiated frame before dispatch.
Acknowledgment echoes connection/subscription/channels and reusable disposition.
Providers use a dedicated native listener with the authorized connection's
credential/database scope, keep the original connection reusable and do not
execute caller SQL or replay a stronger native principal. These are trusted
provider claims, not host attestations.

Typed stream events are Notification(connection, subscription, channel, payload)
and Stopped(connection, subscription, reusable disposition). Stream and
subscription IDs match. Host verifies identity, subscribed channel, payload shape,
wire bytes and supervisor sequence/credit. Event ceiling is 64 KiB including the
framing prefix, bounded further by negotiated framing; initial credit is at most
128 KiB and the existing per-stream default. Supervisor supports explicit initial
credit while preserving old request_stream behavior. Two bounded forwarding
queues (eight events each), bounded in-flight credit and retained-byte quota
reservation prevent unbounded buffering. Credit replenishment has a deadline.
Notification bodies and channel labels never enter durable audit/diagnostics.

## Ownership and stop barrier

The startup owner captures source and authority before spawning, takes bounded
connection read admission and rechecks current authorization/source after waiting.
A dedicated lifetime listener lease is registered while admission is held; normal
editor queries and transaction controls remain available after startup. Session/
connection close, reconnect and invalidating close cancel and drain these leases
before closing the base native handle. Listener cancellation is independent of the
ordinary query gate so cleanup cannot deadlock behind close. Failed reconnects
can start new listeners after old ownership has drained.

Only the startup observer has the configured request timeout. Native RPC startup
and eventual cleanup retain quota, driver-task and shutdown ownership even after
caller loss or observation timeout. After startup, source/authority is checked
periodically, including while output is backpressured. Socket EOF/Close is observed
while the subscription is quiet; lease revocation and server drain remain visible.
The existing SDK stream and Vim start/stop/restart/history paths consume events.
UI labels become provider-neutral; existing command identity remains compatible.
Native Postgres listener registration prunes dead weak entries on restart.

Receiver loss, source/authority change, explicit stop, close and shutdown cause
`unlisten(connection, subscription)`. Completion requires both a matching reusable
unlisten acknowledgment and a matching terminal Stopped stream barrier. Host keeps
the stream registered and drains/credits queued notifications until both arrive,
with a five-second cleanup deadline and bounded process-stop grace. No late frame
can be silently reinterpreted as another subscription. Protocol/RPC errors,
malformed/mismatched acknowledgments, invalid events or uncertain cleanup fence
the tenant/package generation with fixed diagnostics; independent processes live.
An unexpected remote Stopped event is surfaced as a failed subscription and still
requires idempotent unlisten acknowledgment. A healthy observer stop does not kill
the generation. Audited Listen retains original actor/correlation and one terminal
outcome, with native cleanup ownership surviving observer loss. The SDK's existing
subscribe return acknowledges socket/request creation, not native readiness.

## Acceptance

Use the existing executable conformance fixture, shared provider fixture support,
real WebSocket/SDK and Vim listener behavior. Cover v4 minimum/version discovery,
v1–v3 refusal and inherited v3 estimated/v2 savepoint behavior; generic unknown
semantic dialect; exact channels, Unicode payloads, multiple events and bounded
credit; invalid input before dispatch, wrong identities/channels/sequence, oversized
or malformed events; quiet socket disconnect and healthy restart; queued source/
authority changes, active query/transaction coexistence, connection/session close
and reconnect ordering; startup observer loss/timeout and delayed/uncertain stop
with quota/shutdown ownership; independent provider survival and terminal audit
privacy. Native Postgres listener regression must keep quiet disconnect behavior.
Run formatting, strict workspace/all-target Clippy and all packages under the
persistent 10% host cap, using the established grouped Cargo tests when the combined
feature-unified test compiler exceeds memory. No new workflows/smoke scripts or
native Windows/macOS acceptance claims.


## Linux acceptance

Final formatting and strict workspace/all-target Clippy passed with
`sift-server/live-mssql`. All packages passed using the established sequential
resource-capped Cargo groups:

- Server (`cargo test -p sift-server --features live-mssql --locked --offline`):
  662 passed, zero failed, 13 ignored across 52 groups.
- Other packages (`cargo test --workspace --exclude sift-server --features sift-server/live-mssql --locked --offline`):
  1,196 passed, zero failed, five ignored across 47 groups.

Combined default coverage: 1,858 passed, zero failed and 18 ignored. All 13
server process-dependent ignored cases also passed explicitly: three notification
ownership cases, five notification WebSocket/SDK cases, the v2 savepoint lifecycle
case, two v3 estimated-plan SDK cases and two estimated-plan ownership cases. The
shared estimated parser case passed again with those ownership checks.

Notification cases exercise generic unknown dialects, original UTF-8 labels,
multiple independent subscriptions, editor query/transaction coexistence,
v4 inheritance of estimated plans/savepoints, legacy/input refusal without native
dispatch, wrong identities/channels/sequence, malformed/oversized events, uncertain
start/stop state, missing barriers, both stop-receipt orderings and graceful remote
termination with idempotent acknowledgment. Healthy processes survive observer
stop, malformed generations do not affect an independent process, and fixed host
errors do not retain bodies/channel names in terminal audit.

Private lifecycle cases prove queued expiry/source replacement/authority refusal,
live revocation under output backpressure, abandoned/timed-out startup ownership
and reconnect cleanup. SDK cases prove quiet socket disconnect, server drain,
retained query/cursor/4 MiB quota, one terminal durable audit with original actor
and correlation, schema-scope refusal/capability discovery and live durable policy
revocation. Backpressured connection/session close waits for subscription receipts
before base close. Existing supervisor fault coverage now joins repeated explicit
shutdown and verifies terminal Stopped state without a late Degraded overwrite.

An additional disposable native PostgreSQL WebSocket/SDK case passed with
`--features live-pg`: two quiet stop/restart cycles return dedicated listener count
to zero and leave the original connection usable. Native registration prunes dead
weak channel entries on restart. Existing source-bound Vim listener/history tests
and UTF-8/control/byte-bound validation passed in the workspace UI group, including
verbatim whitespace/Unicode dispatch and stale response rejection.

Rust 1.96.1 used one Cargo job and test thread, single-core affinity, incremental
compilation disabled, debug-free package test overrides and bounded codegen units
(server 4096, UI/workspace UI 256, desktop 512). Shared/service limits stayed at
CPUQuota 160%, MemoryMax 3,287,567,155 bytes and zero swap; final successful run
peak was 3,284,430,848 bytes. No compiler settings or CI changes were committed.
Grouped passes preserve the user-requested cap after the previous combined
workspace compiler exceeded it; the single feature-unified workspace runtime
configuration remains unverified under that limit. Native third-party provider
behavior/certification and Windows/macOS acceptance remain separate/deferred.
The parent extension/MCP inventory and broad product goal remain partial.
