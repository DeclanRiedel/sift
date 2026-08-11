# Phase L Graduation Matrix

Status: **graduated on 2026-08-11.** This records the executable evidence and
hard ceilings behind ADR-034 and Phase L workspaces, VCS, execution automation,
and transfers. The normative design remains
`phase-l-workspaces-vcs-automation.md`.

## Delivered surface

| Area | Graduated behavior | Primary evidence |
| --- | --- | --- |
| Virtual workspace | Room-owned revisioned trees, stable node identity, Loro-backed SQL documents, atomic batches, immutable checkpoints, bounded history, and restore-as-new-head | `sift-metadata::workspace::tests`; `server/tests/workspaces.rs` |
| Projection and DDL | Root-confined optional materialization, three-way reconcile plans, explicit conflicts, deterministic offline DDL models, and live mappings | `workspace_adapter` and `workspace_projection` unit matrices; workspace integration test |
| Git | Zed-inspired adapter boundary; typed status/diff/branches; checkpoint-bound stage/commit; fixed system Git; pending paths; bounded subprocesses; prompt, hook, helper, pager, and external-diff suppression | `git_adapter::tests`; `repository::tests`; workspace integration test |
| Runs | Revisioned configurations, immutable manifests, typed variables, schema/pre-task resolution, transaction/error policies, bounded logs, cancellation, query history, audit, and rerun provenance | metadata run matrix; `run_executor::tests`; exhaustive operation tests |
| Scheduling | Owner-bound five-field cron plus IANA timezone, unique occurrences, leases, misfire/concurrency policy, reauthorization, conservative restart recovery, inspection, and explicit resume | metadata run/schedule matrix; `scheduler::tests` |
| Transfers | Revisioned recipes; CSV/TSV/JSONL/JSON-array compatibility; escaped HTML/Markdown; formula-safe XLSX export and named-sheet import; expiring digest-addressed artifacts; streaming SDK | transfer metadata tests; `export::tests`; `transfer::tests`; OpenAPI/SDK parity |
| Extension formats | Installed `import_format`/`export_format` contributions hydrate with their versioned option schema and execute through tenant-scoped supervised generations using 256 KiB start/data/finish frames | `formatter_extension::tests`; Phase I package, framing, supervisor, cancellation, and quarantine corpus |
| Public contract | Every action is typed, room-authorized, audited, present exactly once in OpenAPI, and represented by the reference SDK | operation authorization/audit tests; `openapi_operation_ids_are_stable_and_unique`; `openapi_has_no_orphan_schema_refs`; `openapi_matches_client_sdk_coverage_manifest` |

## Deployment, conflict, and recovery matrix

Virtual state has no local-path dependency and therefore uses the same metadata
and authorization code in personal in-process, personal daemon, SSH-proxy,
network-hosted team, and container modes. Deployment-policy tests reject unsafe
mode/transport combinations. Remote lifecycle fixtures execute the real
probe/migrate command boundary and assert stable, redacted output. Projection
and Git capability discovery can be absent without removing the virtual tree.

The executable corpus covers concurrent online/offline Loro edits, stale
workspace revisions, idempotent document updates, checkpoint restore,
workspace-only/projection-only/both-changed reconcile states, edits arriving
after a commit checkpoint, read-only roots, symlink and hard-link escape,
hostile paths and Git identity, detached/dirty typed Git observations, and
bounded Git output. No cleanup command resets user state automatically.

Run and schedule tests cover ordered manifests, pinned/latest revision policy,
parameter versus identifier substitution around strings/comments, invalid
variables, every transaction/error policy constraint, cancellation/timeout,
cron/timezone parsing, unique enqueue, concurrency, lease takeover, explicit
resume, and restart conversion to `outcome_unknown`. PostgreSQL and SQL Server
driver corpora cover their parameter/null/transaction and cancellation
boundaries; driver panic/timeout containment proves a failed engine task cannot
freeze the server.

## Transfer, security, and backup matrix

Formatter option schemas are compiled before activation. Every input and output
frame is limited to 256 KiB and total artifacts/XLSX parts to 64 MiB. A crash,
malformed response, unavailable version, invalid schema, or size overrun ends
before the artifact insert, so no partial output becomes selectable. HTML and
Markdown escape markup; XLSX writes inline strings rather than formulas. XLSX
imports require an explicit sheet and pass their converted records through the
existing CSV inference/conflict/table-creation core.

Workspace, repository, run, schedule, recipe, and artifact lookups re-enter the
room/tenant authorization boundary. Paths are relative and normalized; Git
never receives a shell command or ambient credential helper. Credential DTO
debug output, operation payloads, and extension diagnostics are redaction
tested.

Backup now snapshots current SQLite state, then sanitizes the snapshot before
archiving it. Workspace/DDL/repository/run/schedule/recipe definitions,
checkpoints, completed histories, and immutable manifests round-trip. The
snapshot drops artifacts and repository credential handles, filters the
`vcs-credential` secret namespace, clears every scheduler lease, and converts
active run/occurrence state to terminal `outcome_unknown`; checkouts, sessions,
connections, cursors, and cancellation tokens were never durable. The
file-backend round-trip test proves a durable recipe survives while a VCS
secret and artifact do not.

## Metadata compatibility

Committed SQLite fixtures cover V32 through V37 in addition to the retained
V18/V19 contract boundaries. They exercise these Phase L migrations:

| Version | Boundary |
| ---: | --- |
| 32 | Room-owned virtual workspaces and checkpoints |
| 33 | Projection state and offline DDL sources |
| 34 | Repository/Git binding and checkpoint commits |
| 35 | Run configurations, manifests, steps, and logs |
| 36 | Durable schedules, occurrences, and leases |
| 37 | Transfer recipes and staged artifacts |

The matrix migrates every retained fixture to V37, accepts an unknown additive
tail above the V19 floor, and rejects a future contract floor. Lifecycle JSON
fixtures pin backup, restore, and remote migration output at V37.

## Hard resource ceilings

| Resource | Hard ceiling |
| --- | ---: |
| Workspaces / room; nodes / workspace; atomic mutations | 32; 10,000; 100 |
| Checkpoint capture; retained checkpoint bytes; checkpoints | 64 MiB; 256 MiB; 200 |
| Projection entries; file bytes; scan bytes | 20,000; 8 MiB; 64 MiB |
| Git stdout or stderr; status entries; diff files | 8 MiB each; 20,000; 2,000 |
| Scripts / run; variables / run; run timeout | 100; 64; 3,600 seconds |
| Run logs; bytes / log | 10,000; 4 KiB |
| Schedules / configuration; scheduler claim batch | 16; 100 |
| Formatter input/output frame; artifact/XLSX | 256 KiB; 64 MiB |

## Repeatable timing evidence

The 2026-08-11 baseline used Linux x86-64, Rust 1.96.1, an 8-thread Intel Core
i5-8265U, and 7.6 GiB RAM. These are deterministic server-boundary regression
budgets, not claims about a user's database or WAN latency.

| Workload | Observed test body | Regression ceiling |
| --- | ---: | ---: |
| Three workspace collaboration/projection/Git integration scenarios, serial | 0.91 s | 2.0 s |
| SSH-proxy remote lifecycle/probe/migrate fixture | 0.13 s | 0.50 s |
| Complete metadata unit/compatibility matrix (78 passing, one regeneration test ignored) | 7.76 s | 15 s |

Run the first two with `--test-threads=1` to compare test-body times. SSH adds
the deployment's network RTT to ordinary protocol calls; the proxy keeps the
database and all workspace/Git execution server-side, so it does not add a
second synchronization or checkout path.

## Release gates

Graduation requires all of the following to remain green:

```text
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
```

Live PostgreSQL and SQL Server suites remain opt-in because they require the
repository `.env` and local service fixtures. Engine-neutral state-machine,
renderer, policy, cancellation, and driver-isolation coverage runs in the
ordinary workspace gate.
