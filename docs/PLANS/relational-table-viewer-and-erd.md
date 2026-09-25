# Relational table viewer and ER diagram

Status: **implementation underway, 2026-09-25.** The command-opened viewer,
Objects-style toolbar, scoped catalog requests, table picker, FK diagram and
details, zoom, fit-width, and Mermaid copy are implemented. Remaining design
items below are candidates for later passes; this document retains the complete
feature map.

Scope: a command-opened Sift workspace viewer for database tables and their
relationships. Selecting or opening a table does not open this viewer by
default. The existing Objects and object tabs keep their current entry behavior.
“ER diagram” here means a navigable view of actual catalog relationships, not a
data model that invents relationships from matching names. This plan builds on
ADR-033 and the existing table definition inspector and catalog diagram API.

## Product jobs

1. Invoke the viewer for a selected table and answer: what are its columns,
   keys, indexes, constraints, and nearby tables?
2. Trace a foreign key in either direction, including its ordered column pairs,
   without losing the original table.
3. Explore a small neighborhood visually, then expand it deliberately. Keep a
   large database usable without rendering the whole graph at once.
4. Move from structure to data, DDL, usages, comparison, or the existing design
   workflow with a clear sense of which database and revision are in view.

Success means a user can find a table, identify its primary key and all outgoing
and incoming foreign keys, inspect a composite key, and open a related table
using only the keyboard or pointer. Partial metadata must remain visibly partial.

## Existing foundation and gaps

- The Connections dock and schema search open database object tabs. These tabs
  already have a relation definition inspector with Columns, Indexes, Relations,
  Triggers, and Dependencies, plus data preview and design actions.
- `CatalogGraph` supplies stable opaque object ids, column metadata, constraint
  details, foreign-key edges with ordered column pairs, certainty, revision, and
  coverage. `CatalogDiagramRequest` projects a bounded neighborhood.
- The Objects tab uses a compact two-row toolbar inside its workspace item,
  with connection/catalog/schema pickers, filters, and contextual actions.
  This is the visual and interaction pattern for the new viewer toolbar; it is
  distinct from the window's global app bar.
- The current Catalog Diagram modal renders up to 100 object cards with text
  relationships and can copy Mermaid or open comparison. It does not position
  tables or draw inspectable FK connections.
- Current Relations lists outgoing FK constraints. Dependencies mixes other
  edges with incoming/outgoing references. The column list gives little room
  for types and key roles. The diagram has no focused-table journey.

Preserve those working API and design paths while improving their presentation.
Do not infer server truth in UI code, parse opaque ids, or store graph data in
client presentation state.

## Information architecture

### Entry and workspace item

Add **Open Relationship Viewer for Table** to the selected table's context
menu, object tab, Objects row actions, and command palette. One command receives
the selected table identity and opens or focuses a dedicated viewer tab in the
current pane. The table is its initial scope, with one FK hop and a bounded node
budget. No automatic opening on selection, search, or table tab creation.

The existing database-wide Catalog Diagram command may open the same viewer
with an empty scope and prompt for a schema/table. Existing table data, DDL,
design, and results stay in their existing tabs. The viewer links to those tabs
through explicit actions.

### Viewer toolbar, following Objects

Use the same inset, height, theme tokens, picker style, focus treatment, and
overflow behavior as the Objects tab's two-row toolbar. First row establishes
context: **Connection › Catalog › Schema › Anchor table**, searchable scope
picker, and coverage/revision status. Second row controls the projection:
**References / Referenced by**, hop depth, object-kind/edge filters, search,
node budget, **Fit**, **Refresh**, **Copy Mermaid**, and an overflow menu for
DDL, data, compare, and other contextual actions. Keep primary scope and
refresh controls visible at narrow widths; overflow secondary actions. Changing
connection clears incompatible scope and selection rather than showing old
graph data under a new breadcrumb. Explicit Apply may be needed for broad scope
changes to avoid repeated server requests while adjusting controls.

```text
RELATIONSHIPS   Connection ▾  Database ▾  Schema ▾  Anchor: public.orders ▾
Scope: 1 hop ▾  [→ References] [← Referenced by]  Search…  [Fit] [Refresh] [⋯]
┌─ tables / paths ─┐  ┌──────────── canvas ────────────┐  ┌─ details ───┐
│ orders           │  │ customers.id ← orders.customer_id│  │ FK name     │
│ customers        │  │    [customers] ←── [orders]      │  │ column pairs│
│ order_items      │  │                      ↑            │  │ Open / DDL  │
└──────────────────┘  └──────────────────────────────────┘  └─────────────┘
```

### Viewer body

Use a resizable workspace item/pane for exploration. Default body combines a
searchable table list, a central canvas, and a contextual detail panel. The
list and details may collapse on narrow panes. A compact **Table details** view
inside the viewer shows the selected table's columns, keys, constraints,
indexes, triggers, and incoming/outgoing relationships; selecting a different
card changes this inspection target while the scope anchor stays put. Label
anchor and selection separately.

The canvas owns pan, zoom, fit, and reset. Add a minimap only if usability tests
show a need. The companion list exposes the same nodes and edges for keyboard
and assistive access. Relationship rows split **References** and **Referenced
by**, show constraint name, qualified endpoints, ordered column pairs, and
certainty. Selecting one highlights participating columns. Actions include
**Open table**, **Focus graph here**, **Copy JOIN** when catalog-proven and
supported by existing generation, and **DDL** when available.

Column rows show ordinal, name, native/display type, nullability (`Unknown`
stays unknown), and proven PK/FK/unique role. Show available default/generated
facets in expanded detail. If a field is absent from the protocol, omit it or
label it unavailable; do not infer it from DDL text. At narrow widths, stack
attributes and endpoints; keep names copyable and full text accessible.

Each table card shows qualified name and a compact field list: key fields first,
then remaining fields, with name, type, and PK/FK/nullable marks. Expand a card
to show all fields. The edge connects specific field anchors where column pairs
are known. Edge selection shows constraint name, each ordered pair, source and
target, certainty, and available actions in a side inspector. Distinguish
outgoing/incoming by arrow direction and text; do not claim cardinality from an
FK alone. Self references loop visibly. Parallel FKs remain distinct and
selectable. Views and other dependency kinds can be a separate overlay; the
default ER layer shows table-like objects and catalog-proven FKs.

Canvas layout is deterministic for a given visible set, with no moving nodes
after an unrelated refresh. Local drag positions may be saved as presentation
preferences keyed by connection/profile, database identity, object id, and
layout version. Missing or renamed objects drop their pins gracefully. Saved
positions and filters contain no graph truth or row data. Export uses the exact
visible projection and records partial/omitted state in the output. Existing
Mermaid copy remains; image export is a later design choice.

## UI rendering options

| Option | Appearance and use | Performance shape | Assessment |
| --- | --- | --- | --- |
| Master-detail list | Objects-like table list with relationships and fields in a side panel | Simple row virtualization; excellent for huge scopes | Strong accessible fallback; weak spatial understanding |
| GPUI element cards and edge elements | Easy theme reuse and native focus/hover, with flexible card layout | Element count and layout/hit testing grow with visible nodes and edges | Fine for small neighborhoods; risky as the only large-graph renderer |
| Hybrid GPUI scene | Native toolbar/list/details and table cards; custom painted, batched edge layer with viewport culling | Bounded visible elements and paint work; requires geometry, routing, and hit-test model | **Recommended** for first canvas, paired with master-detail list |
| Embedded web graph | Many diagram libraries and visual effects | Second UI runtime, bridge, theme/focus/accessibility duplication | Avoid unless GPUI prototype proves insufficient |

Recommended visual language: Sift's theme tokens and Objects toolbar, restrained
card surfaces, crisp type, aligned column anchors, subdued edges, strong
selection, and readable labels at each zoom level. Use a grid or lane layout
to keep the anchor stable. Emphasize FK direction with arrow and label, not
color alone. At low zoom, collapse field rows to table names and key counts;
zooming in reveals fields. Selected/hovered edges get stronger contrast while
unrelated edges recede. Keep cards stable during pan, filter, and refresh.

Performance design for the hybrid scene:

- Request bounded neighborhoods from `CatalogDiagramRequest`; never ask for or
  lay out an entire large catalog as a default view. Show omitted counts and a
  clear expand action.
- Build an indexed, immutable scene from authorized graph data. Compute layout
  and edge routes away from the UI thread, cancel obsolete work, and swap only
  when the scope/revision token matches.
- Cull cards and edges against the viewport plus a small margin. Keep a spatial
  index for hit testing. Pan/zoom updates transform and visible set; it does not
  rebuild every card or reshape all labels.
- Cache card dimensions and text shaping by node id, graph revision, theme,
  and zoom detail level. Batch edge paths by style; paint highlights separately.
  Reuse the repository's existing GPUI custom-paint and retained-row patterns.
- Keep selected item, toolbar, inspector, and status responsive while layout or
  fetch runs. Measure first open, pan/zoom frame time, peak retained memory,
  and dense-hub expansion before setting production node budgets.

## Interaction and state

- **Invoke → scope → inspect → follow → return:** select a table in Objects,
  explorer, or its object tab; run the viewer command; adjust scope in its
  toolbar; select an FK or table; Open uses normal tab rules; Back returns to
  prior selection. Focus and scroll restore within the viewer.
- **Vim only:** normal mode `j/k` moves rows/list entries, `h/l` changes section
  or graph neighbor where unambiguous, `/` searches within the active view,
  Enter opens/expands, Escape backs out of detail/search before leaving the
  pane. `Tab` reaches toolbar, canvas, list, and inspector. Final bindings must
  be checked against the command registry before implementation.
- **Pointer:** click selects; double click or explicit Open navigates; drag pans
  background or moves a card only when its handle is active; wheel zoom is
  anchored to pointer with bounded range. Tooltips supplement visible labels.
- **Refresh:** retain last good projection while loading. Show source revision,
  capture time, and `complete`, `partial`, or `stale` state. If revision changes,
  reconcile selection by opaque id, then qualified path only as a navigation
  suggestion; never silently treat a rename as proven. An obsolete response
  cannot replace a newer one.
- **Failures and gaps:** distinguish no relationships, unsupported graph
  capability, permission boundary, truncated/partial coverage, stale catalog,
  and request failure. Show omitted node/edge counts where supplied. Offer
  bounded retry/refresh. Shallow/deep schema metadata can still populate basic
  table details when graph capability is missing; graph-dependent controls
  explain why they are unavailable.
- **Safety:** browsing, projection, DDL retrieval, export, and navigation use
  existing audited operations. Design and migration stay in their review and
  authorization flows. A diagram drag only changes layout; it never changes
  schema. Hidden names and edge endpoints stay hidden in text, exports, and
  accessible labels.

## Feature map and priority

| Capability | First release | Later, if useful |
| --- | --- | --- |
| Table profile | Readable selected-table details inside viewer: columns, keys, outgoing/incoming FKs, indexes, constraints, triggers, dependencies | Engine-specific facets and richer column details as protocol supports them |
| Navigation | Explicit command from selected table; scope in viewer; related-table/data/DDL handoff; back/forward | Saved object collections |
| Diagram | Focused one-hop canvas, expand/collapse, pan/zoom/fit, FK pair inspector, list alternative | Multiple layouts, larger custom scopes, overview minimap |
| Filtering | Schema, text, direction, depth, bounded node count | Dependency overlays and custom views |
| Sharing | Copy Mermaid with coverage note; copy qualified names | Image/SVG export, shareable layout presets |
| Editing | Existing explicit table designer and migration preview | Diagram-origin draft FK editing only after separate design review |

## Implementation sequence for a later phase

1. **Projection model:** derive a UI-only table profile and FK rows from the
   authorized graph; index by opaque id; preserve ordered pairs, certainty,
   incoming/outgoing direction, and coverage. Check whether default/generated
   column facets and FK actions need protocol additions before promising them.
2. **Entry and toolbar:** add a viewer item and context-aware command for a
   selected table. Reuse Objects toolbar components/patterns for connection,
   scope, filters, status, and actions. Leave object tab defaults unchanged.
3. **Viewer body:** reuse `CatalogDiagramRequest`; build hybrid GPUI canvas,
   selected-table details, list equivalent, selection/focus, bounded layout and
   routing, and local layout state. Retire the card modal only after command
   parity.
4. **Polish:** export, dense/large-schema behavior, accessibility, and visual
   review across light/dark themes and narrow panes.

No new server graph inference or driver trait signature is expected. A missing
public operation or metadata field must be specified through protocol/API/SDK,
authorization, audit, and capability review before coding it.

## Acceptance checks

- PostgreSQL and SQL Server: simple, composite, multiple, and self-referencing
  FKs; quoted and long names; cross-schema targets; empty table; wide table.
  SQLite and providers without graph capability get an honest fallback.
- Incoming and outgoing views agree on source, target, and ordered pairs.
  Partial, stale, truncated, unresolved, and inaccessible edges never appear
  complete or leak hidden names.
- Invoking the command opens the viewer on the chosen table; ordinary table
  selection never does. Scope controls update the viewer without changing the
  original table tab. Opening a related table, returning, changing pane, and
  refreshing preserve sensible selection/focus. No background editor
  receives diagram/viewer keys.
- A focused neighborhood opens quickly without waiting for whole-database
  layout. Expanding a dense hub remains bounded and explains omissions.
- Keyboard can perform all core browsing and inspection without a pointer;
  focus, edge direction, key roles, and partial state are available as text.
- Existing designer review, DDL, comparison, and Mermaid commands remain
  reachable. No schema mutation occurs from view or layout interactions.

## Decisions to settle before implementation

1. Should the initial body favor canvas or table details when a small pane
   leaves room for only one? Prototype both, with list access in either case.
2. Should saved diagram layouts be per user and connection profile, or only for
   the current workspace session? Start with session state until value is clear.
3. Which native column defaults/generated attributes and FK update/delete
   actions merit portable protocol fields? Display only proven current data in
   the first release.
