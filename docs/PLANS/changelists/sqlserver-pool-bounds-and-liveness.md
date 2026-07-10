# SQL Server pool bounds and liveness

## Issue

The SQL Server warm pool map is unbounded, and warm connections are handed out without liveness validation.

## Current proof

- `crates/driver-sqlserver/src/lib.rs` stores `pools: DashMap<String, Arc<Mutex<MssqlPool>>>` with no max size or eviction.
- `pop_warm` returns `idle.pop_front()` without pinging the connection.
- `ensure_warm` uses a `refilling` flag that is reset only on normal task completion.

## Failure mode

Many distinct specs can permanently retain pool entries and warm sockets. After a backend restart or idle timeout, the next user receives a dead connection and sees the first query fail.

## Changelist

- Add a pool cap and best-effort idle eviction similar to the Postgres driver.
- Validate warm connections with `SELECT 1` before returning them; discard dead ones and try another.
- Reset `refilling` through a guard so panics cannot wedge refill.
- Add tests for bounded pool growth and stale warm connection discard.
