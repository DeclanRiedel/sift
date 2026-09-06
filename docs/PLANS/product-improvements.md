# Product refactors and improvements

Status: active, requested 2026-09-06. Commit completed milestones with validation.
The canonical feature inventory remains the source of product feature status.

## Milestones

- [x] Reconcile the historical desktop API checklist with canonical feature status.
- [x] Bound active metadata connections and extract pool ownership.
- [ ] Split workspace shell by feature state and event ownership.
- [x] Split HTTP handlers by domain while preserving authorization and audit.
- [x] Separate desktop semantic/query worker domains and task lifetimes; broader command dispatch remains incremental.
- [x] Consolidate supported interaction paths around Vim.
- [x] Extract metadata pool and SDK vault/automation/transfer APIs without interface changes; remaining domains are incremental.
- [ ] Measure responsiveness and memory with existing large fixtures.
- [ ] Complete crash/restart/offline/auth-expiry recovery validation.
- [x] Verify existing inspection UI and fix independent/nested join findings.
- [ ] Complete foreign-key JOIN assistance, then explicit multi-hop path selection.
- [x] Harden existing saved layouts and verify bounded foreign-key value selection.
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

## Design: domain extraction

Keep HTTP routing/middleware in `http.rs`, moving domain handlers together while
retaining existing session services and authorization helpers. Route names,
operation IDs, auditing, and query isolation remain unchanged. SDK domain modules
continue implementing the same `Client`; this is a source organization change.
Desktop semantic jobs and streamed queries get separate task-owner modules;
semantic revision state stays private to its worker. Existing contract and
stale-work tests validate the moves.

Domain extraction: workspace Clippy and full workspace tests passed, including
doc tests. HTTP execution, semantic, vault, repository, and automation handlers
now live in domain modules. SDK vault/automation/transfer methods preserve their
`Client` interface. Desktop semantic state and its six worker tests moved with
the worker; streamed-query recovery lives separately. Workspace query history
owns its state and event handling in one module. Broader shell decomposition
remains open rather than claiming the entire shell has been redesigned.

## Design: Vim-only interaction

Vim is the sole supported settings/editor enum variant. New editors construct
their Vim engine immediately and begin in normal mode. Remove mode/profile
switches from settings, keymaps, and status chrome; keep editable leader bindings.
Remove tests of unsupported profiles while preserving Vim behavior and settings
persistence tests. Unsupported settings values report the existing decode error;
no second interaction implementation is retained.

SQL safety: existing server diagnostics already reach the editor. Broad UPDATE
and DELETE statements now also receive join findings; nested FROM relations are
walked consistently and output stays within the diagnostic cap. Expanded existing
regression passed. Workspace Clippy passed for the semantic change.

## Design: sequence DDL

Generate sequence definitions through existing Driver execution and server-owned
DDL isolation. Read configured type, start, increment, bounds, cycle, and cache
from engine catalogs. Do not advance the sequence or export its runtime counter.
Use existing object-kind dispatch, so the Driver trait and wire shape do not
change. Catalog scalar reads must reject absent/NULL/non-text definitions rather
than returning empty or debug-formatted SQL. PostgreSQL fixtures exercise both
ascending and descending sequences with non-default options.

Vim/grid milestone: formatting, workspace Clippy, and full workspace tests passed.
Editors initialize Vim directly; unsupported profile controls/tests are removed.
Explicit text replacements and undo/redo now resynchronize the Vim engine.
Grid layout keys include database object identity and a framed column signature;
ordinal column keys preserve duplicate aliases, and repeated persisted positions
are deduplicated. Existing foreign-key picker resolves catalog-proven references,
uses bounded data search, and stages edits through the established edit preview.
Old unscoped grid keys are not applied to new object-scoped layouts.

DDL milestone: sequence generation added for PostgreSQL and SQL Server. Scalar
catalog reads reject NULL/non-text results and missing terminal pages. Four DDL
unit tests passed. A PostgreSQL live fixture was added; local `.env` and the
existing fixture's default socket are absent, so live execution is unverified.
Trigger/type/generated-column/collation fidelity and SQL Server live round trips
remain open. Sources used for catalog field semantics:
[PostgreSQL pg_sequence](https://www.postgresql.org/docs/current/catalog-pg-sequence.html)
and [SQL Server sys.sequences](https://learn.microsoft.com/en-us/sql/relational-databases/system-catalog-views/sys-sequences-transact-sql).
