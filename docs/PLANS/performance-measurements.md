# Desktop performance measurements

Correction (2026-09-09): the historical `vim_typing_large_document` runs below
did not install the desktop Backspace binding in the standalone GPUI fixture.
They measured insertion with a non-editing Backspace event, not a stable-length
insert/backspace pair. Keep them as historical observations only, not validated
pair timings or an apples-to-apples speedup claim. The fixture now binds Backspace
explicitly and asserts unchanged document length after each pair. The new rapid
completion fixture similarly binds Tab and asserts the resulting SQL text.

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

## SQL editor refinement (2026-09-09)

Repeated the existing 8,000-line `vim_typing_large_document` fixture using
`--profile release-dev`, matching the demo launcher. Ten Criterion samples,
one-second requested warmup and two-second requested measurement; Criterion
extends collection when the workload cannot fit that duration. The workstation
was not isolated, so these are diagnostic measurements, not portable gates.

The historical pre-refinement fixture measured a 7.491-second mean iteration
and 8153.727 ms dirty-to-draw p95. Reusing unchanged wrap ranges and avoiding
whole-buffer Vim snapshots reduced those to 60.329 ms and 58.294 ms respectively.
This exposed another invalidation cost: every edit still discarded unchanged
visible glyph layouts. Those layouts are now retained when text, styling and
visual row placement remain valid.

| Stage | Historical mean iteration (see correction above) | Dirty-to-draw p95 |
|---|---:|---:|
| Pre-refinement | 7490.6 ms | 8153.727 ms |
| Wrap reuse and Vim splice path | 60.329 ms | 58.294 ms |
| Plus unchanged visible glyph reuse | 11.090 ms | 7.590 ms |

The final run recorded 694 frames (including warmup/calibration), p50 4.502 ms,
p99 8.139 ms, and maximum 10.772 ms. Two observed frames exceeded the 8.33 ms
120 Hz CPU-frame budget. Both p95 and p99 fit that budget, but this does not
guarantee 120 Hz presentation on a real display. Criterion reported two high
outliers among ten samples; the final iteration-time confidence interval was
10.914–11.457 ms.

The fixture has no live database or workspace shell. It does not measure server
completion latency, the Problems projection savings, or physical key-to-display
latency. First layout of a large document still requires wrapping the document;
initial opening is not covered by the steady-edit improvement claim. Native
profiling via `perf`/attach was unavailable under the host's security settings;
no host settings were changed.

```sh
cargo bench -p sift-workspace-ui --features benchmark --profile release-dev --bench frame_budget -- vim_typing_large_document --sample-size 10 --warm-up-time 1 --measurement-time 2
```

## Verified rapid completion (2026-09-09)

`vim_rapid_completion_large_document` installs the desktop Tab binding and
asserts that `sel<Tab> * fro<Tab>` produces `SELECT * FROM` before recording a
successful iteration. The fixture has 8,000 SQL lines plus an empty editing line.
Resetting the document occurs outside the measured interval. This measures local
keyword previews and incremental acceptance, without a server or workspace shell.

Under `release-dev`, ten samples measured the complete 11-key sequence at a mean
59.668 ms (95% confidence interval 58.045–61.883 ms). Across 385 observed frames,
dirty-to-draw p50 was 5.435 ms, p95 6.545 ms, p99 7.016 ms and maximum 9.929 ms.
One frame exceeded the 8.33 ms CPU-frame budget. This is an absolute measurement,
not a before/after speedup claim; compositor, GPU and physical display latency
remain excluded. Other desktop applications were running; some checking work
overlapped the run, so these are diagnostic observations, not a portable gate.

```sh
cargo bench -p sift-workspace-ui --features benchmark --profile release-dev --bench frame_budget -- vim_rapid_completion_large_document --sample-size 10 --warm-up-time 1 --measurement-time 2
```

## Quiet deletion and Insert input (2026-09-09)

After `cdc081f`, the corrected `vim_typing_large_document` fixture (explicit
Backspace binding and unchanged-length assertion) measured a mean 13.973 ms per
insert/backspace pair under `release-dev`, with a 95% confidence interval of
13.868–14.080 ms. Across 584 observed CPU frames, dirty-to-draw p50 was 6.971 ms,
p95 7.930 ms, p99 9.519 ms and maximum 12.763 ms; 15 frames exceeded 8.33 ms.
Ten samples used one-second requested warmup and two-second requested measurement.

This validates the actual insert/delete workload, not an end-to-end latency or
speedup claim. An earlier probe and compilation overlapped part of this run;
Criterion's automatic comparison is not a controlled before/after result.
Network, workspace shell, GPU submission and compositor latency remain excluded.
Fixture construction makes wall-clock benchmark execution much longer than the
measured individual edits. Behaviour tests separately cover completion suppression,
Unicode/newline/selection deletion, boundary no-ops, undo and native Replace mode.
