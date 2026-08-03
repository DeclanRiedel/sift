# Extensions

Sift extension packages are immutable ZIP archives with a strict
`sift-extension.toml`, canonical `sift-extension.lock`, and optional Ed25519
signature. Server code runs as a directly executable child process over
length-prefixed JSON stdio. Sift does not invoke a shell, inherit the server
environment, or install an extension's language runtime.

The Rust wire contract and generated JSON Schema source are in
`crates/extension-protocol`. The normative design is
`docs/PLANS/phase-i-extensibility.md`.

## Compatibility

| Contract | Current version | Compatibility rule |
| --- | ---: | --- |
| Public HTTP/WebSocket protocol | 1 | A package range must contain `1`. |
| Extension RPC | 1 | The manifest and `hello` range must contain `1`. |
| Driver method family | 1 | The manifest and negotiated family range must contain `1`. |
| Manifest | 1 | Unknown fields and any other schema version are rejected. |
| Provider configuration schema | 1 | Draft 2020-12; the descriptor publishes the schema version. |

There is no historical pre-release `Engine` codec. Protocol v1 uses
`ProviderId`, `DialectId`, `ProviderRef`, and explicit capabilities throughout.
Unknown capabilities are ignored by older clients; missing required
capabilities fail explicitly.

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
`sift_certified`. Phase I's external `acme/conformance` executable is a test
fixture, not a supported production database provider.

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

The executable corpus covers:

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
