# Desktop performance budgets

> These are engineering targets, not release criteria or evidence that Sift is
> near a release. Current recorded fixtures include substantial failures. The
> word "graduation" below means passing one benchmark budget only.

Sift targets a 120 Hz Wayland desktop. A responsive frame therefore has 8.3 ms
for all client work, including entity updates, layout, paint, and renderer
submission. The 16.7 ms budget remains the hard fallback for 60 Hz systems.

## Repeatable GPUI benchmarks

Run the headless renderer benchmarks from the reproducible development shell:

```sh
nix develop -c cargo bench -p sift-workspace-ui --features benchmark --bench frame_budget
```

For a quick local comparison while iterating:

```sh
nix develop -c cargo bench -p sift-workspace-ui --features benchmark --bench frame_budget -- --quick
```

The suite uses GPUI's own benchmark context and headless Blade renderer. Frame
reports therefore include entity update, layout, paint, scene construction, and
renderer submission rather than timing isolated model functions.

| Benchmark | Representative fixture | Graduation budget |
| --- | --- | --- |
| `vim_typing_large_document` | 8,000-line SQL document, Vim insert/backspace cycle | p95 draw ≤ 8.3 ms; no p99 draw above 16.7 ms |
| `editor_scroll_large_document` | sub-row scrolling through 8,000 SQL lines | p95 draw ≤ 8.3 ms; zero steady-state budget overruns |
| `first_result_page` | 500 rows × 20 typed display cells | p95 draw ≤ 16.7 ms; update-to-frame ≤ 50 ms |
| `retained_grid_navigation` | 10,000 retained rows × 20 columns | p95 draw ≤ 8.3 ms; offscreen cells remain unshaped |
| `git_panel_first_frame` | 20,000 changed paths (adapter response cap) | record first materialization separately from steady refresh |
| `git_panel_steady_refresh` | unchanged 20,000-path status | unchanged identities reuse flattened rows and diff metadata |
| `command_palette_open` | full command registry, empty filter | p95 draw ≤ 8.3 ms |
| `command_palette_arrow_navigation` | up/down through the filtered registry | p95 draw ≤ 8.3 ms; selection must not rebuild the registry |
| `command_palette_filter_typing` | five-keystroke prefix over the registry | p95 draw ≤ 8.3 ms |
| `query_outline_first_frame` | 2,000 statements + 4,000 symbols | record first materialization separately from navigation |
| `query_outline_navigation` | j/k through the same outline | p95 draw ≤ 8.3 ms |
| `change_ledger_first_frame` | 1,000 ledger entries | p95 draw ≤ 16.7 ms |
| `result_set_tab_navigation` | switch among eight visible retained result sets | p95 draw ≤ 8.3 ms; inactive cells remain unshaped |

Benchmarks are comparison gates, not portable absolute scores. Record CPU, GPU,
display server, compositor, build revision, and benchmark output when publishing
a baseline. Compare release builds on the same machine. Do not use the ordinary
debug profile for product feel claims.

### Captured runs and local comparisons

Use the performance wrapper for repeatable `release-dev` runs:

```sh
nix develop --command ./scripts/performance.sh run
nix develop --command ./scripts/performance.sh run editor_scroll_large_document
```

Each run writes an untracked directory below `target/performance/` containing:

- `metadata.txt` — commit, dirty state, toolchain, OS, CPU and optional display Hz;
- `command.txt` — exact command;
- `benchmark.log` — Criterion output and GPUI frame histograms.

Set `SIFT_PERF_DISPLAY_HZ=120` when display context matters. Sample count,
warmup and measurement time are configurable through `SIFT_PERF_SAMPLES`,
`SIFT_PERF_WARMUP` and `SIFT_PERF_MEASUREMENT`.

Create and compare a local Criterion baseline:

```sh
nix develop --command ./scripts/performance.sh baseline editor-scroll editor_scroll_large_document
# change code
nix develop --command ./scripts/performance.sh compare editor-scroll editor_scroll_large_document
```

Compare only on the same machine, power mode, toolchain and build profile.
Criterion state stays below `target/`; controlled conclusions worth retaining
belong in `docs/PLANS/performance-measurements.md` with hardware, commit,
fixture, profile, sample settings and limitations. Track iteration confidence
interval, dirty-to-draw p50/p95/p99/max, budget overruns, invalidations per frame
and peak resident memory when relevant. Tail latency matters more than mean.

### Initial headless baseline

Quick Criterion run on 2026-08-27, parent revision `b9839ad`, 13th Gen Intel
Core i7-13620H, GPUI headless Blade renderer:

| Benchmark | Criterion interval | Draw p50 / p95 | 120 Hz result |
| --- | --- | --- | --- |
| Vim typing | 21.66–22.78 ms | 10.31 / 12.25 ms | misses budget |
| First result page | 671.09–713.67 ms | 580.91 / 1055.92 ms | severe hitch |
| Retained-grid navigation | 106.06–166.05 ms | 109.84 / 261.75 ms | severe hitch |

This baseline intentionally records current failure. It gives the incremental
editor and result-projection branches a reproducible before measurement. The
quick run includes Criterion warm-up/calibration frames; use the full command
for graduation evidence.

### Incremental-path baseline

Quick Criterion run on 2026-08-27 after the incremental editor indexes and
background result-cell preparation were merged:

| Benchmark | Draw p95 | Change from initial | 120 Hz result |
| --- | --- | --- | --- |
| Vim typing | 5.45 ms | 55% lower | passes |
| First result page | 159.78 ms | 85% lower | still misses |
| Retained-grid navigation | 75.37 ms | 71% lower | still misses |

The first-result benchmark now constructs `PreparedResultPage` values before
entering GPUI, matching the production executor boundary. Formatting time is
therefore excluded from the UI-thread measurement. M3 performance is not yet
graduated: typing meets its draw budget, while grid construction and navigation
still require a custom paint/layout path or equivalent measured reduction.

### Git integration G11 baseline

Measured 2026-08-28 immediately before the G11 commit, on Linux 6.17.0 x86_64,
Git 2.51.0, and 16 logical CPUs. The status fixture used the adapter's exact
hardened command shape (`--porcelain=v2 --branch -z --untracked-files=all`)
with hooks, credential helpers, fsmonitor, external diff, and pager disabled.
Each repository contained clean tracked SQL files; steady values are the mean
of 20 runs after one warm-up.

| Tracked paths | First observed status | Steady mean |
| ---: | ---: | ---: |
| 1,000 | <10 ms (timer resolution) | 3.324 ms |
| 10,000 | <10 ms (timer resolution) | 8.934 ms |
| 100,000 | 50 ms | 57.139 ms |

The renderer fixture is reproducible with:

```sh
nix develop -c cargo bench -p sift-workspace-ui --features benchmark \
  --bench frame_budget -- --quick git_panel
```

It uses the server's 20,000-entry response ceiling. The initial measurement was
149.815 ms p95 draw for first materialization and 173.933 ms p95 for an
unchanged refresh. Identity-aware row/diff reuse and removal of per-frame path
vector allocations reduced the latter to 61.669 ms p95 (64.5% lower). This is
a measured baseline, not a 120 Hz graduation: first loading and rendering a
maximal status still needs background row preparation or a custom layout path.

## Interactive large-schema fixture

The schema tree also needs a real compositor because the important path includes
pointer hover, scroll presentation, and the window's present loop:

1. Run `nix run .#sift-desktop-demo` under Wayland.
2. Load a catalog containing at least 2,000 objects and expand all groups.
3. Toggle frame metrics with `Ctrl+Alt+Shift+P`.
4. Type continuously, move the caret, scroll the schema tree, and stream a
   500×20 result page.
5. Capture draw/present p50, p95, p99 and counts above 8.3/16.7 ms.

Graduation requires p95 draw below 8.3 ms during typing and schema scrolling,
with no repeated 16.7 ms misses. First-result application may use one 16.7 ms
frame but must not block later input frames.

## Memory fixture

Measure resident set size after warm-up, after an 8,000-line editor, after a
10,000×20 retained result, and after closing both tabs. Product data must remain
bounded by the documented result window; closing tabs must release their row and
shape caches. Publish steady-state and post-close RSS deltas with the frame
baseline instead of committing machine-specific values here.

### UI component survey

Quick Criterion run on 2026-08-29, revision `ba63253`, 13th Gen Intel Core
i7-13620H, GPUI headless Blade renderer. Ranked slowest first by draw p95.
Quick mode includes Criterion warm-up and calibration frames and takes as few
as one sample per benchmark, so treat these as a *relative* ranking, not
absolute per-interaction latency. Use the full command for graduation evidence.

| Benchmark | Draw p50 | Draw p95 | Budget overruns | Invalidations/frame |
| --- | --- | --- | --- | --- |
| `change_ledger_first_frame` | 7545.55 ms | 7545.55 ms | 905 | 2.00 |
| `first_result_page` | 374.60 ms | 385.35 ms | 134 | 3.00 |
| `retained_grid_navigation` | 186.12 ms | 187.30 ms | 66 | 1.00 |
| `git_panel_first_frame` | 164.89 ms | 167.38 ms | 58 | 1.00 |
| `git_panel_steady_refresh` | 163.71 ms | 165.94 ms | 57 | 1.00 |
| `query_outline_first_frame` | 146.93 ms | 161.61 ms | 121 | 1.00 |
| `query_outline_navigation` | 143.26 ms | 146.28 ms | 51 | 1.00 |
| `command_palette_filter_typing` | 91.36 ms | 94.90 ms | 31 | 3.00 |
| `command_palette_open` | 85.52 ms | 86.25 ms | 29 | 3.00 |
| `command_palette_arrow_navigation` | 79.43 ms | 79.82 ms | 27 | 1.00 |
| `vim_typing_large_document` | 9.86 ms | 12.93 ms | 6 | 1.50 |

Two findings are structural rather than a matter of degree:

- **The change ledger list is not virtualized.** Every other large list in the
  shell renders through `uniform_list`; the ledger modal builds all rows eagerly
  with `.children(rows)` inside a plain `overflow_y_scroll` container, so a
  1,000-row ledger lays out 1,000 rows per frame. This is the single largest
  number in the table by two orders of magnitude.
- **Command palette selection rebuilds the command registry.** `palette_down`
  calls `filtered_commands`, which rebuilds every `CommandSpec` — including each
  command's disabled-reason evaluation — solely to clamp the selection index
  against the list length. Arrow navigation should read a cached count.

Outline navigation is comparable to outline first paint, which points the same
way: `filtered_query_outline_entries` is recomputed per keystroke and again per
`uniform_list` batch.

## Profiling workflow

Use four layers:

1. Reproduce one user-visible slowdown in `release-dev`.
2. Measure a stable fixture and retain its raw report.
3. Sample CPU stacks to locate expensive work.
4. Add narrow tracing only when sampling cannot explain tail latency.

`release-dev` keeps line-table symbols with optimized code and avoids release
LTO, making it the default for feel tests and profiles. Record document size,
row/column count, open-tab count, connection type, display refresh rate and
competing heavy processes. “Hold Backspace in an 8,000-line SQL buffer” is an
actionable reproduction; “editor feels slow” is not yet one.

### CPU flamecharts

[Samply](https://github.com/mstange/samply) provides interactive flamecharts
with local symbols. Install it, then profile the desktop:

```sh
nix develop --command ./scripts/performance.sh profile
```

Reproduce the interaction, stop recording, then inspect the main thread. Start
with wide stacks and repeated leaf work. Check allocations, text shaping,
layout, paint, serialization and lock contention before adding instrumentation.
Profiles may contain SQL, file paths, connection names or other private runtime
data; inspect before sharing and never commit captures.

For server-only CPU work:

```sh
nix develop --command cargo build -p sift-server --profile release-dev
nix develop --command samply record target/release-dev/sift-server
```

### Tracing and async work

Use existing `tracing` spans for scheduling, waiting and rare tail latency.
Prefer `#[tracing::instrument(skip_all)]`; add only bounded, non-secret
identifiers or counts. Never record SQL, credentials, secret bytes, cell values
or complete connection configuration.

Server logging accepts `RUST_LOG` or `SIFT_LOG__FILTER`. HTTP request spans can
be exported with `SIFT_LOG__OTLP_ENDPOINT`; `docs/BACKEND-OPERATIONS.md`
documents the bounded best-effort exporter. Traces support diagnosis, not audit
or benchmark truth.

### Memory capture

Measure a built process, not Cargo compilation:

```sh
nix develop --command cargo build -p sift-desktop --profile release-dev
/usr/bin/time -v target/release-dev/sift-desktop
```

Warm the exact workload, record steady and peak resident memory, then close
tabs/connections and verify resources return toward baseline. Separate fixture
payload memory from leaks. Stress many query tabs, large schema trees, retained
result windows, repeated connection cycles and large repository status sets.

## Adding a performance fixture

A useful fixture reproduces a real interaction, fixes input size, asserts that
the operation happened, keeps setup outside measurement, exercises the rendered
tree for UI feel, and remains bounded enough for repetition. UI fixtures report
the 120 Hz budget with `#[gpui::bench(fps = 120)]`. Avoid network/database
variance unless it is the subject. Add behavior tests separately: benchmarks
detect cost regressions; tests protect correctness.
