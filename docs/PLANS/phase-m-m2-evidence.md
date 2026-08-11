# Phase M2 — Connection and Workspace Lifecycle Evidence

Recorded: 2026-08-11

Status: **complete.** M2 establishes the desktop lifecycle projection through
the public SDK. Query editing and execution remain M3 work.

## Local and remote lifecycle

- `SiftApp` owns one Tokio client runtime and one process-wide local-server
  manager. A window acquires a lease; the first disconnected lease can start
  the bundled `sift-launcher`, while the final lease stops only a launcher the
  desktop owns. Attaching to an already-running server does not claim it.
- The window paints restored presentation state before starting lifecycle I/O.
  A channel moves selected-instance, negotiation, authentication, navigation,
  reconnect, and degraded events back across the GPUI boundary.
- Local, SSH, and hosted instance descriptors feed the same `Client`-based
  loader. Endpoint/tunnel acquisition and secret-backed authentication happen
  before that boundary; no server, metadata, or driver crate enters the client
  dependency graph.
- The local supervisor checks health after readiness. Transport loss clears
  ephemeral presence and retries with bounded exponential backoff; a fresh SDK
  client repeats protocol negotiation and authoritative navigation loading.
  Expired authentication, revoked access, and incompatible protocol states do
  not spin in an automatic retry loop.

## Workspace and room projection

- Tenants, connection profiles, rooms, and virtual workspaces arrive
  progressively. Workspace capability flags determine whether Git and Runs
  affordances appear in navigation.
- The SDK path supports browsing and creating virtual workspaces. Opening one
  persists only its instance/workspace reference for that window. A restored
  reference is retained while loading, then explicitly cleared with a toast if
  the authoritative navigation set no longer contains it.
- A restored workspace joins its room WebSocket and preserves the initial
  presence snapshot consumed by the attach acknowledgement. Heartbeats keep
  the attachment live. Follow mode accepts only a current attachment and drops
  a departed target.
- Room presence, attachment IDs, and follow selection are absent from the
  serialized presentation contract and are rebuilt after reconnect.

## Failure exercises

Deterministic tests cover:

- bounded restart/reconnect delay and offline/degraded labels;
- distinct 401 expired-auth and 403 revoked-membership classification;
- replacement of stale navigation and removal of a stale restored workspace;
- selection of known local/SSH/hosted-style instance descriptors and rejection
  of removed instance IDs;
- initial presence, follow validation, and departed-target cleanup; and
- two desktop windows sharing one local-server lifecycle lease.

The native harness still lacks an X authorization cookie, so visual inspection
against a live local server remains an external acceptance check. GPUI visual
tests exercise the real entity/update path for lifecycle reconciliation.

## Verification

```text
nix develop . --command cargo fmt --all --check
nix develop . --command cargo clippy --workspace --all-targets -- -D warnings
nix develop . --command cargo test --workspace
nix develop . --command cargo deny check
nix develop . --command scripts/check-client-dependency-firewall.sh
```
