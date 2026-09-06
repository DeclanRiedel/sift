# Maintainability cleanup

## Design

Keep server-owned operations and authorization intact. Extract domain modules,
not new parallel implementations. Preserve query/document events; queue limits
must expose overload or apply backpressure rather than silently lose edits.
Instance generation belongs to the pure instance-config crate. Database SQL
must respect provider capabilities, never infer an unknown engine as PostgreSQL.
Modal geometry belongs to one host; feature views own their content.

## Milestones

- [ ] Unify CLI/desktop starter manifests through a validated typed generator.
- [ ] Split HTTP handlers into domain modules without changing router middleware.
- [ ] Extract feature-owned workspace-shell actions and views.
- [ ] Bound desktop event queues with explicit overload and lossless delivery semantics.
- [ ] Remove provider-specific SQL construction from shell orchestration; use the server's existing DDL boundary where applicable.
- [ ] Standardize modal frame geometry, content scrolling and action visibility.
- [ ] Remove superseded Markdown work logs, repair references, and retain ADRs/current operator guidance.
- [ ] Run formatting, workspace Clippy and tests; review and commit milestones.

Performance claims require measurements. Local tests do not establish live
database/SSH behavior or pixel-level visual correctness.
