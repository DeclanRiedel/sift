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
