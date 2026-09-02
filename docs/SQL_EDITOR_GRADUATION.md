# SQL editor graduation evidence

Measured 2026-09-02 on Linux x86_64, a 13th Gen Intel Core i7-13620H with
16 logical CPUs. Rust latency numbers use the optimized `bench` profile.
GPUI numbers use the repository Nix development shell and headless Blade
renderer. Criterion intervals report the lower, estimate, and upper bound.

## Latency budgets

Run with:

```sh
cargo bench -p sift-completion --bench sql_editor_latency -- --noplot
nix develop -c cargo bench -p sift-workspace-ui --features benchmark \
  --bench frame_budget -- result_set_tab_navigation
```

| Path | Fixture | Measured interval / p95 | Budget | Result |
| --- | --- | ---: | ---: | --- |
| Warm completion | 100,000 catalog objects | 0.236–0.241 ms | 30 ms | pass |
| Warm hover | 500-column relation | 0.00625–0.00636 ms | 30 ms | pass |
| Changed-statement semantics | 100-line revision | 0.483–0.499 ms | 5 ms | pass |
| Full-document semantics | 8,000 statements | 20.385–21.017 ms | 50 ms | pass |
| Star expansion | 500 columns | 0.0588–0.0604 ms | 50 ms | pass |
| Snippet lookup | 2,000 snippets | 0.0119–0.0122 ms | 2 ms | pass |
| Result-set tab navigation | eight visible result sets | 7.766 ms draw p95 | 8.3 ms | pass |

The result-tab run observed 1,211 frames: 6.975 ms draw p50, 7.766 ms p95,
9.224 ms p99, and 13.468 ms maximum, with one invalidation per frame. Larger
result-set counts remain horizontally scrollable; inactive sets retain prepared
data and do not shape cells.

Execution progress is capped at four publications per second by
`progress_is_coalesced_to_four_updates_per_second`; native progress uses the
same coalescer and cannot become a terminal execution event.

## Correctness and resilience corpora

- `dialect-completion-corpus.json` fixes dialect, engine, connection/database
  identity, catalog revision, UTF-8 cursor, replacement range, ordered leading
  candidates, and forbidden candidates. The broader completion suite covers
  empty and partial input, qualification, quoting/case folding, aliases,
  ambiguity, joins, CTE/local objects, functions and overloads, DML pseudo
  relations, invalid/incomplete SQL, and catalog isolation for PostgreSQL and
  SQL Server.
- `deterministic_utf8_fuzz_corpus_never_panics_or_returns_invalid_ranges`
  validates 512 deterministic inputs against both dialect packs.
- `single_token_mutation_corpus_keeps_completion_ranges_bounded` mutates valid,
  incomplete, quoted, temporary-object, CTE, and DML inputs at every UTF-8
  boundary.
- `buffered_page_over_tenant_byte_limit_fails_the_cursor`, spill lifecycle
  tests, and the result-window tests prove retained bytes, spill files, and
  active grid rows remain bounded independently of total streamed rows.
- `streamed_results_keep_each_result_set_navigable` proves per-set columns,
  rows, status, and active-tab restoration across a multi-result stream.

## Live engines and workspace gates

- PostgreSQL: 17/17 `live_pg` tests passed against a disposable PostgreSQL
  instance, including multiple result sets, extended native types, schema
  graph, transactions, copy, advisory locks, and cancellation.
- SQL Server: 8/8 `live_mssql` tests passed against a disposable SQL Server
  2022 container, including multiple result sets, typed nulls, schema graph,
  bulk copy, transactions, close-mid-query, and cancellation.
- `cargo fmt --all`, strict all-target workspace clippy, and the full workspace
  test suite are the final repeatable source gates for this milestone.

