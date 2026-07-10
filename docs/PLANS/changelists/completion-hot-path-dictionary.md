# Completion hot-path dictionary rebuild

## Status

Core cache fix implemented in the working tree: `SchemaCache` now builds a
`sift_completion::Dictionary` once per inserted snapshot, stores it behind an
`Arc`, and the autocomplete endpoint ranks against the cached dictionary on
cache hits. Follow-up still open from the original changelist: criterion
benchmarks and any additional prefix-index tuning.

## Issue

Autocomplete rebuilds a full `Dictionary` from the schema snapshot on every request.

## Current proof

- `crates/completion/src/lib.rs` calls `Dictionary::from_snapshot(snapshot)` for each `complete`.
- `Dictionary::from_snapshot` clones schema, object, column, and type-display strings and builds hash maps.
- The current dirty tree fixes per-candidate lowercase allocation in ranking, but dictionary rebuild remains.

## Failure mode

On a schema with thousands of tables and columns, every keystroke allocates and indexes MB-scale data even though the schema only changes on cache refresh or invalidation.

## Changelist

- Build `Dictionary` once when a schema snapshot is inserted into cache.
- Return `Arc<Dictionary>` to autocomplete.
- Precompute lowercased names and sorted prefix indexes in the dictionary.
- Add criterion benchmarks for dictionary construction and per-keystroke completion at 1k and 10k objects.
