# Phase H — Remote Development And Distribution

Status: implemented; release and SSH matrices are enforced in CI.

This plan graduates ADR-021, refines ADR-016, and graduates ADR-015. It is the
implementation contract for Phase H in `server-build-list-v2.md`.

## Goal

A thin local client can use a Sift server on a machine reachable through SSH
without making that server publicly reachable. The remote server keeps product
state and database connections; the local process owns SSH bootstrap and byte
forwarding. The same release artifacts also support a foreground local server,
a long-lived daemon, and an immutable container.

Phase H must preserve four existing boundaries:

- SSH transport never widens the personal-loopback authentication bypass.
- The remote server remains authoritative for sessions, rooms, policy, audit,
  query execution, and durable document state.
- Application protocol compatibility is negotiated independently from the
  executable's release version.
- An update is never trusted merely because it arrived over HTTPS.

## Non-goals

- A Sift-operated relay, identity broker, account system, or hosted control
  plane.
- Sharing one personal SSH daemon with collaborators who cannot SSH to that
  host. Collaborators continue to use a directly network-hosted team instance.
- Synchronizing a remote filesystem or repository. Phase L owns workspace and
  VCS topology.
- Transparently replaying queries after a disconnect. The result of an
  interrupted request can be indeterminate, especially for writes.
- Updating containers from inside the container.
- Replacing OpenSSH, managing SSH credentials, or weakening the user's host-key
  verification policy.

## Locked topology

Runtime mode, transport, and deployment policy remain separate axes:

| Concern | Values | Phase H meaning |
| --- | --- | --- |
| Deployment policy | `personal`, `team` | Principal, tenant, and authorization policy |
| Transport | `loopback`, `network`, `ssh-proxy` | How the HTTP/WebSocket peer reaches the server and whether implicit local trust is possible |
| Runtime mode | `in-process`, `daemon`, `container` | Process ownership, shutdown, logs, and update activation |

An SSH remote uses `deployment=personal|team`,
`transport=ssh-proxy`, and `mode=daemon`. `ssh-proxy` binds an ephemeral remote
loopback TCP port, but it is not `transport=loopback`: `loopback_bypass` is
always false and startup rejects any attempt to combine them.

```text
local client
    |
    | HTTP/WebSocket to an ephemeral local listener
    v
local Sift SSH helper
    |
    | one byte-forwarding SSH channel per accepted TCP connection
    | (multiplexed through an OpenSSH control master when available)
    v
remote 127.0.0.1:<ephemeral>  ->  sift-server
                                      |
                                      +-- metadata / documents / audit
                                      +-- database drivers and live sessions
```

The local listener is the port-forward analogue. It binds only loopback on an
OS-assigned port and relays bytes without parsing HTTP or WebSocket frames.
Each accepted connection uses an SSH direct-stream channel to the daemon's
remote loopback port. OpenSSH control multiplexing amortizes authentication and
connection setup; a dedicated SSH connection is a correctness-preserving
fallback when control masters are unavailable. Control-master sockets live in
a private runtime directory, use a hash-derived name rather than raw host/user
text, and are removed when the local helper exits.

Sift invokes the user's system OpenSSH client and honors their selected host,
config, agent, jump hosts, and `known_hosts`. It never adds
`StrictHostKeyChecking=no`, captures passwords, copies private keys, or puts
capabilities in command arguments or environment variables.

The daemon is independent of the SSH process. Closing the client, losing the
network, or replacing the control master closes forwarding channels but does
not signal the daemon.

## Remote bootstrap protocol

Bootstrap is an explicit state machine with structured, size-bounded messages.
Machine-readable output goes to stdout as length-delimited JSON; diagnostics go
to stderr. Secret-bearing fields use redacted debug representations and are
never logged.

### 1. Establish and identify

1. Resolve the SSH destination through OpenSSH rather than interpreting
   `~/.ssh/config` inside Sift.
2. Establish a control master when supported, retaining the host-key decision
   made by OpenSSH.
3. Run a fixed remote probe command. The probe reports OS, architecture,
   executable version, supported protocol range, install layout version, and
   whether an instance daemon is already ready. Values are treated as
   untrusted bounded input.
4. Compare the remote protocol range with the local client's range. Reuse a
   wire-compatible daemon even when its release version differs. Executable
   equality is not a prerequisite for connection.

The release version remains semver. Protocol versions remain monotonically
increasing integers. Neither is inferred from the other.

### 2. Select and install an executable

If the probe is absent or incompatible, the local helper selects an artifact
for the reported target from its verified release cache or the client-bundled
server payload. It does not ask the remote host to download from an inferred
URL.

The artifact must already have passed the signed-manifest, size, and SHA-256
checks described below. It is uploaded with the SSH file-transfer subsystem to
a user-owned staging path, then atomically installed under a versioned,
content-addressed directory. Concurrent installers serialize on a lock and
converge on the same artifact. The installed executable self-checks its
expected digest before servicing the bootstrap request. Install directories,
state files, and daemon descriptors must not be group- or world-writable.

A failed or interrupted upload leaves only a removable staging file. It never
changes the active daemon or current-version pointer.

### 3. Start or reuse the daemon

The remote bootstrap command acquires a per-instance singleton lock. It either:

- validates the existing daemon descriptor and performs a loopback readiness
  handshake; or
- starts `sift-server` as a detached daemon with
  `transport=ssh-proxy`, `loopback_bypass=false`, and an OS-assigned loopback
  port, then waits for bounded readiness.

The descriptor contains the instance id, PID/start identity, endpoint, release
version, protocol range, and a random daemon generation. It contains no bearer
credential. Stale descriptors are replaced only while holding the singleton
lock. Process identity and readiness are checked; a recycled PID is not trusted.

Daemon stdout/stderr use configured files or the host service manager and never
inherit the SSH channel. Startup failure returns a structured error and bounded
diagnostic tail with secrets redacted.

### 4. Resolve a principal and issue a capability

SSH proves access to an OS account, not automatically to an arbitrary Sift
principal.

- In a personal deployment, the bootstrap command may resolve the
  bootstrapped local principal only when the instance state and secret material
  are owned by the invoking OS account with private permissions.
- A registered Sift Ed25519 principal key can always prove identity through the
  existing bounded, one-use challenge flow.
- A team deployment requires registered-key proof. An OS username, requested
  principal id, SSH username, or client-supplied claim is never sufficient.

After identity proof, bootstrap creates `SshProxyCapabilityClaims` with the
exact configured instance audience, resolved principal id, random capability
id, issue time, and a short expiry. The claims are encoded in a versioned,
authenticated envelope using a dedicated instance MAC key held by
`SecretStore`. The MAC covers the exact payload bytes and is verified before
deserialization. SQLite stores only the capability id/digest, principal,
audience, daemon generation, expiry, and consumption state; it never stores
the bearer envelope.

The capability is returned only over the authenticated SSH command's stdout.
It is:

- audience- and instance-bound;
- short-lived;
- atomically one-use;
- rejected after principal/key revocation; and
- invalid on any parse, MAC, time-window, generation, or audience mismatch.

The client sends it to a narrowly rate-limited
`POST /v1/auth/ssh-proxy/exchange` endpoint through the tunnel. Successful
exchange atomically consumes it and returns a short-lived access grant for that
principal and daemon generation. It does not return a portable refresh token.
Renewal repeats bootstrap/key proof over SSH. Capability issuance, successful
exchange, replay, expiry, and rejection are audited without recording secret
bytes.

### 5. Forward and hand off

The bootstrap response gives the local helper the remote loopback endpoint,
opaque instance id, daemon generation, server release version, protocol range,
and capability. The helper starts its local loopback listener, exchanges the
capability, completes the application handshake, then hands the local base URL
and authenticated SDK client to the product client.

The helper reports ready only after all three gates succeed:

1. SSH forwarding reaches the expected daemon generation.
2. Capability exchange resolves the expected principal.
3. The application protocol handshake selects a mutually supported version.

## Reconnect and survival contract

The local helper reconnects with bounded exponential backoff and jitter. It
first reuses the existing control master, then establishes a new SSH connection.
It never disables host-key checks to make reconnection succeed.

On reconnection:

- the same ready instance id and daemon generation allow the current access
  grant and server resource ids to be retried where their APIs are idempotent;
- an expired grant causes a fresh capability bootstrap;
- a changed generation or missing runtime resource causes the client to reopen
  its Sift session and managed connections;
- room documents resynchronize through the Phase G Loro version-vector flow;
  presence is re-announced and result references are rediscovered over HTTP;
- durable rooms, documents, profiles, history, and audit survive a daemon
  restart, while process-local sessions, transactions, cursors, presence, and
  live database connections do not; and
- an interrupted execute/import/edit request is reported as
  `outcome_unknown` unless the operation contract has an idempotency key and
  the server can prove its committed result. It is never automatically
  replayed merely because the tunnel returned.

The initial implementation may leave server-side sessions alive until their
normal lifecycle cleanup after an SSH drop. Phase H does not add CRDT semantics
to sessions or results.

## Real application handshake

ADR-016's optional one-way version pin becomes an SDK-enforced, two-sided
handshake.

### Wire contract

`POST /v1/handshake` is a public, aggressively rate-limited endpoint with a
small pure-serde request and response:

- request: client release version, client kind, and inclusive protocol
  `[minimum, maximum]`;
- response: server release version, inclusive supported range, selected
  protocol, opaque instance id, daemon generation, deployment/transport
  descriptors, and a bounded set of stable capability names.

The server selects the highest mutually supported integer. Initially both
ranges are `[1, 1]`; the range contract exists now so adding N-1 support later
does not require inventing a second negotiation mechanism. No overlap returns
HTTP 426 with `unsupported_protocol_version` and the server's supported range.
The handshake response carries the selected value in
`X-Sift-Protocol-Version`.

Handshake metadata contains no secret, principal, tenant, path, hostname, or
database information. Authentication and authorization still govern product
routes.

After selection:

- every SDK HTTP request sends the exact selected
  `X-Sift-Protocol-Version`;
- every HTTP response, including errors, must echo the exact value and the SDK
  rejects a missing or different header before decoding its body;
- every WebSocket upgrade sends the exact value and validates it in the 101
  response before exposing the socket; and
- one connection never changes protocol version in place.

The SDK keeps `Client::new` as a cheap builder but lazily performs one shared
async handshake before its first HTTP or WebSocket operation. It caches the
selected version together with instance id and generation. Concurrent first
calls share the result. A generation change invalidates that cache and forces a
new handshake. Callers that need eager diagnostics can call `connect`.

Raw third-party clients may call the endpoint themselves. Once Phase H ships,
an absent version header is accepted only by the handshake and health/readiness
probes; protected and product routes fail with
`protocol_handshake_required`. This replaces the current assumption that an
unpinned client is compatible.

Release update decisions use both ranges:

- a compatible client may continue with a different server release;
- a compatible server need not be replaced just to match semver;
- an incompatible remote server triggers verified artifact selection; and
- lack of overlap is always an explicit error, never an attempted downgrade
  outside the ranges each side declared.

## Signed background updates

### Trust and manifest

Each release channel has a small manifest served from a distribution-configured
HTTPS origin. No URL is derived from an SSH host or accepted from an unsigned
response. A detached Ed25519 signature covers the exact manifest bytes; the
client verifies the signature against release public keys embedded in the
currently trusted binary before parsing JSON.

The signed manifest contains:

- schema version, channel, monotonically increasing sequence, release version,
  publication time, and expiry;
- minimum updater version and supported application protocol range;
- for each target, artifact URL, byte length, SHA-256 digest, archive format,
  executable path, and optional symbols/SBOM references; and
- rollout eligibility data that does not identify a user.

HTTPS protects transport; the signature establishes release authority; the
length and digest bind the artifact. The updater rejects an unknown schema,
bad signature, wrong channel/target, expired manifest, lower observed sequence,
unexpected size, digest mismatch, unsupported archive layout, downgrade, or
unsafe path. Previously observed sequence is stored durably per channel.
Release-key rotation ships overlapping trusted keys in an earlier signed
release; revocation requires a new client release or operator intervention.

The updater collects no telemetry in Phase H.

### Staging and activation

Checks run in the background with bounded timeouts, jitter, and a configured
channel (`stable` by default; changing channel is explicit). Downloads stream
to a private staging file with a hard size ceiling. Verification completes
before extraction. Extraction rejects absolute paths, parent traversal,
links, duplicate paths, and anything outside the expected layout.

Verified releases install into immutable versioned directories. A small
atomic current-version pointer selects the next launch; the running executable
is never overwritten. The previous known-good version is retained. Activation
does not interrupt queries or force a restart.

On next launch, the owner starts the candidate and requires process readiness
plus a compatible application handshake. Failure before the health deadline
restores the previous pointer and reports a rollback. Database operations are
not used as the health check. Metadata migrations must therefore obey the
project's existing forward-compatible migration policy; an update with an
irreversibly incompatible migration requires a separately designed release
gate and cannot rely on binary rollback.

Remote bootstrap uses the same manifest verifier and content-addressed artifact
cache. Both local and remote self-check the expected artifact digest, so the SSH
path does not create a second release trust model.

## Runtime modes

`--mode` controls lifecycle policy, not authorization:

| Mode | Owner | Shutdown/logging | Updater |
| --- | --- | --- | --- |
| `in-process` | Parent application/foreground invocation | Parent-owned; inherited foreground diagnostics | Parent checks, stages, and activates its matched client/server bundle |
| `daemon` | Sift daemon or OS user service | Signal-driven ADR-018 drain; durable configured logs | Daemon may check and stage; activation occurs through an explicit restart or bootstrap |
| `container` | Container orchestrator | PID 1 signal semantics; stdout/stderr | Disabled; image replacement is the updater |

The name `in-process` describes parent-owned lifecycle. The first binary
implementation may still spawn a foreground child until a product client links
the server runtime as a library; its externally visible lifecycle contract is
the same.

An SSH remote is daemon mode. Container mode rejects self-update configuration.
Daemon mode never restarts itself while queries are active merely because an
update is ready. All modes use ADR-018 drain behavior, and a forced deadline is
visible in logs and audit/metrics rather than presented as a clean shutdown.

## Failure and security gates

Phase H is not complete without tests for:

- host-key rejection and control-master fallback;
- truncated, oversized, malformed, or hostile probe/bootstrap messages;
- interrupted/concurrent upload and stale daemon descriptor recovery;
- `ssh-proxy + loopback_bypass` startup rejection;
- personal file-ownership checks and team registered-key proof;
- capability expiry, replay race, audience/generation mismatch, principal/key
  revocation, and log redaction;
- tunnel loss during reads, writes, transactions, cursor streaming, document
  updates, and capability renewal;
- daemon survival across control-master loss and durable/ephemeral state
  recovery across daemon restart;
- handshake success, no overlap, missing/mismatched response headers,
  concurrent SDK first calls, and HTTP/WebSocket parity;
- signed-manifest tamper, replay, expiry, target mismatch, oversized artifact,
  digest mismatch, hostile archive, interrupted staging, activation failure,
  and rollback; and
- updater behavior in all three runtime modes, including a hard refusal to
  self-update in container mode.

The standard `fmt`, workspace `clippy -D warnings`, workspace tests,
`cargo-deny`, and secret-scanning gates remain mandatory. Release CI must also
produce reproducible target artifacts where supported, SBOMs/checksums, the
signed raw manifest, signature-verification fixtures, and an end-to-end
install/bootstrap test from a clean SSH account.

## Implementation order

1. **H1 — Handshake foundation.** Add protocol range DTOs and errors, the
   handshake endpoint, mandatory selected-version middleware, SDK lazy/eager
   negotiation, and HTTP/WebSocket response validation.
2. **H2 — Runtime modes.** Separate lifecycle mode from deployment/transport,
   implement daemon descriptors/singleton locking and container restrictions,
   and keep ADR-018 shutdown green in every mode.
3. **H3 — SSH capability.** Implement authenticated capability encoding,
   durable atomic consumption, personal ownership and team key-proof
   resolution, exchange, access-grant renewal, audit, and adversarial tests.
4. **H4 — Bootstrap and forwarding.** Implement probe, target selection,
   verified upload/install, detached daemon start/reuse, local byte forwarder,
   control-master optimization/fallback, reconnect, and recovery reporting.
5. **H5 — Release trust and updater.** Implement signed manifest parsing,
   streaming artifact verification, safe staging, version pointers,
   mode-specific activation, rollback, and remote cache reuse.
6. **H6 — Release graduation.** Add cross-target release CI and the full local,
   remote-SSH, daemon-restart, container, compatibility, and update-failure
   matrix; update the public protocol/OpenAPI and operator documentation.

H1 precedes all remote work: bootstrap must never decide compatibility from
semver alone. H2 precedes H4 so the SSH helper starts a defined lifecycle
rather than an ad-hoc background process. H3 precedes exposing the forwarder.
H5 may proceed in parallel after H1, but remote artifact installation cannot
graduate until both are complete.
