# Current development TODO

Reconciled 2026-11-11 against the canonical feature inventory, focused plans,
implementation, and accepted commits through `e3d6b13e`.

Use this file to choose the next development slice. The
[feature inventory](PLANS/ide-parity-and-provider-extensibility.md) records product
coverage; the [plan index](PLANS/README.md) identifies completed contracts and
active detailed plans. `[x]` means the stated scope is implemented; `[ ]` is
remaining work. A completed bounded slice does not complete its parent feature.
Native platform evidence and external blockers are tracked separately below.

## Next milestone

- [ ] Add captured human AI delegation review controls in Vim and CLI: preview
  exact material, create/list/inspect, approve/deny/revoke, and tenant policy.
  Use the existing scoped APIs and immutable fingerprints; preserve the original
  caller, source and one-use authority. See [AI execution](PLANS/ai-execution.md).
- [ ] Follow with delegated startup/dispatch and provenance only after bounded
  native command/object/database/role proof. Keep unattended execution disabled
  until its companion, cancellation and recovery lifecycle is accepted.

## Desktop updates

- [x] Server artifact verification, staging, readiness/handshake activation and
  rollback foundations exist. This is separate from desktop update acceptance.
- [ ] Produce prebuilt desktop release artifacts and configure real release
  channels, publisher identities and trusted signing keys.
- [ ] Download and verify desktop candidates in the background without changing
  the running application or compiling Rust on the recipient's update path.
- [ ] Add “Restart to update”, staged activation, workspace restoration, startup
  verification and rollback. Respect package-manager-owned installations.
- [ ] Productize host-aware launcher names and bundled stable icons; prevent
  development/hosted entries from listing the same window twice. The local
  Bedrock launcher repair is complete, not portable installer acceptance.
- [ ] Accept the complete signed desktop update/rollback path on supported hosts.

Details: [update contract](REMOTE-AND-UPDATES.md),
[artifact matrix](DESKTOP-ARTIFACT-VALIDATION.md).

## AI execution

- [x] Interactive human-approved private SQL, typed database actions, query-text
  edits and governed extension execution, with captured callers and receipts.
- [x] Native migration evidence, retained automatic review sources and atomic
  shared-room proposal commits.
- [x] Scoped human delegation material/review/approval/withdrawal and tenant
  policy HTTP/SDK APIs; encrypted authority and one-use claims.
- [ ] Add explicitly reviewed public-room write grants and disclosure/provenance
  boundaries after the delegation startup/dispatch milestone above is accepted.
- [ ] Prove bounded native command effects, object scope and database/role
  identity before delegated mutation admission.
- [ ] Implement durable private-editor companions, headless ownership, unattended
  and scheduled lifecycle, Stop/expiry/revocation, and restart/recovery.
- [ ] Complete family-specific native and restricted-provider acceptance without
  retrying uncertain writes or treating RPC acknowledgement as database commit.

Details: [execution and delegation](PLANS/ai-execution.md),
[provider integration](PLANS/ai-agent-integration.md).

## Extensions and MCP

- [x] Bounded declarative forms/panels, authorized context actions, savepoints,
  estimated plans, notifications, prepared approvals and retained RPC receipts.
- [x] Captured desktop callers, targeted Stop, runtime readiness and interactive
  reviewed AI extension Execute.
- [ ] Add external analyzed/Profile capture with its own bounded native contract.
- [ ] Certify native third-party provider semantics and complete broader provider
  capabilities; host wire/lifecycle tests alone do not establish engine quality.

Details: [extensions](EXTENSIONS.md), [MCP](sift-mcp.md),
[provider completion](PLANS/provider-and-operations-completion.md).

## Performance workbench

- [x] Serial/paired/concurrent suites, parameter matrices, private saved runs,
  notes, retention, source context, attached profiles and plan comparisons.
- [x] Bounded untimed paired-result verification and descriptive slowdown reports.
- [ ] Add optional fetch/render measurements and separate instrumented populations.
- [ ] Capture database CPU/IO/saturation and establish controlled cache/load and
  environment comparisons; do not infer these from client occupancy or latency.
- [ ] Add isolated test-database setup/teardown and reviewed index experiments.
- [ ] Design explicit advanced write benchmarking and non-rollback side effects.
- [ ] Add optional historical server statistics with separate provenance/clocks.
- [ ] Complete controlled regression inference, sampling-assumption evidence,
  repeated-comparison handling and broader permission-checked artifact sharing.
- [ ] Expand SQLite profiling beyond estimated plans and bounded read timing only
  when the native interface can provide trustworthy counters.

Details: [performance workbench](PLANS/query-performance.md),
[load](PLANS/benchmark-load.md), [measurement evidence](PLANS/performance-measurements.md).

## Security and administration

- [x] Hosted/local owner admission, invitations, bounded admin surfaces, audited
  native mutation owners and source-bound reviewed database administration exist.
- [ ] Finish broader principal/identity/session, tenant/membership, RBAC,
  connection-policy, quota/rate-limit, audit, API-token/signing-key and approval
  administration. Existing bounded surfaces remain usable, not globally complete.
- [ ] Expand PostgreSQL credentials, additional privilege families, grant-chain
  inspection/cascading revocation and dependency remediation.
- [ ] Expand SQL Server login credentials, contained/external identity families,
  server-token/grant-chain inspection and additional permissions/ownership.

Details: [inventory](PLANS/ide-parity-and-provider-extensibility.md#security-and-administration),
[PostgreSQL gaps](PLANS/postgres-provider-gaps.md),
[SQL Server gaps](PLANS/sqlserver-provider-gaps.md).

## PostgreSQL

- [x] Scoped driver graduation, native DDL, role/RLS/ACL editors, replication and
  statistics inspection, historical deadlock reads, extension/partition actions.
- [ ] Extend lossless DDL and dependency ordering across remaining inheritance,
  partition, extension, storage, index, sequence, policy, ownership/grant shapes.
- [ ] Extend rich-schema migrations one proven shape at a time, with inverse or
  explicit irreversibility and real-engine restricted-role round trips.
- [ ] Complete broader plan, process-control, bulk-import and recovery desktop
  workflows beyond the bounded actions already implemented.
- [ ] Expand certificate, larger-fixture and supported-version acceptance.

Details: [provider gaps](PLANS/postgres-provider-gaps.md),
[feature map](PLANS/postgres-feature-map.md).

## SQL Server

- [x] Scoped driver graduation, reviewed identity/computed-column import,
  bounded effective permissions, database-user repair and index maintenance.
- [ ] Design lossless `sql_variant`/UDT values and binds; keep explicit refusals
  until each supported family is proven.
- [ ] Add native round trips and migrations for remaining temporal, memory,
  replication/policy, storage, nonordinary-index, constraint and type shapes.
- [ ] Complete broader plan and administration workflows, with restricted-user,
  certificate, large-fixture and supported-version acceptance.

Details: [provider gaps](PLANS/sqlserver-provider-gaps.md),
[index maintenance](PLANS/sqlserver-index-maintenance.md).

## SQLite

- [x] Scoped provider graduation; proven table/column renames, ordinary column
  removal, nullable additions, native binds, keyed edits and connected-main restore.
- [ ] Complete dependency coverage and dependency-bearing/general alter/rebuild
  migrations and designer controls with native preservation/rollback evidence.
- [ ] Define safe identity/write rules for additional editable table shapes.
- [ ] Expand native streaming transfers, cross-root recovery/delete, physical
  storage-error and external-writer recovery acceptance.
- [ ] Decide safe scopes for ATTACH, extensions, custom functions/collations and
  virtual-table writes; retain current refusal until an accepted design exists.
- [ ] Expand real-file mixed-storage, large-schema/result and platform fixtures.

Details: [provider gaps](PLANS/sqlite-provider-gaps.md), [support contract](SQLITE.md).

## Imports and transfers

- [x] CSV/XLSX/typed Parquet projection, text/decimal expressions, direct Vim
  mapping controls, quarantine history, durable CSV resume and scheduling.
- [ ] Add broader formats, larger bounded sources and remaining native conversion
  shapes with sample privacy, cancellation, source binding and real-engine tests.

Details: [mapping](PLANS/non-csv-mapping-review.md),
[provider operations](PLANS/provider-and-operations-completion.md).

## Recovery and maintenance

- [x] State backup/restore, scheduling/retention, remote destinations, same-ID
  tenant recovery and bounded native database backup/restore/maintenance.
- [ ] Add cross-instance tenant identity migration and external disaster-recovery
  orchestration; complete a unified provider-aware recovery UI.
- [ ] Add advanced partition/storage maintenance and SQL Server online/resumable
  index operations under separate reviewed native contracts.
- [ ] Complete provider-specific recovery scope and compatibility validation;
  cluster/PITR recovery is outside the existing PostgreSQL dump/restore slice.

Details: [backup/restore](PLANS/state-backup-restore.md),
[provider completion](PLANS/provider-and-operations-completion.md).

## Desktop quality and architecture

- [x] Core SQL editor/navigation/results/workspace/Git/automation workflows exist.
- [x] Provider-format schema-cache normalization and hosted TLS WebSocket result
  delivery are fixed. The real Rust SDK returned 10 `dashboard_events` rows;
  the desktop package was rebuilt and installed at `e3d6b13e`.
- [ ] Split the remaining large workspace shell by feature state/event ownership
  and continue bounded command-worker separation.
- [ ] Complete keyboard-equivalence and accessibility audits for every primary
  flow and context-local action, including external Wiki/License links.
- [ ] Meet measured typing, scroll, first-result and desktop memory budgets on
  representative large documents, schemas, results, diagrams, histories and logs.
- [ ] Finish native crash/restart/offline/auth-expiry/unknown-outcome recovery.
- [ ] Complete filter type metadata/type-specific inputs, in-panel SQL/context
  review, asynchronous large-result Apply and keyboard reachability.
- [ ] Complete diagram dense/large-schema navigation and accessible list parity;
  review optional additional layouts, filters, export formats and presets.

Details: [desktop](PLANS/gpui-desktop.md), [refactors](PLANS/product-improvements.md),
[keyboard](PLANS/keyboard-first-desktop.md), [filters](PLANS/result-filter-builder-ux.md),
[diagram](PLANS/relational-table-viewer-and-erd.md).

## Reproducible instances

- [x] Two-file config/lock foundation, validation, offline plan/apply, destination
  identity/generations and desktop multi-instance lifecycle exist.
- [ ] Complete claim/first-up automation, established-instance step-up and
  deployment authority, generation management and typed credential workflows.
- [ ] Finish complete locked artifact closure/offline vendoring, portable encrypted
  bundles and the documented crash/security/deployment acceptance matrix.

Details: [instance contract and phase plan](PLANS/reproducible-instances.md).

## Platform distribution and native acceptance

- [x] Native amd64 development Debian producer, Windows development packaging
  tooling/historical receipts, and offline artifact-byte verification exist.
- [ ] Produce and accept optimized production release artifacts, native publisher
  trust, signing, installers and clean-machine prerequisites.
- [ ] Complete Linux accelerated/Wayland interaction, IME, clipboard, scaling,
  keyboard layouts and broader architecture acceptance. Xvfb/lavapipe evidence
  covers only the software-rendered slice actually tested.
- [ ] Finish macOS distribution and current Windows/macOS native installer,
  graphics, isolation and update acceptance when those hosts are available.

Details: [artifact matrix](DESKTOP-ARTIFACT-VALIDATION.md),
[Linux distribution](PLANS/linux-debian-distribution.md),
[Windows distribution](PLANS/windows-native-distribution.md).

## Blocked and deferred validation

- [ ] Re-run OpenCode's captured-transport native gate after the previously
  recorded HTTP 429/capacity blocker is resolved. Historical success does not
  replace that outstanding rerun.
- [ ] Native Windows/macOS acceptance is explicitly deferred by the user until
  those hosts are available. Keep historical receipts scoped to their source/artifact.
- [ ] Perform certificate, native-engine, restricted-role and representative
  large-fixture checks for each new provider shape; passing workspace tests do
  not certify untested database versions or deployments.

## Completed baseline and continuation rules

- [x] Local Sift (Bedrock) launcher naming, stable icon and duplicate-window
  entry cleanup verified. This is device configuration, not portable packaging.

- [x] `e3d6b13e`: explicit per-connection TLS crypto provider for hosted
  WebSockets, preserving certificate verification; regression and live row
  delivery verified, packaged desktop startup accepted.
- [x] Formatting, strict workspace/all-target Clippy and the full Linux workspace
  suite with `sift-server/live-mssql` passed at that code baseline:
  **1,988 passed, 0 failed, 32 ignored**.
  This documentation reconciliation does not claim new native/platform evidence.
- Keep Rust/Cargo at the existing aggregate **10% CPU/RAM cap**, one Cargo job,
  and manual-dispatch-only CI. Support Vim only.
- Choose one unchecked bounded slice, read its detailed contract and relevant
  ADRs, implement it with audited/source-bound behavior, validate appropriate
  real behavior, and commit the verified milestone.
- Update this list, the feature inventory and the affected detailed plan together.
  Delete superseded pending notes; preserve current limitations, architecture,
  native evidence boundaries and third-party attribution.
