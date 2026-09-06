# Database provider overnight handoff

Status: **implementation authorized and started, 2026-09-06.**
SQLite design is complete as a planning artifact, not implementation evidence.

The user additionally requested advanced SQLite graduation, a seeded SQLite
connection in `nix run .#sift-desktop-demo-wiki` and related demos, and an easy
command to inspect Sift's own SQLite metadata. Commit validated milestones.
After S1–S5, design and implement savepoints, structured explain, and bounded
CSV import before evaluating scoped SQLite graduation. Metadata inspection must
be an explicit local-owner, read-only workflow with its own authorization and
audit; ordinary SQLite profiles must not gain arbitrary access to Sift state.
Prefer an isolated sanitized SQLite inspection snapshot so credentials and
authentication artifacts never become queryable through the convenience command.
Run tasks in order on the preceding task's resulting tree. They modify shared
protocol/server code and must not run concurrently in the same checkout.

Preserve existing user changes. Read README.md and AGENTS.md, then the named
plans. Keep UI Vim-only, protocol pure serde, actions audited, secrets outside
metadata, and driver work isolated. Do not enable CI, add smoke scripts, invent
credentials, or reset an existing database. Use existing local development
helpers and configured disposable fixtures. A task that lacks required evidence
records the gap; it must not claim graduation to unblock its successor.

## Task 1 — Close PostgreSQL/SQL Server correctness scope

Suggested task prompt:

> Implement the DDL and value/execution-boundary work in
> docs/PLANS/postgres-sqlserver-graduation.md, using docs/PLANS/ddl-gaps.md.
> Design metadata/protocol changes first. Fix supported trigger/type DDL,
> generated/computed columns, collation, identity options, and index/partition
> fidelity. Make unsupported shapes fail explicitly; record narrow exclusions
> rather than inventing lossy output. Resolve or explicitly document native-value
> and parameter boundaries. Do not rebuild completed sequence DDL or add DBA
> features. Add focused regression coverage, run required local workspace checks,
> and update the support matrix with actual behavior and remaining live evidence.

Done: supported scope is reviewable in code/docs; no known silent DDL loss inside
that scope; focused/workspace checks pass. Any exclusions have clear user-visible
unsupported behavior. No claim of live-engine acceptance yet.

## Task 2 — Prove both live engines

Suggested task prompt:

> Starting from Task 1, complete the acceptance evidence section of
> docs/PLANS/postgres-sqlserver-graduation.md. Run existing opt-in live driver
> suites. Extend PostgreSQL DDL fixtures and add SQL Server round trips for the
> declared scope. Validate plans, imports, supported process/notification paths,
> transactions, timeouts/cancel, recovery and restricted-user behavior through
> server paths. Fix failures within that scope. Record exact commands, engine
> versions, results, resource/performance evidence, and unrun cases. Use existing
> configured disposable databases; never substitute mocks for live acceptance.

Done: both engines meet the scoped matrix with recorded evidence and required
workspace checks. If fixture services/credentials are unavailable, report the
specific missing prerequisite and leave acceptance unchecked. Reuse .env and
existing helpers; never print secret values.

## Task 3 — Record scoped graduation

Suggested task prompt:

> Review Task 1/2 code and evidence against
> docs/PLANS/postgres-sqlserver-graduation.md. If every required gate passes,
> record the scoped PostgreSQL/SQL Server graduation decision in
> docs/DECISIONS.md and reconcile the canonical inventory, DDL backlog, provider
> limitations and protocol compatibility notes. Preserve explicit exclusions.
> If any gate is missing or failed, document remaining work and do not mark
> graduation complete. Do not manufacture test results or treat implementation
> existence as acceptance evidence.

Done: a justified graduation ADR with a bounded support matrix, or an honest
blocked evidence report. Broader DBA features remain queued separately.

## Task 4 — Implement SQLite daily-driver scope

Suggested task prompt:

> First verify Task 3 recorded successful scoped two-engine graduation. If not,
> report that prerequisite and do not begin SQLite implementation. Otherwise
> implement S1–S5 in docs/PLANS/sqlite-provider.md. Use a native rusqlite driver,
> explicit server-file root authority, bounded dedicated workers, cancel/discard,
> truthful dynamic values, managed transactions, exact scoped DDL and SQLite
> dialect/UI support. Rebase protocol changes on the preceding tasks. Gate all
> unsupported features. Run focused temporary-file integration tests and required
> workspace checks, document evidence, and graduate only the proven SQLite scope.

Done: users can open an authorized existing file, query/cancel, browse schema,
view DDL, edit eligible rows, export results, and use managed transactions via
the existing server/SDK/Vim desktop path. Deferred features stay visibly gated.
Publish limitations and any unrun platform/performance acceptance checks.

## Queue behavior

These are dependency-ordered work packets, not promises that all work fits in
one night. If the runner queues tasks independently, include the prerequisite
checks above in each prompt; queue order alone is not proof the previous task
succeeded. Leave a handoff containing changed files, checks/results, current
protocol version, exclusions, and remaining work after every task. Scheduling
and execution are separate from preparing this document.
