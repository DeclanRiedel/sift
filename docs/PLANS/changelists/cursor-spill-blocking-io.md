# Cursor spill blocking I/O

## Issue

Cursor spill read/write paths perform filesystem I/O, JSON encode/decode, and `sync_all` synchronously on async request or pump tasks.

## Current proof

- `crates/server/src/cursors.rs` `write_spill` uses `serde_json::to_vec`, `write_all`, and `file.sync_all`.
- `emit_terminal` calls `write_spill` directly from the async pump task.
- `read_spill_pages` opens, seeks, reads, allocates, and decodes pages synchronously.
- `crates/server/src/http.rs` `read_spilled_cursor_pages` returns up to 256 pages as one JSON response.

## Failure mode

Spilling a large cursor can block a tokio worker for tens to hundreds of milliseconds per fsync, or longer on network storage. Reading spilled pages can allocate and serialize hundreds of MB in one handler.

## Changelist

- Move spill write and read work into `spawn_blocking`.
- Make `sync_all` configurable or defer it to a lower-priority durability mode.
- Replace JSON spill format with a compact binary encoding or pre-serialized page format.
- Stream spill reads as NDJSON or bounded chunks instead of returning a large JSON array.
- Make `approx_page_bytes` value-aware so spill thresholds reflect actual row width.
