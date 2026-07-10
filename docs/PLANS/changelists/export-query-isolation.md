# Export query isolation

## Issue

The export path executes a fresh query directly through the driver and bypasses the cursor registry, request timeout, and explicit cancel-token flow used elsewhere.

## Current proof

- `crates/server/src/export.rs` `run_export` calls `driver.execute(handle, ExecuteRequest { ... }).await` and streams the driver receiver directly.
- It is not wrapped with `SessionStore::run_bounded`.
- It does not call `CursorRegistry::wrap`, so per-session cursor caps and spill/eviction policy do not apply.

## Failure mode

A client can start many exports to bypass cursor caps and hold database connections. A slow or wedged export depends on stream drop rather than the server's timeout/cancel path.

## Changelist

- Route exports through `SessionStore::execute_stream` or a dedicated bounded export runner.
- Apply request timeout and cancellation on disconnect.
- Count exports against the same per-session cursor/concurrency budget.
- Add tests that export honors cursor caps and timeout.
