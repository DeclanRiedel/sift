# Maintainability cleanup

## Design

Keep server-owned operations and authorization intact. Extract domain modules,
not new parallel implementations. Preserve query/document events; queue limits
must expose overload or apply backpressure rather than silently lose edits.
Instance generation belongs to the pure instance-config crate. Database SQL
must respect provider capabilities, never infer an unknown engine as PostgreSQL.
Modal geometry belongs to one host; feature views own their content.

## Milestones

- [x] Unify CLI/desktop starter manifests through a validated typed generator.
- [x] Split HTTP handlers into domain modules without changing router middleware.
- [x] Extract feature-owned workspace-shell actions and views.
- [x] Bound desktop event queues with explicit overload and lossless delivery semantics.
- [x] Remove provider-specific SQL construction from shell orchestration; use the server's existing DDL boundary where applicable.
- [x] Standardize modal frame geometry, content scrolling and action visibility.
- [x] Remove superseded Markdown work logs, repair references, and retain ADRs/current operator guidance.
- [x] Run formatting, workspace Clippy and tests; review and commit milestones.

Performance claims require measurements. Local tests do not establish live
database/SSH behavior or pixel-level visual correctness.

## Documentation retirement

Removed three completed implementation logs: `backend-frontend-cleanup.md`,
`template-refresh.md`, and `shared-room-infrastructure.md`. Implementation and
validation history remains in Git. Current configuration guidance lives in
`docs/INSTANCE-CONFIG.md`; collaboration guidance lives in the Shared Rooms wiki.
The larger audit, canonical feature inventory, active plans, and ADRs remain.
The historical audit's reference to the removed cleanup log was repaired.

## Implemented boundaries

- CLI and desktop use the same validated starter generator. User values are
  assigned as typed fields rather than interpolated into TOML. Both start with
  `default/postgres` and the example's explicit runtime safety defaults.
- HTTP authentication/identity/invitation handlers and room/document handlers
  have domain modules; the router and middleware ordering are unchanged.
- Vault actions, vault forms, modal views and partial SQL previews are separate
  shell modules. This is incremental decomposition, not elimination of all
  large shell state or every inline presentation helper.
- Executor command admission is capped at 128 and never blocks the UI. Full or
  closed queues return a failure and publish a coalesced visible notice, including
  for callers that ignore the return value. Accepted commands retain FIFO order.
  Room presence is capped at 128 events; document output at eight snapshots/events.
  Producers await capacity; snapshots reserve a slot before serialization.
  CRDT input and general executor result/lifecycle channels remain lossless and
  unbounded: this is not a claim of a global desktop memory quota.
- Full DDL comes only from the existing audited server request. Static preview
  and object-designer drafts live in `sift-snippets`, reject unknown providers,
  and use SQL Server bracket escaping. Partial catalog views remain labelled.
  Composed DDL now uses the server's bounded driver-task runner; stalled schema
  or view-definition calls respect the request deadline. Unavailable/failed
  requests no longer leave a misleading loading or catalog-fallback message.
- Every modal uses the shared card frame. Create/Edit Vault additionally use
  independently scrollable content and fixed, wrapping action rows. Other forms
  retain their existing content-specific scrolling and Vim focus behavior.

## Validation

Final `cargo fmt --all -- --check`, workspace Clippy with all targets and warnings
denied, `cargo test --workspace --quiet`, and diff checks passed. The workspace
run includes 413 UI tests, 33 desktop tests, and five driver-timeout regressions.
Existing ignored/live-feature suites were not enabled. Desktop linking used the
existing temporary `LIBRARY_PATH=/tmp/sift-refactor-HGLfxo` XKB development alias;
no system libraries or repository build settings were changed.

Regression coverage includes typed starter validation, bounded command rejection
and visible admission errors, ordered snapshot delivery/receiver closure, exact
provider matching and identifier escaping, DDL timeout/failure behavior, and
long-error vault forms at 480×320. The existing modal matrix still passes at
480×400, 800×600 and 1280×900. This is rendered geometry validation, not native
pixel inspection or measured performance benchmarking.
