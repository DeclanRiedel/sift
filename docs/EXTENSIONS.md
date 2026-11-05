# Extensions

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
| Driver method family | 1–2 | Select the highest common host/manifest/Hello version; retain it in Welcome and the process generation. |
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
requires a manifest minimum of 2 and `driver.transactions@1`. Bulk ingest,
notifications, explain, and process control remain future typed families.

A database-provider package must advertise a compatible Driver family in Hello;
missing families, invalid ranges and duplicate family declarations fail startup.
Action-only packages need not advertise Driver. Existing v1 providers continue
using their v1 wire contract.

### External savepoints

A v2 provider declaring `driver.savepoints@1` implements `savepoint`,
`rollback_to_savepoint`, and `release_savepoint`. Each request contains
`{"transaction":"<wire-id>","name":"<label>"}`. The success response must echo
that exact object. Names contain 1–128 UTF-8 bytes without control characters;
the host passes labels without constructing SQL. Providers must quote native
identifiers and serialize native per-connection/transaction work themselves.

The existing transaction HTTP API, SDK and Vim controls dispatch these actions.
Host controls serialize per transaction, including commit/rollback and close
cleanup. Admission waits at most five seconds, bounded further by a shorter
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

A provider error, timeout, disconnect, malformed ack or mismatched transaction/
name stops the entire tenant/package generation and returns
`connection_invalidated` with an uncertain-outcome message. Reconnect is
required; no native rollback guarantee follows. Other connections in the same
generation are affected. Unrelated tenant/provider processes remain independent.
An expired observer cannot cancel the owned control or discard its eventual
host-state settlement. Admitted HTTP requests retain their audit owner if the
caller disconnects; an observation timeout is audited as a failure, even if the
remote operation later succeeds. Connection/session close retains cleanup
ownership until admitted control settles.

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
the dialog shows the selected target. Flat object input
schemas with string, integer, number, or boolean properties render as forms;
unsupported nested schemas remain unavailable in the desktop. The server
validates the complete input schema on invocation. Password-format fields are
masked, and action arguments never enter presentation storage or action labels.

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
`session_id:connection_id`; the desktop substitutes its active connection at
invocation. The dispatcher derives the extension target kind and ID from the
registered contribution and resolved context, ignoring client target fields.

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

The host suite includes Driver v1/v2 negotiation and typed nested savepoints.
The server's explicit executable acceptance case also checks uncertainty,
stale handles, failed recovery, admission expiry, queued policy revocation,
late/concurrent failure reports,
and abandoned HTTP audit/shutdown ownership. Run it after building the existing
fixture (standalone server test invocations do not build another package's bins):

```sh
cargo build -p sift-plugin-host --bin sift-conformance-provider
cargo test -p sift-server --lib provider_transaction_control::tests -- --ignored
```

This corpus validates the host wire/lifecycle boundary. It does not certify a
provider's native database savepoint implementation or award a quality label.

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
