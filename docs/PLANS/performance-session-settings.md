# Observed performance session settings

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#performance-workbench).

Benchmark and Profile capture an explicit allowlist on their disposable dedicated
connection before starting workload transactions, instrumentation or sample clocks.
Capture is itself read-only and can warm metadata caches; it does not establish a
cold cache or a complete environment. Existing isolation/preparation declarations
remain separate from this before-workload observation. Never inspect the editor
connection or copy its overrides. Never capture users, connection strings, paths,
search paths, arbitrary settings or bind values.

Add an optional typed observation to both environment contracts: phase and a
bounded ordered name/value map. Legacy reports omit it. Capture at most one row
of fixed projected scalar columns (128 bytes per value), with a five-second cap,
run cancellation and the remaining benchmark deadline. Supervise execute and
stream drain on a spawned task, reserve query resources, and cancel/discard on
interruption or failure. No Driver signature changes or new user action: this
read is setup within the existing audited Benchmark/Profile operation. Capture
errors must never run the workload on an uncertain connection. Completed but
unrecognized projections remain explicitly unavailable.

PostgreSQL reads selected current_setting values; SQL Server reads only @@SPID's
row in sys.dm_exec_sessions and selected scalar flags; SQLite reads table-valued
PRAGMA functions with explicit call syntax and no assignments. A same-name table
cannot be read as settings; a namespace collision refuses capture. PostgreSQL
functions are schema-qualified and missing optional settings remain unavailable. Keep a fixed allowlist per engine. Unknown
or null values cannot be relabeled as observed settings. Saved snapshots remain
unattested user submissions; bound observation input before private saving.

Show bounded observations beside their phase in Performance and saved-run views;
comparisons disclose missing/different observations and incomplete coverage.
SQLite authorizes only argument-free reads of these selected diagnostics;
assignments remain forbidden. Native acceptance must prove actual values,
PostgreSQL/SQL Server editor override isolation and
unchanged user workload counts. Cover cancellation/bounds and malformed results.
Run formatting, strict Clippy and workspace tests before committing. Windows and
macOS host acceptance remains deferred.

References: [PostgreSQL configuration settings](https://www.postgresql.org/docs/current/config-setting.html),
[SQL Server own-session visibility](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-views/sys-dm-exec-sessions-transact-sql?view=sql-server-ver17),
and [SQLite table-valued PRAGMAs](https://www.sqlite.org/pragma.html).

## Linux acceptance — 2026-11-04

Implemented in both audited runners, pure protocol environments, bounded private
snapshot admission, descriptive comparison warnings, Performance and saved-run
views. ADR-128 records phase, authority and timing semantics.

Workspace acceptance proves legacy environment decoding, rejection of unsupported
scalar values/controls/oversized text, excess-row refusal before workload start,
owned-connection cleanup and source survival. Existing hung-run cancellation
acceptance now exercises observation setup. HTTP/SDK, reusable definition rerun,
CLI private save and comparison-display regressions pass with setup kept outside
workload samples.

Native SQLite HTTP acceptance observes nine settings for Benchmark and Profile,
checks before-transaction query_only/default cache_size, preserves the editor's
settings and workload row/sample counts, refuses a shadow table as a PRAGMA
function, and rejects assignments to all nine newly readable diagnostics.
Direct authorization inspection confirms only SELECT, read and argument-free
PRAGMA callbacks. No diagnostic assignment is enabled.

Native PostgreSQL and SQL Server server acceptance passes with actual nine-value
observations for Benchmark and Profile. Editor statement_timeout / LOCK_TIMEOUT
overrides remain on the editor connection and are absent from dedicated setup.
User workload samples remain exactly one completed parameterized read. Existing
restricted-login SQL Server Profile acceptance also passes: the caller observes
its own session without a new broad server-state grant, while SHOWPLAN remains
separately enforced.

Passed:

- `cargo fmt --all` and final `cargo fmt --all -- --check`
- `cargo clippy --locked --offline --workspace --all-targets -- -D warnings`
- `cargo test --locked --offline --workspace --no-fail-fast`
- `cargo test --locked --offline --workspace --features sift-server/live-pg,sift-server/live-mssql --test plan_capture_live --test profile_showplan_live -- --test-threads=1`
- `cargo clippy --locked --offline --workspace --features sift-server/live-pg,sift-server/live-mssql --test plan_capture_live --test profile_showplan_live -- -D warnings`

This completes selected before-workload observations. Full setting coverage,
actual later overrides, preparation internals, cache/load/data equivalence and
other performance milestones remain open. Native Windows/macOS host acceptance
remains deferred.
