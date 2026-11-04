# Live Benchmark and Profile progress

Design before implementation. Add one audited `ReadPerformanceProgress` action
and bounded read endpoints for an exact session/source connection/run UUID and
Benchmark or Profile mode. A fixed-size in-memory snapshot belongs to each
existing active-run registration and disappears with it. Do not retain completed
jobs or result rows. An inactive response is null, never proof of completion;
the original response remains authoritative for results and terminal errors.

Return only run identity/mode, stage, monotonic server elapsed milliseconds,
requested/completed warm-up and measured iteration counts, last attempt outcome,
and cancellation-requested state. Profile exposes stages without a percentage,
row estimate or fabricated database progress. Benchmark counts completed attempts,
including unsuccessful ones; its immutable final report keeps existing timing
populations and sample semantics. Setup and cleanup time are included only in
the progress elapsed clock, never in benchmark latency samples.

Read authorization must recheck session ownership, connection provenance,
ReadPerformanceProgress permission and the original Benchmark/Profile plus
Execute permission against privately retained frozen SQL. No SQL, bind values,
source result rows, native diagnostic text or secrets cross the progress API or
audit. Wrong run UUID/mode is inactive. HTTP responses use no-store. Reading
progress performs no driver work. Keep the locked Driver signatures unchanged.

Update stage before setup, workload and cleanup; update benchmark counters after
each attempt and distinguish configured inter-query delay. Existing supervisors,
cancellation, deadlines, quota reservations and dedicated-connection cleanup
remain authoritative. State is fixed-size apart from the already bounded frozen
SQL used for policy rechecks, and is released with registration even on failures.

The desktop awaits the existing result while polling progress at most twice per
second, with one bounded request in flight. It stops polling when the result
arrives, and transient/unsupported progress errors do not replace workload
results. Deliver updates through the existing background executor, bound to
item, source instance/profile/session/connection, a per-connection generation and exact run UUID. A late
update cannot replace another run, revive a finished run, or move keyboard
focus. The Vim Performance panel shows stage, attempt counts and elapsed time;
existing completed sample lists remain virtualized and bounded. Typing and
cancel controls stay responsive throughout. Definitions use the same runner.

Test progress during a held benchmark/profile, partial attempts, cancellation,
cleanup/removal, exact identity, permission loss and privacy. Verify stale and
post-completion UI updates are ignored and ordinary editor input remains usable.
Run formatting, strict workspace Clippy and workspace tests, reconcile the
canonical inventory, graduate a stable ADR and commit the milestone.

## Acceptance

The full workspace suite and strict all-target Clippy pass. Acceptance uses
managed native SQLite for HTTP/SDK lifecycle and policy tests, a mock PostgreSQL
supervisor for cancellation/no-driver-read evidence, GPUI input/source tests and
a stalled-acknowledgement desktop poller test. This does not claim native
PostgreSQL/SQL Server progress percentages, or a measured 120 Hz frame budget.

```sh
cargo fmt --all
cargo clippy --locked --offline --workspace --all-targets -- -D warnings
cargo test --locked --offline --workspace
```

Windows/macOS host acceptance remains deferred. Broader workbench gaps, including
controlled A/B, attached plans, retention/context, suites and advanced load tools,
remain open in the canonical query-performance checklist.
