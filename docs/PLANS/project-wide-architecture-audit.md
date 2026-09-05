# Project-wide architecture and performance audit

This is the broad follow-up to `backend-frontend-cleanup.md`, not a completion
claim based on that initial three-change pass. Audit every area below, follow
cross-layer findings, implement concrete improvements, and commit verified
milestones. Runtime-only tracking lives in temporary JSON outside the repository.

## Audit coverage

- [ ] Configuration: compare portable `sift.toml`, editor/schema help, examples,
  lock/apply, and effective runtime validation. Reject settings that validate
  but cannot start; document ownership and remove misleading examples.
- [ ] Local and hosted instances: inspect authentication topology, lifecycle,
  admission limits, public endpoint handling, and shared server boundaries.
- [ ] SSH networking: inspect bootstrap, process ownership, forwarding, endpoint
  validation, deadlines, reconnect, and cancellation versus local transport.
- [ ] Multiplayer: inspect room attachment/presence lifecycle, CRDT sync and
  reassembly limits, reconnect/replay, permissions, and shared results.
- [ ] SQL editor: inspect revision ownership, stale work, completion/diagnostic
  scheduling, cancellation, large-document work, and Vim interaction.
- [ ] Visual layout: inventory modal sizes, scroll containment, content fit,
  long/error content, and small windows; use GPUI layout assertions and actual
  rendered inspection where the available environment supports it.
- [ ] Broader backend/frontend hot paths: inspect metadata, cursor/results,
  task/resource lifetime, repeated computation, and architectural duplication.
- [ ] Update architecture decisions when a stable boundary changes and reconcile
  product/docs checklists with actual behavior.
- [ ] Complete final workspace formatting, Clippy, tests, diff review, and
  milestone commits; record any unverified external/live behavior explicitly.

## Working rules

Design each coupled fix before implementation. Keep findings concrete: trigger,
current behavior, intended behavior, affected boundaries, and validation.
Preserve secrets outside SQLite/logs and server-owned operations/audit. Keep
protocol types pure and CRDTs limited to query documents. API compatibility and
legacy interaction modes are not work priorities. Do not add unrelated smoke
scripts or CI workflows. Do not claim measured speedups or visual verification
without evidence.

## Findings and milestones

Investigation in progress. The sections below will collect reviewed findings,
fixes, evidence, and deliberately unresolved external dependencies.

### Configuration boundary

- [x] Require loopback binds for SSH manifests, matching runtime topology.
- [x] Intersect the two declared connection/query ceilings instead of letting
  format-v1 fields overwrite a stricter tenant policy. Document this in editor
  help and the operator guide; both fields remain usable without hidden priority.
- [x] Reject portable instance manifests in the development-config loader with
  an actionable `--instance-root` error, and clearly label the development template.
- [x] Share result/cursor/interval limit validation across manifest and runtime
  paths so non-instance startup cannot accept panic-inducing zero intervals.

Evidence: 19 instance-config tests and 226 server unit tests passed. Workspace
Clippy with warnings denied passed for the configuration milestone. Development
and manifest startup now use the same limits validator; timeout bounds also
agree. The example, operator guide, and configuration Wiki explain ownership
and intersecting ceilings.

### SSH helper lifecycle

- [x] Drain helper stderr from spawn, retaining only bounded diagnostics, so
  bootstrap cannot block on a full pipe before it emits readiness.
- [x] Bound readiness lines on initial connect and renewal, reject non-loopback
  forwarding URLs before sending credentials, and never echo token-bearing JSON
  in a parse error. Own the drain task through cancellation and early returns.
- [x] Bound control-master shutdown as well as startup and ordinary commands.

Evidence: two desktop output tests passed, covering IPv4/IPv6 loopback, invalid
and oversized readiness, redacted parse failures, and draining three times the
retention limit through a small pipe. Workspace Clippy passed. Live SSH hosts
were not contacted; endpoint identity pinning and capability exchange remain
in the existing connection path.

### Multiplayer lifecycle and memory

- [ ] Keep room creation/attachment/subscription atomic with idle eviction;
  evict after the last attachment too, regardless of drop order. Expiration
  must recheck a lease under its entry lock before removing refreshed presence.
- [ ] Bound replica chunk counts, bytes, and concurrent transfers; count received
  chunks instead of scanning all slots each arrival. Reject inconsistent or
  incomplete transfers, and clear abandoned transfers on resync/reconnect.
- [ ] Retain pending update IDs only. Reconnect already reconstructs missing
  updates from the CRDT version vector, so retaining payload copies is waste.
