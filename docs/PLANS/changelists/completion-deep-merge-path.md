# Completion dotted deep-merge path

## Status

Implemented in the working tree: completion can rank from an already detected
context, object-path resolution lives on the completion dictionary, and deep
schema merges match by `(catalog, schema, name)`. Tests cover a shallow/deep
mock-driver completion path and duplicate object names across schemas.

## Issue

The server autocomplete path runs completion twice for qualified column completion and merges deep schema data into a shallow snapshot with an O(shallow * deep) name-only scan.

## Current proof

- `crates/server/src/autocomplete.rs` calls `sift_completion::complete`, detects `ExpectingColumn { qualifier: Some(_) }`, fetches deep schema, merges, then calls `complete` again.
- `merge_deep_into_shallow` matches by object name only, ignoring schema/catalog.

## Failure mode

Typing `orders.` doubles context detection, dictionary construction, and ranking. If `public.orders` and `sales.orders` both exist, columns can be merged into the wrong object.

## Changelist

- Split completion into detect and rank phases so context is computed once.
- Build a `(catalog, schema, name)` index for merge.
- Move object-path resolution into the completion crate so the server does not duplicate dictionary lookup rules.
- Add tests with duplicate table names across schemas and a mock driver that distinguishes shallow from deep scope.
