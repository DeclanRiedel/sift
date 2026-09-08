# IDE feature milestones

Requested scope: foreign-key JOIN completion, multi-hop JOIN suggestions,
Vim multi-cursor editing, image/blob result viewers, general object designer,
and shared-query browser. Commit each usable milestone after validation.

## Design

1. JOIN completion consumes catalog-proven foreign-key column pairs. Offer
   reviewable completion snippets at JOIN table slots; quote identifiers,
   preserve source aliases, and support composite keys. Traverse at most three
   edges with bounded candidates and no repeated relations. Missing graph
   metadata falls back to ordinary completion. Reuse the audited completion
   operation and existing bounded schema service; no Driver signature change.
2. Multi-cursor editing uses one document transaction for all selections and
   one undo step. Vim commands create/remove selections; edits preserve UTF-8
   boundaries and normalize overlapping ranges.
3. Binary cell viewers show bounded hex/text and decoded image previews with
   explicit size/type information. Keep raw result bytes outside CRDT state.
4. Object design uses existing DDL, table migration preview/apply, and query
   execution operations, provider capabilities, and production confirmation.
   Expose supported object kinds with editable SQL and review before execution.
5. Shared-query browsing uses room/document permissions and existing audited
   operations. Show searchable shared documents and open the selected document
   in its room without copying collaboration state into results or sessions.

Implementation details and acceptance evidence are recorded per milestone.

## JOIN milestone

Implemented direct/reverse FK snippets and bounded paths up to three hops.
Uses graph column pairs rather than parsing provider constraint definitions.
Top-level JOIN slots only; nested/parenthesized statements and CROSS/NATURAL
JOIN retain ordinary completion. Graph fetch has a 250 ms UI wait budget and
continues under existing driver supervision to warm the shared cache.

Validation: completion and semantic suites (59 tests) pass; Clippy for
completion, semantic, and server, including all targets, passes with warnings
denied. Full workspace validation follows the remaining UI milestones.

## Vim multi-cursor milestone

Ctrl+Alt+Up/Down adds a cursor on the adjacent line, up to 128 cursors.
Use `i` and ordinary Vim insert commands to edit every cursor. Escape closes
followers. Cursor groups, selections, and undo are owned by ModalKit; Sift
mirrors each resulting edit into one CRDT update and renders all cursors.
Automatic/manual completion is suppressed while multiple cursors are active.
Authoritative room updates reset the local group to avoid stale positions.

## Binary viewers milestone

The result inspector's Value tab shows PNG/JPEG/GIF/WebP first-frame previews,
pixel dimensions, and paged hex/ASCII inspection (`j/k`, 4 KiB per page; `y`
copies the displayed page). Decoding runs off the UI
thread, bounded to 16 MiB input, 4096×4096 dimensions and 64 MiB decoder allocation.
Invalid/unsupported images retain hex inspection; previews cache only the
selected binary value and discard stale decode responses.

## Object designer milestone

Objects → Design (`d`) supports canonical PostgreSQL/SQL Server views,
functions, procedures and triggers as editable replacement SQL. The draft
retains its object's connection/database binding and is read-only until DDL
loads. Review the script and use normal audited execution with existing
production confirmation. Table design retains its structured migration
preview/apply; sequence/type design retains dedicated provider templates.
Unrecognized definitions remain read-only. SQLite view replacement and
materialized-view replacement are not offered because they require distinct
drop/recreate workflows.

## Shared-query browser milestone

Use “Find Shared Query…” (`<leader> f c`), or open the command palette and type
`&` to browse shared queries. Search matches
document titles, tenant names, and room names; rows show tenant/room context.
Enter opens the live room document and reuses an existing tab. Browser entries
come exclusively from the authenticated lifecycle's accessible rooms; opening
reuses the existing room-document service and its authorization/audit path.

## Acceptance

`cargo fmt`, strict workspace/all-target Clippy, and `cargo test --workspace`
passed. Final targeted reruns passed all 454 workspace UI tests, 35 desktop
tests, five snippet tests, and 59 completion/semantic tests. The keymap suite
caught and verified the shared-browser shortcut correction to `<leader> f c`.
No new live-database acceptance run was performed for these UI changes.
Stable ownership and execution decisions are recorded in ADR-057.
