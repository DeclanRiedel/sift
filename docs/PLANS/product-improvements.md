# Product refactors and improvements

Status: active, requested 2026-09-06. Commit completed milestones with validation.
The canonical feature inventory remains the source of product feature status.

## Milestones

- [x] Reconcile the historical desktop API checklist with canonical feature status.
- [x] Bound active metadata connections and extract pool ownership.
- [ ] Split workspace shell by feature state and event ownership.
- [ ] Split HTTP handlers by domain while preserving authorization and audit.
- [ ] Separate desktop executor domains and task lifetimes.
- [ ] Consolidate supported interaction paths around Vim.
- [ ] Modularize metadata and SDK domain APIs without interface changes.
- [ ] Measure responsiveness and memory with existing large fixtures.
- [ ] Complete crash/restart/offline/auth-expiry recovery validation.
- [ ] Surface unsafe mutation and Cartesian JOIN inspections.
- [ ] Complete foreign-key JOIN assistance, then explicit multi-hop path selection.
- [ ] Add saved grid layouts and bounded foreign-key value selection.
- [ ] Improve DDL fidelity and engine round-trip coverage.
- [ ] Add transfer dry-run, quarantine, resume, and type-mapping workflows.
- [ ] Add operational metrics/traces and monitoring workflows.
- [ ] Complete Vim/accessibility/platform and signed-update validation.

## Design: metadata admission

The file-backed pool currently retains at most 16 idle connections but has no
active connection cap. Use the same conservative ceiling for total checked-out
connections. Reserve a permit before opening SQLite, return it on open failure
and guard drop, and reject saturation immediately with a typed service-unavailable
error. Do not block Tokio workers or create an unbounded queue of waiting tasks.
Retain the existing SQLite busy timeout for admitted database operations.
Extract the pool into its own module so admission and connection ownership stay
together. Test exhaustion, release, failed open, and concurrent admission.
The limit is an initial safety bound, not a measured throughput optimum.

## Validation

Run formatting, workspace Clippy with warnings denied, and workspace tests.
Use focused behavior tests for new concurrency and state transitions; preserve
existing tests for structural moves. Record external/platform validation limits
explicitly. Do not enable CI or introduce smoke scripts.

## Milestone evidence

Metadata admission: three focused tests passed (capacity/reuse, failed-open
release, simultaneous admission). Workspace Clippy passed. Full workspace tests
are running; the HTTP error regression checks the typed `metadata_busy` 503.
Generated incremental build artifacts were cleared to recover disk capacity;
source files and dependency caches were retained.
