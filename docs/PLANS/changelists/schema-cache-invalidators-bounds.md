# Schema cache invalidator bounds

## Issue

Schema cache invalidators are one task and one extra database connection per distinct spec, with no hard bound. Dead invalidators are not removed, so they may never restart.

## Current proof

- `crates/server/src/schema_cache.rs` `ensure_invalidator` stores handles in `invalidators` by spec hash and returns early on `contains_key`.
- `pg_listen_task` returns on open/listen failure or when notifications end, but it does not remove its `invalidators` entry.
- `entries` and `invalidators` have no max-entry bound.

## Failure mode

A multi-tenant server can accumulate unbounded cache entries and invalidator connections. A transient PG LISTEN failure leaves the process permanently on TTL-only invalidation for that spec.

## Changelist

- Remove the invalidator map entry on task exit.
- Reconnect invalidators with bounded exponential backoff.
- Add max entries or LRU eviction for cached snapshots.
- Bound invalidator count and share invalidators by stable database identity where possible.
- Add tests for task-exit restart and max-entry behavior.
