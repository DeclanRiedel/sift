# Relational table viewer and ER diagram

Status: **design draft, 2026-09-25. No implementation authorized by this plan.**

Scope: the Sift workspace UI for browsing database tables and their structure.
“ER diagram” here means a navigable view of actual catalog relationships, not a
data model that invents relationships from matching names. This plan builds on
ADR-033 and the existing table definition inspector and catalog diagram API.

## Product jobs

1. Open a table and answer: what are its columns, keys, indexes, constraints,
   and nearby tables?
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

### Table viewer

The object tab remains the home for a single table. Its header shows qualified
name, object kind, connection, catalog revision/refresh state, and actions:
**View data**, **Copy name**, **DDL**, **Show in diagram**, **Design** (when
authorized). The active content has two clear views:

- **Data**: existing result preview and editing flow.
- **Structure**: one scrollable, read-only table profile. The current definition
  inspector can evolve into this view. It has sections for Columns, Keys and
  constraints, Relationships, Indexes, Triggers, and Dependencies. Section links
  and counts stay visible in a narrow navigation strip or compact menu.

Keep a shortcut between Data and Structure, and remember the chosen view per
object tab. Opening from schema search should show Structure; opening a table
for row work should show Data. Design is a separate explicit state entered from
Structure, never an implicit effect of clicking a field or ER edge.

Suggested Structure layout at normal width:

```text
public.orders  · Table  · DB: sales     [Data] [Structure] [Diagram] [DDL] [Design]
Columns (8) | Relationships (3) | Indexes (2) | Constraints (4) | ...
Name          Type             Nullable   Key / default
id            bigint           No         PK · identity
customer_id   bigint           No         FK → customers.id
...
Outgoing (1)                         Incoming (2)
orders.customer_id → customers.id     order_items.order_id → orders.id
                                     payments.order_id → orders.id
```

At narrow widths, stack column attributes and relation endpoints; keep names
copyable and full text accessible. Never rely on truncated labels or color alone.

Column rows show ordinal, name, native/display type, nullability (`Unknown`
stays unknown), PK/FK/unique role when proven, and available default/generated
facets. Selecting a column opens a small detail area with exact metadata and
links to participating constraints and indexes. If a field is absent from the
protocol, omit it or label it unavailable; do not infer it from DDL text.

Relationship rows split **References** (outgoing) and **Referenced by**
(incoming). Show constraint name, qualified source/target, ordered column pairs,
and certainty. Selecting a row highlights its columns and offers **Open table**,
**Show path in diagram**, **Copy JOIN** (only when catalog-proven and the existing
join generator can produce it), and **DDL** when available. Preserve separate
rows for multiple FKs between the same tables, self references, and composite
keys. Never turn a hidden policy boundary into a named target.

### ER canvas

Use a resizable workspace item/pane, not the current modal, for exploration.
The modal's command can open this pane. Opening from a table starts at that
table plus one FK hop; opening from the database command starts with a schema or
table picker and a bounded initial selection. Provide search, schema filters,
incoming/outgoing toggles, and depth control. The canvas owns pan, zoom, fit,
reset, and minimap only if navigation testing shows a need. A companion list
exposes the same nodes and edges for keyboard and assistive access.

Each table card shows qualified name and a compact field list: key fields first,
then remaining fields, with name, type, and PK/FK/nullable marks. Expand a card
to show all fields. The edge connects specific field anchors where column pairs
are known. Edge selection shows constraint name, each ordered pair, source and
target, certainty, and available actions in a side inspector. Distinguish
outgoing/incoming by arrow direction and text; do not claim cardinality from an
FK alone. Self references loop visibly. Parallel FKs remain distinct and
selectable. Views and other dependency kinds can be a separate overlay; the
default ER layer shows table-like objects and catalog-proven FKs.

```text
Search tables...  Schema: public  Depth: 1  [Incoming ✓] [Outgoing ✓] [Fit]
┌─ tables / paths ─┐  ┌──────────── canvas ────────────┐  ┌─ details ───┐
│ orders           │  │ customers.id ← orders.customer_id│  │ FK name     │
│ customers        │  │    [customers] ←── [orders]      │  │ column pairs│
│ order_items      │  │                      ↑            │  │ Open / DDL  │
└──────────────────┘  └──────────────────────────────────┘  └─────────────┘
```

Canvas layout is deterministic for a given visible set, with no moving nodes
after an unrelated refresh. Local drag positions may be saved as presentation
preferences keyed by connection/profile, database identity, object id, and
layout version. Missing or renamed objects drop their pins gracefully. Saved
positions and filters contain no graph truth or row data. Export uses the exact
visible projection and records partial/omitted state in the output. Existing
Mermaid copy remains; image export is a later design choice.

## Interaction and state

- **Find → inspect → follow → return:** schema search selects an object; Enter
  opens it; Relationships selects an FK; Enter opens its target in a new or
  reused tab according to the normal tab rule; Back returns to prior selection.
  Diagram selection can open the same Structure view. Focus and scroll restore.
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
  Structure content when graph capability is missing; graph-dependent controls
  explain why they are unavailable.
- **Safety:** browsing, projection, DDL retrieval, export, and navigation use
  existing audited operations. Design and migration stay in their review and
  authorization flows. A diagram drag only changes layout; it never changes
  schema. Hidden names and edge endpoints stay hidden in text, exports, and
  accessible labels.

## Feature map and priority

| Capability | First release | Later, if useful |
| --- | --- | --- |
| Table profile | Readable columns, keys, outgoing/incoming FKs, indexes, constraints, triggers, dependencies | Engine-specific facets and richer column details as protocol supports them |
| Navigation | Search → table → related table; data/structure/DDL/diagram handoff; back/forward | Saved object collections |
| Diagram | Focused one-hop canvas, expand/collapse, pan/zoom/fit, FK pair inspector, list alternative | Multiple layouts, larger custom scopes, overview minimap |
| Filtering | Schema, text, direction, depth, bounded node count | Dependency overlays and custom views |
| Sharing | Copy Mermaid with coverage note; copy qualified names | Image/SVG export, shareable layout presets |
| Editing | Existing explicit table designer and migration preview | Diagram-origin draft FK editing only after separate design review |

## Implementation sequence for a later phase

1. **Projection model:** derive a UI-only table profile and FK rows from the
   authorized graph; index by opaque id; preserve ordered pairs, certainty,
   incoming/outgoing direction, and coverage. Check whether default/generated
   column facets and FK actions need protocol additions before promising them.
2. **Table viewer:** improve the existing object tab and inspector, wire
   navigation/actions through existing commands, and keep Data/Design behavior.
3. **Diagram pane:** reuse `CatalogDiagramRequest`; add bounded layout and edge
   routing off the UI thread, a list equivalent, selection/focus, and local
   layout persistence. Retire the card modal only after command parity.
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
- Opening a related table, returning, switching Data/Structure, changing pane,
  and refreshing preserve sensible selection/focus. No background editor
  receives diagram/viewer keys.
- A focused neighborhood opens quickly without waiting for whole-database
  layout. Expanding a dense hub remains bounded and explains omissions.
- Keyboard can perform all core browsing and inspection without a pointer;
  focus, edge direction, key roles, and partial state are available as text.
- Existing designer review, DDL, comparison, and Mermaid commands remain
  reachable. No schema mutation occurs from view or layout interactions.

## Decisions to settle before implementation

1. Should Structure open by default for table selection, or only when selection
   comes from schema search? The proposal above uses the entry point.
2. Should saved diagram layouts be per user and connection profile, or only for
   the current workspace session? Start with session state until value is clear.
3. Which native column defaults/generated attributes and FK update/delete
   actions merit portable protocol fields? Display only proven current data in
   the first release.
