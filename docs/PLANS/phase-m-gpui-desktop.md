# Phase M — GPUI Desktop Client

Status: **design locked on 2026-08-11; M0–M2 complete, M3 in progress.** ADR-040 is
normative. Every milestone below ends in a separately reviewable commit with
the workspace quality gates green.

Milestones: M0 client boundary and GPUI feasibility, M1 native application
shell, M2 connection and workspace lifecycle, M3 daily-driver SQL vertical
slice, M4 database navigation and editing, M5 advanced product surfaces, and
M6 product polish and graduation.

## Outcome

Phase M delivers the first-party Sift desktop client as a thin, native GPUI
application. The interaction standard is Zed: fast first paint, dense but
quiet chrome, keyboard-first actions, contextual focus, panes and docks,
restore-before-I/O, and background work that cannot stall rendering. The
database workflow combines DataGrip's semantic depth with Navicat's
discoverability without moving product behavior out of `sift-server`.

The first shippable checkpoint is M3. A user can launch Sift, open a local or
remote virtual workspace, connect, edit a collaborative SQL document, use
server-backed SQL intelligence, execute or cancel it, and inspect a large
streamed result without blocking the UI.

## Locked product selections

- Linux is the primary development platform. Linux, macOS, and Windows are
  architectural targets from the first implementation commit.
- The desktop supervises a separate local server process and communicates with
  local and remote servers exclusively through the public HTTP/WebSocket API.
- One window presents one Sift virtual workspace. A workspace can contain many
  database connections and split panes; multiple windows may open the same or
  different workspaces.
- A query item owns its `Data`, `Messages`, `Explain`, and `History` result
  tabs. A result can be pinned or promoted to an independent pane item.
- The initial editor ships a configurable conventional keymap. Vim emulation
  is a follow-up, not an M3 graduation gate.
- Sift uses an original component library, theme, icon set, and brand. Zed is
  the architectural and interaction reference; its application crates and
  assets are not dependencies.
- Dark and light themes are first-class from M1. Theme values are semantic
  tokens rather than colors embedded in feature views.
- Ordinary workflows are mouse discoverable. The command palette and keyboard
  paths are accelerators, not the only way to find an operation.

## Existing foundations

Phase M renders existing server-owned behavior rather than reimplementing it:

- ADR-001/002/003: server authority, UI-free shared crates, and pure-serde
  protocol remain non-negotiable.
- ADR-006/028: every user-visible server operation is typed, audited, and has
  server-derived contextual availability.
- ADR-007/014: rooms own collaboration and only SQL text uses Loro.
- ADR-011: results are bounded, paged cursor streams with explicit resume.
- ADR-015/021: signed restart activation and local/SSH/network lifecycle modes
  already exist.
- ADR-032/033: SQL semantics and database models are shared server services.
- ADR-034: room-owned virtual workspaces, optional projections, Git, runs,
  schedules, and transfer recipes are public API surfaces.
- Phase I client contributions are declarative commands, actions, forms,
  tables, and read-only panels. They do not load arbitrary UI code.

The current `sift-client-sdk` publicly re-exports some HTTP DTOs from
`sift-metadata`. M0 removes that server-internal dependency before the desktop
uses the SDK. Public wire types belong in `sift-protocol` or a deliberately
public API contract, never in metadata.

## Zed architecture adopted as design input

The design was checked against Zed commit
[`d71f1461`](https://github.com/zed-industries/zed/tree/d71f1461045c098dc6ca6b1b5adcf1b8949722e8)
(2026-08-11). Sift adopts these patterns:

- GPUI entities own application and view state on the UI thread;
- parent entities own child entities and receive narrow emitted events instead
  of children reaching across the tree;
- typed local actions drive keymaps, menus, buttons, and the command palette;
- focus contexts form a hierarchy so the most specific active item handles an
  action first;
- async work holds weak entity references and applies results back through a
  GPUI context only while the owner still exists;
- saved layout is restored before network or schema work begins;
- workspace, panes, docks, and items are composable rather than encoded as one
  monolithic screen; and
- custom GPUI elements are used when a large editor, grid, tree, or diagram
  needs tighter layout and paint control than ordinary views provide.

Sift deliberately does not depend on Zed's GPL application crates. GPUI is
Apache-2.0, while Zed's `ui`, `editor`, and related application crates are
GPL-3.0-or-later and encode Zed-specific project, worktree, LSP, Git, RPC,
settings, and buffer assumptions. Sift pins an exact GPUI revision and builds
its own small application-specific component system.

## Dependency and crate boundary

Phase M starts with three crates and splits only under measured pressure:

```text
sift-desktop (binary and composition root)
    |-- sift-ui (GPUI components, theme, icons, accessibility)
    |-- sift-workspace (entities, panes, items, actions, persistence)
    |-- sift-client-sdk
            |-- sift-protocol
            `-- sift-doc (query-text replica only)
```

- `sift-desktop` may compose process supervision and the client runtime, but
  feature views do not import `sift-server`.
- `sift-ui` depends on GPUI and presentation-only libraries. It has no SDK,
  protocol, database, filesystem-workspace, or server dependency.
- `sift-workspace` maps public SDK/protocol models into UI-owned projections.
  It never exposes GPUI types back into shared crates.
- Feature modules begin inside `sift-workspace`. `sift-editor`,
  `sift-results`, or other crates are created only when they have a stable,
  independently testable API.
- Application regions are modules and entities inside `sift-workspace-ui`, not
  one crate per app bar, dock, or status surface. Split crates only when a
  feature owns a stable API and independent test/performance boundary.
- GPUI is pinned to an exact Git revision because it remains pre-1.0. Upgrades
  are intentional commits that run the full client test and performance suite.

CI enforces the dependency firewall. A desktop/UI crate depending directly or
transitively on `sift-server`, a driver, or `sift-metadata` is a failure, except
for an explicitly separated composition-only local-server entry point if the
single-distribution launcher later requires it.

## Host-owned UI extensibility

Desktop extensibility supports first-party evolution, not extension-rendered
UI. Three typed registries define compile-time surfaces:

- `CommandRegistry` owns stable command ids, labels, shortcuts, palette
  visibility, and contextual availability shared by menus and the palette;
- `DockRegistry` owns stable dock ids, titles, and placements; and
- `ItemRegistry` owns persisted item-kind to runtime-view metadata.

Display strings never select behavior. `WorkspaceShell` coordinates state and
dispatch, while focused modules render status and output chrome and dedicated
entities own editors, results, and panes. New built-in surfaces enter through
these typed registries and exhaustive dispatch.

Extensions cannot register GPUI entities, commands, panels, item factories,
styles, or layout slots. They continue to contribute server-side providers and
governed operations through typed, audited Phase I contracts. Public
declarative client-contribution descriptors remain compatible for independent
thin clients; the first-party desktop does not consume them as UI mutation.
This boundary keeps theme, accessibility, focus, restoration, and crash
behavior under client ownership.

## Entity and ownership model

```text
SiftApp
 |-- ClientStateStore
 |-- ServerRegistry
 |-- ThemeRegistry
 |-- ActionRegistry
 `-- Window...
      `-- Workspace
           |-- ConnectionModel...
           |-- RoomReplica...
           |-- Dock(left | bottom | right)...
           `-- Pane...
                `-- Item...
                     |-- QueryItem
                     |    |-- QueryDocument
                     |    `-- ResultSet...
                     |-- SchemaItem
                     |-- ModelItem
                     |-- MigrationItem
                     `-- RunItem
```

`SiftApp` contains process-wide presentation services, not authoritative
product state. `Workspace` owns the UI projection of one server workspace and
its room/connection attachments. A `Pane` owns ordered items and focus. Items
emit events such as title changes, dirty presentation state, close requests,
and promotion requests to their pane or workspace owner.

Network responses are mapped into immutable or revisioned UI projections.
Views render the last complete projection while a refresh is pending; they do
not clear useful data merely because the network is slow. Optimistic UI is
allowed only for reversible presentation state. Server mutations render a
pending state and reconcile against the authoritative response or event.

## Actions, commands, and capability checks

GPUI actions and Sift protocol operations are different layers:

- a local action such as `workspace::SplitPane` changes presentation state;
- a local action such as `query::ExecuteSelection` resolves the focused query,
  requests current server capability, and dispatches an audited protocol
  operation through the SDK; and
- server capability discovery remains advisory. Dispatch rechecks authority,
  policy, revisions, and resource state.

Every user-facing action has one stable identity used by keybindings, menus,
buttons, telemetry names, tests, and the command palette. Availability has a
disabled reason suitable for a tooltip or palette row. Destructive and
database-writing actions show consequence, target, and approval state before
dispatch.

The initial focus-context hierarchy is:

```text
Application > Window > Workspace > Dock|Pane > Item > Control
```

More specific contexts win. Text input consumes editing commands without
preventing workspace commands that are intentionally global.

## Visual and interaction system

The default composition is:

```text
+---------------- integrated title bar / workspace switcher ---------------+
| Connections | Query / DDL / Model editor panes          | Inspector       |
| Workspaces  |                                            | Properties      |
| Schema tree |                                            | Dependencies    |
| Git / Runs  +--------------------------------------------+ Changes         |
|             | Data / Messages / Explain / History        |                 |
+-------------+--------------------------------------------+-----------------+
| Connections Git Collab Outline | Search Problems Error                    |
| target - transaction - execution       Mode | Console Monitor Automations |
+---------------------------------------------------------------------------+
```

The shell is recognizably Zed-like: restrained borders, compact controls,
clear focus, integrated tabs, contextual actions, and no permanent ribbon.
Database density comes from resizable/collapsible docks, tree filtering,
column-aware grids, inspector sections, and progressive detail—not from tiny
hit targets or unlabeled icon walls.

The footer is host-owned SQL chrome. Its left selectors share one left dock:
Connections, capability-gated Git, Collaboration, and Query Outline. Its
middle actions expose project search, diagnostics, a copyable current error,
the active target, transaction state, and execution state. Its right controls
hold the editor keymap plus one bottom tool at a time: Console, Monitor, or
Automations. Selecting the active panel closes that dock; selecting another
panel swaps it in place. Debugger and generic agent controls are deliberately
absent because they do not describe Sift's database workflow.
Footer actions use compact application-owned icons, accessible names, and
hover tooltips; live target, transaction, and execution state remains textual
because those values must be directly scannable.

Component completion requires all of:

- rest, hover, active, focus-visible, selected, disabled, loading, error, and
  destructive states where applicable;
- keyboard navigation and meaningful accessibility roles/labels;
- light and dark semantic-token coverage;
- scale-factor and text-zoom behavior;
- no layout dependence on English string length; and
- deterministic tests for action/focus behavior.

## Local server and lifecycle topology

The desktop launches or attaches to a separately supervised local server. It
discovers readiness through the existing health/version handshake and then
uses the same HTTP/WebSocket client path as SSH or network-hosted instances.
The UI never calls server stores, drivers, or session internals directly.

Local startup is staged:

1. load client presentation state and paint the window;
2. restore workspace, pane, item, and scroll placeholders;
3. start or attach to the local server without blocking the UI thread;
4. negotiate protocol/auth and reconnect room/session replicas;
5. hydrate visible items first, then docks and background metadata; and
6. surface degraded capabilities without hiding the restored workspace.

The local server may outlive a window during normal multi-window use. The
supervisor owns one lease/reference count per desktop process and requests
graceful shutdown only when the last local window releases it and policy says
the daemon should not remain running.

## Client-local persistence

The client has one OS-account-local state store for presentation only. It is
owned by the desktop installation, not a Sift server, tenant, room, or remote
principal, and is never synchronized between collaborators. Two users opening
the same server workspace from different desktops retain independent UI
composition.

The store contains:

- window bounds and display identity with off-screen recovery;
- workspace/window recents;
- pane and dock layout;
- active left panel and bottom tool;
- open item references and selected result tabs;
- column widths, sort presentation, scroll anchors, and expansion state;
- theme, keymap, text zoom, and accessibility preferences; and
- non-secret local server discovery metadata.

It does not persist connection secret bytes, authoritative query history,
schema/catalog truth, server workspace contents, results, permissions, or
operation outcomes. Secret values remain in the existing `SecretStore`
boundary. Restored references can be stale and must degrade to explicit
missing/unauthorized states rather than being silently discarded.

## Platform policy

GPUI abstracts rendering and most window/input behavior, but Phase M treats
cross-platform support as a tested boundary rather than an assumption:

- the `sift-desktop` platform module owns native menus, window decoration
  integration, semantic modifier labels, file dialogs, credential integration,
  notification hooks, reveal/open actions, and packaging paths;
- product code uses semantic `primary`, `secondary`, and `alternate`
  modifiers instead of spelling Command or Control;
- paths remain typed paths and are never parsed by string separators;
- font fallback, shaping, scale factors, keyboard layouts, IME, clipboard, and
  accessibility behavior receive platform fixtures or acceptance checks;
- no feature launches a shell to open a URL, file, or folder; and
- unsupported native facilities produce a capability state, not scattered
  compile-time branches in views.

Linux is required at every milestone. Cross-compilation or native CI for
macOS and Windows is introduced in M1 and remains green before M6 graduation.
Platform-specific visual acceptance runs are evidence, even when they require
dedicated runners outside the primary Linux development environment.

## Performance model

M0 records reproducible baselines on the reference Linux environment before
hard numerical budgets are graduated. The following behavioral budgets are
locked immediately:

- first paint never waits for server, schema, Git, semantic, or result I/O;
- no HTTP/WebSocket, SQLite, filesystem, serialization-heavy, or semantic work
  runs synchronously in a GPUI render/action handler;
- grid and tree layout/paint work is proportional to the visible window plus a
  small overscan, not total row/object count;
- typing does not await completion, diagnostics, collaboration, or persistence;
- superseded semantic, filter, preview, and page requests are cancellable;
- background refresh keeps the last valid projection visible; and
- result memory is bounded by the SDK/server page window plus explicit pinned
  pages, never by total result cardinality.

M6 graduates measured p50/p95 budgets for cold first paint, restored first
paint, action-to-paint, editor input, 100k-row grid scrolling, 100k-object tree
filtering, reconnect, and idle/active memory. Regressions beyond the recorded
tolerance fail the relevant performance gate.

## Error, recovery, and trust model

- Loading, empty, stale, offline, unauthorized, unsupported, conflict,
  cancelled, timed-out, outcome-unknown, and failed are distinct UI states.
- Transport loss never converts an indeterminate write into success or retries
  it automatically.
- Reconnect resumes room/document state through existing replica contracts and
  result cursors through their explicit resume contract.
- A crashed local server degrades the workspace and offers a bounded restart;
  it does not crash the GPUI process.
- A crashed desktop restores presentation references and reconciles them with
  server state. It does not reconstruct authoritative state from a client
  snapshot.
- Public extension client descriptors remain wire-compatible for independent
  thin clients. The first-party GPUI desktop declines them and does not allow
  extensions to add commands, panels, views, styles, or layout.
- Values, credentials, SQL bodies, and result cells are excluded from logs,
  action labels, analytics, crash context, and persisted presentation state.

## Milestones

### M0 — client boundary and GPUI feasibility

- [x] Move public SDK HTTP DTOs out of `sift-metadata`; enforce the client
      dependency firewall.
- [x] Add the three client crates and an exact GPUI revision.
- [x] Open a native window with semantic theme tokens and one root entity.
- [x] Prove background-to-UI entity updates, cancellation on entity drop,
      action dispatch, focus contexts, text/IME input, clipboard, and one
      custom virtual-list element.
- [x] Establish Linux build/test prerequisites without manual shell state.
- [x] Record initial startup, input, and virtual-list measurements.
- [x] Add `#[gpui::test]` coverage and a headless-safe test strategy.

M0 is a feasibility gate. If GPUI cannot meet text input, accessibility,
virtualization, or automated-test needs, implementation stops for an explicit
ADR amendment instead of burying a second UI toolkit behind an abstraction.

### M1 — native application shell

- [x] Implement `SiftApp`, window, workspace, pane, item, dock, modal, toast,
      tooltip, and status-bar ownership.
- [x] Build the initial Sift component set and light/dark themes.
- [x] Implement typed actions, key contexts, command palette, menus, and
      capability-aware disabled reasons.
- [x] Persist and restore window/pane/dock/item presentation state before I/O.
- [x] Add the platform boundary and Linux/macOS/Windows compile lanes.
- [x] Test focus transfer, pane splitting, close/save prompts, command routing,
      scale factors, and off-screen window recovery.

### M2 — connection and workspace lifecycle

- [x] Supervise/attach to a local server and reuse the same SDK path for local,
      SSH, and hosted instances.
- [x] Implement instance selection, authentication, reconnect, version
      negotiation, and capability/degraded states.
- [x] Browse/open/create virtual workspaces and restore one workspace per
      window.
- [x] Render connection, workspace, Git, and run navigation docks progressively.
- [x] Join rooms, project presence, and expose follow controls without making
      presence durable.
- [x] Exercise server restart, expired auth, revoked membership, offline start,
      stale restored references, and multi-window local-server leases.

### M3 — daily-driver SQL vertical slice

- [x] Implement the SQL query item, selections, statement targeting, undo/redo,
      clipboard, find, and server-backed Loro document replica. **Landed:**
      `QueryDocument` (Loro `TextReplica`-backed buffer, byte-offset selections
      with sticky goal column, edit + undo/redo, quote/comment-aware statement
      targeting, case-insensitive find) and the multi-line `QueryEditor` view
      (custom element, `EntityInputHandler` text/IME, `SiftEditor` keymap),
      hosted per query item in each pane. Room-owned tabs now persist only
      stable instance/room/document references, restore from server snapshots,
      emit native Loro updates to a reconnecting SDK `RoomReplica`, apply peer
      commits, and become clean only after durable ACK. Room navigation can
      create and open these documents without storing SQL client-side.
- [x] Integrate completion, diagnostics, formatting, quick fixes, usages, and
      semantic revision cancellation. **Landed:** `workspace-ui/src/editor/semantic.rs`
      projects the server semantic document onto client byte offsets; the editor
      owns a completion menu, severity-coloured diagnostic underlines, usage
      highlights, a caret-diagnostic status strip, and typed actions
      (`Complete`, `FormatDocument`, `ApplyQuickFix`, `FindUsages`,
      `GoToNextDiagnostic`) surfaced through the keymap, Query menu, and command
      palette. The shell debounces keystroke-driven `Analyze` and dispatches
      interactive requests immediately; `desktop/src/app.rs` runs a per-connection
      semantic service task that resynchronizes the server document from the text
      each job carries, so requests need no ordering protocol. Revision
      cancellation is enforced twice — superseded jobs are discarded before they
      cost a round trip, and answers whose revision no longer matches the buffer
      are dropped rather than applied late. Catalog-bound diagnostics degrade to
      syntax-only rather than failing.
- [x] Execute selection/document, stream status, cancel, and distinguish
      rejected, failed, cancelled, timed-out, and outcome-unknown operations.
      **Landed:** editor `ExecuteStatement`/`ExecuteDocument` actions
      (Ctrl/Cmd+Enter) emit `EditorEvent::Execute`; the pane raises
      `PaneEvent::ExecuteRequested` and shows Pending; the workspace forwards it
      over an `ExecuteCommand` channel to a background executor in `sift-desktop`
      that owns the SDK client, opens a session + connection-from-profile, and
      uses a cursor-backed session WebSocket. Each page reaches the UI before
      its ACK, bounding in-flight work to one page; query tasks remain
      independently cancellable through the same cursor socket. Evicted cursors
      resume through typed spill batches, transport loss remains an explicit
      outcome-unknown state, and all terminal responses map to distinct
      Ready/Unavailable/Failed/Cancelled/TimedOut/OutcomeUnknown states.
- [~] Implement virtualized results with typed cells, null/error states, column
      resizing/reordering, selection, copy, paging, resume, and bounded prefetch.
      **Landed:** `uniform_list`-virtualized grid, typed cell rendering
      (null/number/bool/text/temporal/binary/structured), header type badges,
      cell selection + copy, independently resizable columns, horizontal scroll,
      incremental WebSocket page application, one-page ACK backpressure, spill
      resume, and a 10,000-row retained-grid cap. **Remaining:** column reorder
      and explicit navigation for rows beyond the retained window.
- [x] Add query-owned Data/Messages/Explain/History tabs plus pin/promote.
- [ ] Restore query and result references without persisting result data.
- [ ] Meet measured typing, first-result, scroll, and memory budgets on large
      fixtures.

### M4 — database navigation and editing

- [ ] Implement lazy schema navigation, filtering, refresh/invalidation, object
      details, DDL, dependencies, usages, and data browsing.
- [ ] Render transaction state, savepoints, process control, execution plans,
      schema/data search, saved queries, and query history.
- [ ] Implement typed edit staging, preview, optimistic conflict display,
      approval, apply, and deterministic reconciliation.
- [ ] Ensure read-only/capability-limited connections remain useful and explain
      every disabled mutation.

### M5 — advanced product surfaces

- [ ] Render catalog diagrams, comparisons, diffs, migration plans, safety
      findings, previews, and apply results.
- [ ] Implement virtual workspace history, projection reconciliation, Git
      status/diff/stage/commit/fetch/push, and conflict resolution.
- [ ] Implement run configurations, live runs, logs, schedules, and recovery.
- [ ] Implement import/export and transfer recipes with progress, cancellation,
      artifact handling, and bounded previews.
- [ ] Render Phase I declarative contributions through trusted actions, forms,
      tables, and read-only panels.

### M6 — product polish and graduation

- [ ] Complete keyboard-only and accessibility audits for all primary flows.
- [ ] Complete crash/restart/offline/auth-expiry/outcome-unknown recovery
      matrices.
- [ ] Graduate measured performance and memory budgets on representative large
      schemas, documents, results, diagrams, histories, and logs.
- [ ] Validate dark/light themes, scaling, IME, keyboard layouts, clipboard,
      dialogs, window chrome, packaging, and updates on Linux/macOS/Windows.
- [ ] Produce signed desktop artifacts using the existing release/update
      lifecycle without weakening server verification.
- [ ] Publish the Phase M graduation matrix and update product status docs.

## Commit and quality policy

The normative plan/ADR is committed before M0 code. Each milestone then ends in
its own named commit only after:

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Client-specific GPUI tests, dependency-firewall checks, performance fixtures,
and applicable platform compile/acceptance gates join this baseline as they
land. A milestone is not marked complete because its happy path renders; its
failure, focus, restoration, accessibility, and cancellation behavior must be
covered at the level specified above.

## Non-goals

- A web client or a generic UI toolkit abstraction over GPUI.
- Reimplementing server policy, semantic analysis, workspace authority,
  scheduling, Git orchestration, or transfer behavior in the desktop.
- Loading arbitrary extension JavaScript, native UI libraries, or GPUI code.
- Copying Zed source, assets, branding, or its local-worktree ownership model.
- Treating grids, schema, catalog models, sessions, presence, Git, or results as
  CRDT documents.
- Persisting result data, secret values, or authoritative database state in the
  client state store.
- Full Vim emulation, forge/code-review workflows, or a mobile/tablet client in
  Phase M.

## Graduation definition

Phase M graduates only when the first-party desktop can reach every selected
v1 server capability through the public SDK, while a third-party client could
still do the same without GPUI or private server access. Linux, macOS, and
Windows artifacts must share the same workspace/action model and public API;
platform differences are confined to the declared native boundary. The final
evidence matrix records feature reachability, protocol/SDK parity, focus and
accessibility behavior, recovery scenarios, dependency boundaries, native
platform checks, and measured performance budgets.
