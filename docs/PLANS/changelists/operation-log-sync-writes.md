# Operation log synchronous writes

## Issue

The in-memory operation log and optional JSONL writer sit behind one process-global mutex. Appending an operation writes JSON, writes a newline, and flushes while holding that lock.

## Current proof

- `crates/server/src/session.rs` `push_operation_full` locks `self.inner.operations`.
- Inside the lock it calls `serde_json::to_writer`, `write_all`, and `flush`.
- `list_operations` clones the full entries vector under the same lock.

## Failure mode

Every user-visible operation across every session serializes on this lock. A slow filesystem flush stalls operation recording globally and can add tail latency to unrelated requests.

## Changelist

- Wrap the JSONL file in `BufWriter` and remove per-entry flushes.
- Move disk writes to the existing audit-writer thread or a dedicated bounded writer.
- Keep the in-memory ring update small and lock-local; clone snapshots outside the lock where possible.
- Add a stress test that records many operations while listing operations.
