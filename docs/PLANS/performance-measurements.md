# Desktop performance measurements

Measured 2026-09-06 on Linux x86_64, Intel Core i7-13620H, Rust 1.96.1.
Release profile; existing `frame_budget` harness, 10 Criterion samples per case,
one-second warmup and two-second measurement. GPUI histograms include warmup
and calibration frames. These are local measurements, not cross-platform gates.

## Baseline after Vim/grid changes

Times below are GPUI window dirty-to-draw milliseconds. First-page measurements
start with an available result page; database/network execution time is excluded.
Linux's GPUI benchmark platform uses native text shaping but omits GPU submission,
compositor and physical display latency. The 120 Hz target is 8.33 ms per frame.

| Fixture | p50 ms | p95 ms |
|---|---:|---:|
| `vim_typing_large_document` | 0.702 | 1.109 |
| `first_result_page` | 7.836 | 8.733 |
| `retained_grid_navigation` | 6.488 | 6.771 |
| `result_set_tab_navigation` | 0.633 | 0.673 |
| `git_panel_first_frame` | 3.475 | 5.685 |
| `git_panel_steady_refresh` | 3.469 | 3.662 |
| `command_palette_open` | 2.212 | 2.314 |
| `command_palette_arrow_navigation` | 2.218 | 2.294 |
| `command_palette_filter_typing` | 2.236 | 2.322 |
| `schema_tree_filter` | 33.391 | 34.505 |
| `query_outline_first_frame` | 2.591 | 2.724 |
| `query_outline_navigation` | 2.107 | 2.167 |
| `change_ledger_first_frame` | 2.583 | 2.712 |

Fixtures cover 8,000 SQL lines, a 500-row first page, 10,000 retained result rows,
20,000 Git status entries, 100,000 schema objects, 2,000 outline statements and
4,000 symbols, and 1,000 change-ledger rows. Fixture definitions live in
`crates/workspace-ui/benches/frame_budget.rs`.

## Schema filter improvement

The tree builder previously cloned every object/profile before retaining matches.
Filtering borrowed objects first, reusing search storage, and skipping path
formatting for simple names reduced schema-filter p95 from 34.505 ms to 9.593 ms
(p50 8.765 ms), approximately 72% lower in this fixture. Qualified-name, kind,
and Unicode case-insensitive matching remain supported. No persistent search
index or new cache invalidation rules were added.

The result still exceeds 8.33 ms. First-page paint also exceeded that target in
the baseline. A full performance/memory acceptance milestone remains open.

## Reproduction

```sh
cargo bench -p sift-workspace-ui --features benchmark --bench frame_budget -- --sample-size 10 --warm-up-time 1 --measurement-time 2
```

This machine needed an existing temporary `libxkbcommon-x11.so` linker alias in
`LIBRARY_PATH`; no project linker settings were changed. Runtime memory is
measured separately by running the generated benchmark executable under
`/usr/bin/time -v`, without Cargo compilation.

The direct executable run completed all 13 fixtures in 59.76 seconds with maximum
resident set size 1,023,768 KiB (about 999.8 MiB), no swaps, and exit status 0.
This is the peak of the complete benchmark process, including fixture data,
Criterion and retained platform caches; it is not a steady-state desktop memory
measurement or a per-window ceiling. Further memory acceptance remains open.
