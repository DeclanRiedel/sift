# Schema cache singleflight and clone cost

## Issue

Schema cache misses are not coalesced, and cache hits clone the full `SchemaSnapshot`.

## Current proof

- `crates/server/src/session.rs` checks `schema_cache.get` and calls `driver.schema` directly on miss.
- `crates/server/src/schema_cache.rs` stores `snapshot: SchemaSnapshot` and returns `entry.snapshot.clone()`.
- Autocomplete then builds a fresh completion dictionary from the cloned snapshot.

## Failure mode

Ten clients opening the same schema panel at once trigger ten backend schema scans. Repeated cache hits on deep snapshots allocate and copy large trees.

## Changelist

- Store snapshots as `Arc<SchemaSnapshot>` and return cheap clones.
- Add per-key singleflight for in-progress schema fetches.
- Cache derived completion dictionaries beside snapshots or expose an `Arc<Dictionary>`.
- Add concurrency tests proving one miss performs one driver schema call.
