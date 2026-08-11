# Phase K Graduation Matrix

Status: **graduated on 2026-08-10.** This records the evidence and hard
budgets behind ADR-032, ADR-033, and the Phase K modeling operations. The two
normative contracts remain `sql-semantic-service.md`,
`catalog-graph-schema-migrations.md`, and `phase-k-modeling-operations.md`.

## Delivered surface

| Area | Graduated behavior | Primary evidence |
| --- | --- | --- |
| SQL semantics | Revision-bound PostgreSQL/T-SQL parsing, formatting, diagnostics, completion, quick fixes, statement selection, usages, rename edits, and catalog-aware binding | `sift-semantic` unit corpus; completion corpus; server autocomplete and audit integration tests |
| Catalog graph | Typed, normalized graph; native identity hints; explicit coverage; deterministic revision/digest; refresh, stale fallback, policy projection, durable snapshots, diagrams, and declarative diagram mutation preview | core catalog tests; `server/tests/catalog_graph.rs`; live PostgreSQL and SQL Server graph suites |
| Schema diff | Live/snapshot sources; create/drop/alter/rename/move; accepted mappings; descendant correlation; stable dependency order; cycle groups; conservative risk and partial-coverage behavior | `sift-core::schema_diff` corpus and catalog snapshot integration |
| Migrations | PostgreSQL/SQL Server quoting and rendering, prerequisite closure, revision/policy/digest binding, destructive acknowledgements, bounded isolated execution, cancel, and durable statement/group outcomes | renderer corpus; `server/tests/migration_lifecycle.rs`; existing live PostgreSQL DDL round trips |
| Comparison | Table, cursor, and room-result sources; explicit/inferred/ordinal keys; duplicate multisets; typed tolerances; cross-engine values; opaque paging; cancellation; encrypted retained spill; optimistic edit preparation | core comparison matrix and `server/tests/comparison.rs` |
| Diagrams | Deterministic policy-filtered projection with exact FK column pairs; rename/FK/column intents feed the ordinary migration preview path | core catalog/migration tests and catalog graph integration |
| Plan capture | Exact semantic provenance, PostgreSQL and SQL Server normalization, estimate/analyze policy, raw-plan exclusion, durable bounded retention, paging, deletion, and normalized comparison | plan unit/integration tests, metadata plan-capture tests, and explain integration |
| Public API | Every Phase K action is typed, authorized, classified, audited, present in OpenAPI, and represented by the reference SDK | exhaustive operation authorization/capability tests and `openapi_matches_client_sdk_coverage_manifest` |

## Engine and safety fixtures

The live PostgreSQL fixture covers tables, views, materialized views,
partitioned tables, overloaded routines, triggers, types, sequences, columns,
indexes, constraints, native OIDs, dependency edges, composite FKs, and quoted
UTF-8 identifiers. The live SQL Server fixture covers tables, views,
procedures, scalar functions, synonyms, triggers, alias types, sequences,
columns, indexes, constraints, native object/column ids, routine/view/type/
sequence dependencies, composite FKs, and quoted UTF-8 identifiers. Node
families that an engine does not implement are not synthesized. Engine-neutral
unit fixtures cover partial graphs, inaccessible policy boundaries, unresolved
references, dependency cycles, shuffled provider order, and hostile graph
shapes.

Migration lifecycle evidence distinguishes all terminal states:

| Situation | Required result |
| --- | --- |
| Stale refreshed catalog or changed policy | Reject before statement one; retain no run |
| Invalid digest or missing acknowledgement | Reject without consuming an otherwise valid plan |
| Transactional group fails | Roll back attempted statements and report `rolled_back` |
| Non-transactional work commits, then failure/cancel | Report `partial`; never `failed` or `canceled` |
| Cancellation before remaining work | Stop at the safe boundary and record every unattempted statement as `skipped` |
| Any attempted DDL | Persist redacted fingerprints/outcomes and synchronously invalidate schema-derived caches |

Canonical graphs are validated before caching, then projected by principal,
tenant, profile-policy revision, and allowed schemas. Hidden targets become
metadata-free `inaccessible` boundaries. SQL, values, predicates, secret bytes,
and provider error fragments are excluded from ordinary errors and audit.
In-flight publication is epoch-checked, and a failed refresh can return only a
validated prior graph marked `stale`.

## Hard resource ceilings

| Resource | Default hard ceiling |
| --- | ---: |
| Catalog graph | 100,000 nodes; 500,000 edges; 16 MiB total definitions |
| Durable catalog snapshot | 32 MiB each; 100 and 256 MiB per tenant |
| Comparison source | 50,000 rows and 64 MiB per side; 512 columns |
| Comparison output | 20,000 retained diffs; 64 MiB each; 256 MiB and 1,024 active globally |
| Comparison encrypted-spill threshold / TTL | 2 MiB / 600 seconds |
| Plan capture | 8 MiB each; 5,000 per tenant; 50 per source; 30 days |
| Migration plan request | 100,000 selected ids; renderer rejects empty/unsupported/cyclic plans |

Plan-capture settings are operator configurable only downward. Catalog,
comparison, cursor, task, and execution paths retain their existing server
deadlines, cancellation tokens, and tenant resource admission.

## Repeatable performance budget

Run:

```text
nix develop --command cargo build -p sift-core --release --example phase_k_budgets
/usr/bin/time -f 'elapsed=%E max_rss_kb=%M user=%U system=%S' \
  target/release/examples/phase_k_budgets
```

The 2026-08-10 baseline used Linux x86-64, Rust 1.96.1, an 8-thread Intel
Core i5-8265U, and 7.6 GiB RAM. It is a repeatable regression baseline, not a
claim about network/database latency.

| Workload | Observed | Regression ceiling |
| --- | ---: | ---: |
| Build 10,000-table/90,002-node graph | 265 ms | 500 ms |
| Validate 90,002 nodes and 90,001 edges | 279 ms | 500 ms |
| Diff 100 typed changes | 455 ms | 750 ms |
| Compare 50,000 rows per side, 50 changed | 171 ms | 300 ms |
| Whole harness / maximum RSS | 1.346 s / 275,164 KiB | 2.25 s / 320 MiB |

The graph serialized to 69,300,998 bytes; the comparison inputs serialized to
4,077,781 and 4,077,881 bytes. Regressions above a ceiling require either a
fix or an explicit update here with new hardware, toolchain, and measurements.

## Release gates

Graduation requires all of the following to remain green:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
```

The live driver commands are documented in their test modules and require the
repository `.env` plus the local PostgreSQL and SQL Server fixtures. OpenAPI
operation-id uniqueness, orphan-reference validation, and SDK manifest parity
run as ordinary workspace integration tests.
