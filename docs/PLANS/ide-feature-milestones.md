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
4. Object design uses existing DDL and migration preview/apply operations,
   provider capabilities, and production confirmation. Expose supported object
   kinds with editable SQL and review before execution.
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
