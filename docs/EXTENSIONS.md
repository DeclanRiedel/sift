# Extensions

Current development status and remaining work: [TODO](TODO.md); detailed [plan index](PLANS/README.md).

Sift extension packages are immutable ZIP archives with a strict
`sift-extension.toml`, canonical `sift-extension.lock`, and optional Ed25519
signature. Server code runs as a directly executable child process over
length-prefixed JSON stdio. Sift does not invoke a shell, inherit the server
environment, or install an extension's language runtime.

The Rust wire contract and generated JSON Schema source are in
`crates/extension-protocol`. ADR-022 and ADR-031 define the normative design.

## Compatibility

| Contract | Current version | Compatibility rule |
| --- | ---: | --- |
| Public HTTP/WebSocket protocol | 5 | A package range must contain `5`. |
| Extension RPC | 1 | The manifest and `hello` range must contain `1`. |
| Driver method family | 1–4 | Select the highest common host/manifest/Hello version; retain it in Welcome and the process generation. |
| Manifest | 1 | Unknown fields and any other schema version are rejected. |
| Provider configuration schema | 1 | Draft 2020-12; the descriptor publishes the schema version. |

There is no historical pre-release `Engine` codec. Protocol v1 uses
`ProviderId`, `DialectId`, `ProviderRef`, and explicit capabilities throughout.
Unknown capabilities are ignored by older clients; missing required
capabilities fail explicitly.

Package and development-override validation reject provider capabilities that
their declared minimum Driver RPC version cannot dispatch. Every provider must
declare `driver.core@1`. Version 1 supports `driver.cancel@1`,
`driver.schema.shallow@1`, `driver.schema.deep@1`, `driver.schema.graph@1`, and
`driver.transactions@1`. Version 2 adds `driver.savepoints@1`; that capability
requires a manifest minimum of 2 and `driver.transactions@1`. Version 3 inherits
these families and adds `driver.explain.estimated@1`, requiring minimum 3.
Version 4 inherits these methods and adds `driver.notifications@1`, requiring
minimum 4. Bulk ingest, analyzed plans/Profile and process control remain future
typed families. The broader `driver.explain@1` capability is not accepted from
external packages.

A database-provider package must advertise a compatible Driver family in Hello;
missing families, invalid ranges and duplicate family declarations fail startup.
Action-only packages need not advertise Driver. Existing v1 providers continue
using their v1 wire contract.

### External savepoints

A v2–v4 provider declaring `driver.savepoints@1` implements `savepoint`,
`rollback_to_savepoint`, and `release_savepoint`. Each request contains
`{"transaction":"<wire-id>","name":"<label>"}`. The success response must echo
that exact object. Names contain 1–128 UTF-8 bytes without control characters;
the host passes labels without constructing SQL. Providers must quote native
identifiers and serialize native per-connection/transaction work themselves.

The existing transaction HTTP API, SDK and Vim controls dispatch these actions.
Host controls and explicit transaction queries serialize per connection, including
begin, commit/rollback and close cleanup. Ordinary nontransactional queries share
read leases, while begin waits for their original native streams to reach EOF
(ADR-153). Admission waits at most five seconds, bounded further by a shorter
request timeout, and rechecks current policy and transaction ownership before
dispatch. Duplicate/unknown active names fail before RPC. Successful rollback-to
invalidates younger points; release releases the target and younger points.
Recovery cannot clear a query failure reported after rollback-to admission.

The desktop reads active names and transaction condition from authoritative SDK
snapshots. A savepoint ack keeps controls pending until its own refresh settles.
Query completions request refreshes through the same serial executor, preserving
control ownership. Source identities reject stale actions and replies. Parked
connection activation restores current transaction state; refresh failure shows
unknown state and prevents commit (ADR-152).
Begin/commit/rollback also capture instance/profile/session/connection and a unique
request ID; end captures transaction/opened-at identity. Executor and UI refuse
stale targets before dispatch or settlement. Connection activation metadata is
separate from control acknowledgment (ADR-154).

A provider error, timeout, disconnect, malformed ack or mismatched transaction/
name stops the entire tenant/package generation and returns
`connection_invalidated` with an uncertain-outcome message. Reconnect is
required; no native rollback guarantee follows. Other connections in the same
generation are affected. Unrelated tenant/provider processes remain independent.
Begin/commit/rollback RPC errors also stop the generation; existing v1 success
payloads remain compatible. Request deadlines include writer admission and frame
writes. Partial or timed-out writes invalidate framing and stop the generation;
cancel/shutdown writes cannot wait indefinitely on a blocked pipe.

An expired observer cannot cancel the owned control or discard its eventual
host-state settlement. Admitted HTTP requests retain their audit owner if the
caller disconnects; an observation timeout is audited as a failure, even if the
remote operation later succeeds. Connection/session close retains cleanup
ownership until admitted control settles.

### External estimated plans

A v3/v4 provider declaring `driver.explain.estimated@1` implements
`explain_estimated`. `DriverEstimatedPlanRequest` carries the connection wire ID,
original single query/DML SQL, typed parameters and `max_bytes`.
`DriverEstimatedPlanResponse` echoes the connection, declares a `reusable`
connection disposition, and supplies one tagged plan: `postgres_json` (`json`),
`sql_server_xml` (`xml`) or `sqlite_query_plan` (`rows` of `id`, `parent`, `aux`,
`detail`). The format must match the descriptor's host-supported semantic dialect.
The typed structs and their JSON Schema derives are in `sift-extension-protocol`.

The provider must obtain an estimated plan without executing/analyzing the
workload and restore temporary native settings or discard its connection. These
are provider claims: process conformance cannot attest database behavior or award
quality. The existing HTTP/SDK, Vim Explain view and governed estimated-plan reads
retain this warning alongside the tree/raw plan. External Profile/analyzed capture
and unknown semantic dialects are refused; no execute or built-in fallback occurs.

Host limits are 1 MiB SQL, 256 parameters, and the smaller of 1 MiB, the configured
result-byte limit and the negotiated response frame budget (with envelope reserve).
Nonfinite floats, intervals outside the wire microsecond range and native
parameters tagged with another provider are refused before dispatch. The encoded
request must fit the frame too. `max_bytes` bounds both the serialized tagged plan
payload and normalized raw text; the RPC envelope is additional. Plans have at most
10,000 tree nodes,
100,000 JSON/XML values/nodes and depth 64. Host parsing rejects runtime evidence,
nonfinite/negative normalized estimates, missing/malformed plans, cyclic SQLite
rows, dialect/format mismatch, wrong connection and uncertain disposition. Raw
plans may contain sensitive query data and remain subject to existing access and
audit policy. Every failure after RPC dispatch fences the whole tenant/package
generation with a fixed diagnostic; independent generations remain live.

Capture holds exclusive connection admission and refuses an active managed
transaction. Admission is bounded by five seconds or a shorter request timeout;
current policy and source generation are checked again after waiting. The native
RPC, connection gate, quota and shutdown ownership survive caller loss and
observation timeout. HTTP Explain retains captured source, actor/correlation and
one terminal audit. An observation timeout remains a failed audit even if the
provider later returns a valid plan.

### External notification subscriptions

A v4 provider declaring `driver.notifications@1` implements streaming `listen`
and unary `unlisten`. Channels/payloads are opaque data and need no host SQL
dialect. Schema-restricted profiles cannot subscribe because channel payloads
have no proven schema mapping; capability discovery and live revocation retain
this refusal. `DriverListenRequest` carries the original connection wire ID, a
host-generated subscription ID, exact ordered channels, `max_event_bytes` and
`initial_credit_bytes`. The stream ID equals the subscription ID.
`DriverListenAck` echoes connection/subscription/channels and declares reusable
base state. Channels are 1–16 distinct labels, each 1–63 UTF-8 bytes without
control characters. Requests must fit negotiated framing before dispatch.
Providers retain their own native naming constraints; built-in PostgreSQL still
requires its existing ASCII identifier form.

`DriverNotificationFrame` uses the `event` tag: `notification` carries
connection/subscription/channel/payload, while `stopped` carries
connection/subscription/disposition. Host validates identity, channel membership,
shape, supervisor sequence and byte credit. Each framed event (including its
four-byte prefix) is limited to 64 KiB or smaller negotiated framing. Initial
credit is at most 128 KiB and the normal stream default; the request explicitly
narrows Welcome's default for this family. Eight-event queues and a 4 MiB tenant
retained-byte reservation bound live retention; one concurrent-query and cursor
slot remain held through cleanup. Providers must keep heartbeats alive while quiet
or backpressured. Host notification errors use fixed diagnostics without bodies
or channel labels; trusted providers must also avoid logging notification data.

The provider opens a dedicated native listener within the authorized connection's
credential/database scope, keeps the editor connection reusable and does not
execute caller SQL or replay a stronger principal. These are operator-trusted
claims, not native-behavior attestations. Ordinary queries and transaction
controls stay available while listening. Short startup admission captures source
and rechecks current authority; live checks continue during backpressure.
Connection/session close and reconnect cancel and drain lifetime listener leases
before closing the base handle.

On receiver loss or stop, `DriverUnlistenRequest` names connection/subscription.
Completion requires both `DriverUnlistenAck` echoing those IDs with reusable
state and the matching terminal `stopped` stream event. Either receipt may arrive
first; the host retains stream registration and drains/credits queued data until
both arrive. Cleanup has a five-second deadline plus bounded process-stop grace.
Unlisten is idempotent for a subscription the provider already stopped; unexpected
remote termination fails the subscription but still needs that acknowledgment.
Any uncertain RPC/cleanup or invalid event fences the tenant/package generation;
a healthy observer stop preserves it. Independent process generations stay live.

Existing session WebSocket/SDK subscriptions and Vim start/stop/restart, bounded
history and source guards use this path. Display labels are provider-neutral;
command identity remains compatible. Subscribe returns after socket/request
creation, not native readiness. Quiet socket EOF, auth/source revocation and
server drain are observed while listening. Startup observation timeout/caller loss
cannot discard the owned RPC or its eventual cleanup. Query/cursor/byte quota,
driver-task and shutdown ownership survive until native settlement; one terminal
Listen audit retains original actor/correlation. No public protocol or locked
Driver trait signature changes. Native provider certification remains separate.

Provider quality is not supplied by an extension:

| Quality | Meaning |
| --- | --- |
| `compatible` | Package, handshake, health, and failure-boundary conformance. |
| `query_capable` | Core query/value/stream/cancel corpus passes. |
| `transactional` | Transaction and failed-transaction corpus passes. |
| `ide_capable` | Deep-schema and declared IDE corpus passes. |
| `sift_certified` | Sift-maintained fixtures, release matrix, performance budget, signed provenance, and security review. |

An external provider has no quality label until the corresponding host-owned
conformance evidence exists. Installing a package or completing one process
handshake does not by itself award `compatible`. `available` is live runtime
state: built-ins are immediately available, while a lazy external provider
becomes available after its first successful supervised handshake.

The bundled `sift/postgres` and `sift/sql-server` providers are
`sift_certified`. The external `acme/conformance` executable is a test fixture,
not a supported production database provider.

## Security and data handling

- Native third-party code is operator-trusted code isolated by a process
  boundary. `process_only` is not an OS sandbox claim.
- Packages and every referenced schema file are content-locked. Paths,
  symlinks, case collisions, hashes, signatures, target triples, sizes, schema
  depth, and compatibility ranges fail closed.
- Schema references may resolve only to locked files in the same package.
  Remote, absolute, parent, query, and encoded references are rejected.
- Configuration and action schemas reject secret-shaped fields and
  `x-sift-secret`. Credential schemas require secret-shaped fields to set
  `x-sift-secret = true`.
- Credentials are resolved by opaque handles from `SecretStore` and delivered
  only to the tenant-scoped process that needs them. Secret bytes never enter
  SQLite, process arguments, environment variables, audit arguments, or
  diagnostics.
- Tenant execution is deny-by-default. Installation and grants are
  instance-admin actions; an explicit tenant allowlist entry is also required.

## Runtime behavior

One executable may multiplex a package's contributions, but data-bearing
processes are scoped per tenant. Lazy generations start on first use. Eager
generations start for every currently allowed tenant. Admission is bounded per
instance, extension, and extension/tenant; one tenant's slow startup does not
serialize another tenant's startup.

The supervisor validates the exact extension identity, version, manifest
digest, contribution set, and RPC ranges before accepting work. It enforces
frame and concurrency limits, deadlines, cancellation grace, stream byte
credit, ordered frames, heartbeats, bounded/redacted diagnostics, restart
backoff, and quarantine. A crash or protocol violation kills that generation,
releases pending unary and stream consumers, and does not affect server
readiness or unrelated providers.

Hydration and eager-start failures are isolated per extension. A package with
no artifact for the current platform, an invalid runtime descriptor, or a
failed eager generation is unavailable without blocking server readiness or
removing healthy providers. Static hydration failures are persisted as a
quarantined selection and exposed through extension diagnostics.

`driver.core@1` providers may use any declared dialect id for generic
connect/ping/query/page/close behavior. SQL-semantic features remain available
only when both their capability family and a host-supported semantic dialect
are present. Restricted connection policies fail closed when the host cannot
classify the provider's dialect; unrestricted profiles may use provider-native
SQL. Contextual operation discovery uses the real provider id and the same
capability checks as dispatch.

Commands and governed tools pass through central authorization, schema
validation, classification, timeout/result limits, one-use approvals, and
operation audit. MCP exposes only descriptors that are both `mcp_exposable`
and currently authorized.

## Desktop contributions

Settings → Extensions opens a fixed host-owned dialog. Command contributions
render as buttons when they take no input. The active tenant, workspace room,
connection profile, runtime connection, and document supply contextual targets;
the dialog shows the selected target. Explicit object schemas render string,
integer, number, boolean, nested object and JSON-array controls, bounded to
16 controls, four nested object levels, and 64 KiB of raw and encoded arguments.
Object controls include or omit the group with true/false; required groups start
included. Blank optional fields stay omitted, while required empty strings remain
strings. Entered values inside omitted groups produce an enable-parent error.
Array controls accept JSON arrays; integer tokens must fit signed/unsigned 64-bit
JSON numbers. References without explicit supported types, type unions and other
unsupported property types remain unavailable. Schema defaults are not applied.
The server validates the complete input schema, including array items and other
constraints, on invocation.

Use `i` to edit, Tab/Shift-Tab to move through fields, and Escape or Enter to
return to action navigation. The field region scrolls to the selected control.
Password-format and writeOnly annotations propagate through object groups;
JSON-array fields containing sensitive annotations or schema references are
conservatively masked, including when the bounded sensitivity scan is inconclusive.
Action arguments never enter presentation storage or action labels. Approval review
and invocation bind the same exact nested arguments and current target context;
editing either requires a fresh review.

Object outputs render as read-only detail fields; arrays of objects render as
read-only tables. The desktop caps the dialog at 200 contributions and each
result at 16 fields, 100 rows, and 256 characters per cell. Other output
shapes use a bounded generic value display. The server's declared output schema
and result-byte limit remain authoritative.

A `client_panel` can bind to a read operation by setting `source_action` to the
id of exactly one command or governed tool in the same package:

```toml
[[contributions.client_panel]]
id = "usage"
source_action = "read-usage"
```

The source must be classified `read`, accept an empty object, and declare an
object or array-of-objects output. The desktop shows a Refresh button; it
invokes the source through the same audited operation path. Invalid or
ambiguous bindings remain unavailable. Extension code cannot add GPUI elements,
scripts, styles, or layout slots.

The action API accepts a context hint. The server resolves tenant membership,
room membership, profile ownership, document membership, and runtime connection
ownership before dispatch. A runtime connection hint uses
`session_id:connection_id`; the desktop captures those exact identifiers before
queueing the action. The dispatcher derives the extension target kind and ID from the
registered contribution and resolved context, ignoring client target fields.

## Extension actions and receipts

`POST /v1/extension-actions/invoke` and `POST /v1/tools/invoke` validate arguments
against one prepared contract. Review binds the actual invoker, schemas, limits,
package/grant revision and authorized source. Changed input, registration or
source requires fresh review. Approval consumption and the durable dispatch claim
commit together; a retained owner keeps the original actor, correlation and
shutdown lifetime after the caller disappears. Credentials, source authority and
package selection are checked again after process readiness and writer waits.

Preserve one `request_id` UUID for each intent across explicit retries, including
Read actions. The SDK generates a UUID when omitted, but applications needing
durable retry must supply and retain their own. The desktop preserves it through
review, approval and retry; CLI calls print it. An exact request/approval replay
returns `status: observed` with a redacted execution receipt, never another RPC or
a saved result body. Conflicting input, contract or approval identities refuse.
Omitting a UUID without a previously claimed approval creates a new intent.

Receipts distinguish `claimed`, `dispatching`, `acknowledged`, `refused` and
`outcome_unknown`. Dispatching records intent; `frame_started` records entry into
the writer, not complete transmission or native admission. An acknowledgement is
a validated extension result/error response, not database commit, rollback or
proof of read purity. Late responses remain separate from deadline failure.
Revocation can withhold a result while preserving known RPC observations. The
journal stores no arguments, result bodies, credentials or private diagnostic
text, and commits one terminal invocation audit with settlement. Unknown outcomes
never authorize automatic retry.

Inspect receipts through these separately audited owner reads:

- `GET /v1/extension-executions` returns at most 200 recent visible receipts.
- Add either `?request_id=<uuid>` or `?approval_id=<uuid>` for one identity lookup.
- `GET /v1/extension-executions/:id` reads one receipt.

Inspection needs no active database connection. Current actor/tenant and applicable
room/profile rights still apply; tenant-scoped tokens cannot inspect another
tenant. In Vim Extensions or Governed Tools, `o` opens receipts; j/k selects and
Enter or r refreshes without invoking work. CLI `sift tools receipts` and the
private standalone MCP receipt tool inspect the same records.

Current-file recovery refuses unissued claims and marks unresolved dispatch
intent unknown. Full restore marks every historical pending claim unknown, since
it may have executed after snapshot capture, and removes old operation approvals.
Tenant restore preserves target-side journal facts instead of importing snapshot
dispatch authority. Known terminal observations survive recovery.

## Captured desktop caller

Protected desktop work requires the handshake capability
`extension.caller-scope@1`. The UI captures its logical endpoint, verified instance
and daemon identity, principal and authenticated session before enqueueing. The
executor verifies that endpoint and uses `Client::capture_extension_caller` to
copy one credential into a scoped SDK client. Capture is bounded by 30 seconds.
That client never adopts a later session token or falls back to legacy routes.
Native connection targets stay bound to the original session/connection IDs.
Replies retain the original caller scope and UI generation; account, session,
endpoint, daemon or pending target changes prevent stale publication. Original
HTTP owners still settle their receipts independently of UI delivery.

Scoped requests retain normal authentication and protocol negotiation and require
exactly one of each assertion header:

| Header | Expected value |
| --- | --- |
| `x-sift-expected-instance` | Handshake `instance_id` |
| `x-sift-expected-daemon` | Handshake `daemon_generation` |
| `x-sift-expected-principal` | Authenticated principal ID as a decimal integer |
| `x-sift-expected-auth-session` | `whoami.auth_session_id`, or `-` when absent |

These assertions restrict existing authority; they do not grant it. The server
checks instance/daemon identity before routing and principal/session identity
after authentication, before handling the action. Missing, duplicate or changed
assertions refuse. Authenticated caller mismatches retain the actual actor in
HTTP audit; assertion values cannot replace that identity. The scoped SDK exposes
only these guarded routes:

| Method | Route |
| --- | --- |
| GET | `/v1/scoped/extensions` |
| GET | `/v1/scoped/tools` |
| POST | `/v1/scoped/tools/invoke` |
| POST | `/v1/scoped/extension-actions/invoke` |
| POST | `/v1/scoped/operation-approvals` |
| POST | `/v1/scoped/operation-approvals/:approval_id/approve` |
| GET | `/v1/scoped/extension-executions` |

Distinct routes make an older daemon refuse a request rather than ignore identity
headers on a legacy action route. The captured SDK transport refuses HTTP
redirects, including during identity capture and credential refresh. A 307/308 response cannot redirect captured work
to an ordinary route. Ordinary clients retain their existing redirect policy.
The scoped SDK validates approval IDs as UUIDs before constructing a URL, so an ID
cannot escape the guarded route family. Missing capability, mismatch and 404
require refresh/sign-in or a server update. Ordinary explicit clients retain their legacy
routes; scoped desktop work never downgrades to them. This adds no AI extension
execution or unattended authority, OS sandbox claim or native provider certification.

## Operator workflow

The management API is revision-guarded:

- `POST /v1/extensions/validate`
- `POST /v1/extensions/install`
- `GET /v1/extensions` and `GET /v1/extensions/:publisher/:name`
- `PUT /v1/extensions/:publisher/:name/selection`
- `PUT /v1/extensions/:publisher/:name/grants`
- `PUT /v1/extensions/:publisher/:name/tenants/:tenant_id`
- `POST /v1/extensions/:publisher/:name/rollback`
- `DELETE /v1/extensions/:publisher/:name`
- `POST /v1/extensions/:publisher/:name/purge`
- `GET /v1/extensions/:publisher/:name/diagnostics`

Uninstall stops selection but retains namespaced extension data as orphaned.
Purge is separate and explicit. Development overrides are configured with
`SIFT_EXTENSIONS__DEVELOPMENT_OVERRIDES`; team deployments additionally
require `SIFT_EXTENSIONS__ALLOW_HOSTED_DEVELOPMENT=true`. These directories are
development provenance and never imply signature trust.

Provider-neutral discovery is `GET /v1/providers`. Declarative extension
discovery is part of the extension descriptors. The OpenAPI document remains
the transport-level API artifact.

## Fault and graduation matrix

The host suite includes Driver v1–v4 negotiation, typed nested savepoints and
estimated-plan parsing.
The server's explicit executable acceptance case also checks uncertainty,
stale handles, failed recovery, admission expiry, queued policy revocation,
late/concurrent failure reports,
and abandoned HTTP audit/shutdown ownership. Run it after building the existing
fixture (standalone server test invocations do not build another package's bins):

```sh
cargo build -p sift-plugin-host --bin sift-conformance-provider
cargo test -p sift-server --lib provider_transaction_control::tests -- --ignored
cargo test -p sift-server --lib external_plan -- --include-ignored
cargo test -p sift-server --test external_estimated -- --ignored
cargo test -p sift-server --lib notifications::tests -- --ignored
cargo test -p sift-server --test external_notifications -- --ignored
```

HTTP plan/savepoint caller-loss and audit/shutdown checks live in
`external_estimated`; private admission, source and transaction-state checks
remain in the library tests. This keeps the full HTTP router out of the library
test compilation without dropping executable boundary coverage.

This corpus validates the host wire/lifecycle boundary. It does not certify a
provider's native database savepoint or estimated-plan implementation or award a
quality label. Estimated-plan acceptance exercises exact SQL/parameters, all three
formats, v1/v2 refusal, inherited v3 savepoints, generation fencing, independent
process survival, source replacement/admission expiry and abandoned/expired HTTP
audit and shutdown ownership.

Additional executable corpus boundaries:

| Boundary | Enforced cases |
| --- | --- |
| Archive | traversal, unsafe types, duplicate/case-colliding paths, undeclared files, size/count ceilings, digest and signature mismatch |
| Schema | invalid JSON/Draft 2020-12, remote or escaping references, byte/depth ceilings, secret/config separation |
| Handshake | wrong first message, timeout, version mismatch, identity/version/manifest/contribution mismatch |
| Framing | fragmented input, empty/oversized/malformed JSON, unknown message kind |
| Runtime | unknown response, out-of-order stream, credit/concurrency bounds, missed heartbeat, deadline, ignored cancellation, bounded kill |
| Secrets | credential-store separation and secret-shaped stderr/log redaction |
| Lifecycle | generation admission caps, bounded exponential restart, quarantine, candidate activation/drain/rollback state machine |
| Governance | deny-wins authorization, schema validation, classification, audit, approval binding/replay rejection, MCP filtering |

Run the repository graduation gates from the Nix development shell:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
```
