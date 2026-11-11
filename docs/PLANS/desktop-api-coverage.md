# Desktop API and workflow coverage

Work tracking (2026-11-11): **Reference / scoped evidence**. Remaining product work: [current TODO](../TODO.md#desktop-quality-and-architecture).

Status: current workflow map, reconciled 2026-11-11. The old P0–P6 per-method
backlog is retired: it listed many shipped workflows as not started and contained
broken references. Use [TODO](../TODO.md) for work selection and the
[feature inventory](ide-parity-and-provider-extensibility.md) for product coverage.
A working workflow does not imply every SDK method needs a separate screen.

## Completed scoped workflows

- [x] Connection profiles, ad-hoc sessions, health, authentication and SSH lifecycle.
- [x] Schema/catalog navigation, DDL, search, history, saved queries and capability gating.
- [x] Completion, diagnostics, formatting, quick fixes, rename/refactoring and statement selection.
- [x] Streaming results, cancellation, paging/resume, typed parameters, grid edits and imports.
- [x] Transactions, savepoints, bounded process monitoring and reviewed termination.
- [x] Rooms, live documents, presence/follow and shared result references.
- [x] Workspace trees/checkpoints, projection and Git status/diff/stage/commit/remotes.
- [x] Catalog snapshots/comparison, supported migration preview/apply and diagrams.
- [x] Run configurations, schedules, durable logs, cancellation and recovery surfaces.
- [x] Transfer recipes, import/export progress and artifact/quarantine handling.
- [x] Parameter-aware plan captures, private benchmark/Profile libraries and comparisons.
- [x] Declarative extension forms/panels and authorized context-target actions.
- [x] Scoped audit inspection, one-use approvals and governed-tool review.
- [x] Interactive reviewed AI SQL/database/query/extension actions and captured callers.
- [x] Hosted TLS WebSocket result delivery (`e3d6b13e`); real SDK table rows verified.

## Partial workflows and next work

- [ ] Delegation review in Vim/CLI, then bounded delegated startup/dispatch,
  durable companions and unattended/scheduled recovery.
- [ ] Broader principal, membership, authorization, policy, quota, credential,
  token/key and database privilege administration beyond the bounded forms.
- [ ] Advanced provider DDL, migrations, plans, maintenance and recovery UI.
- [ ] Instrumented/controlled performance populations and broader transfer shapes.
- [ ] Type-aware/asynchronous filter controls and remaining diagram/accessibility work.
- [ ] Complete keyboard equivalence, measured performance/memory and native recovery gates.
- [ ] Signed desktop distribution, background update/restart/rollback and native acceptance.

See [desktop architecture and validation](gpui-desktop.md),
[provider completion](provider-and-operations-completion.md),
[performance](query-performance.md), [AI execution](ai-execution.md), and
[artifact gates](../DESKTOP-ARTIFACT-VALIDATION.md). Native Windows/macOS acceptance
remains deferred; historical receipts do not certify the current artifact.
