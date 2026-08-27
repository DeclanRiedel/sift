# Desktop performance budgets

Sift targets a 120 Hz Wayland desktop. A responsive frame therefore has 8.3 ms
for all client work, including entity updates, layout, paint, and renderer
submission. The 16.7 ms budget remains the hard fallback for 60 Hz systems.

## Repeatable GPUI benchmarks

Run the headless renderer benchmarks from the reproducible development shell:

```sh
nix develop -c cargo bench -p sift-workspace-ui --bench frame_budget
```

For a quick local comparison while iterating:

```sh
nix develop -c cargo bench -p sift-workspace-ui --bench frame_budget -- --quick
```

The suite uses GPUI's own benchmark context and headless Blade renderer. Frame
reports therefore include entity update, layout, paint, scene construction, and
renderer submission rather than timing isolated model functions.

| Benchmark | Representative fixture | Graduation budget |
| --- | --- | --- |
| `vim_typing_large_document` | 8,000-line SQL document, Vim insert/backspace cycle | p95 draw ≤ 8.3 ms; no p99 draw above 16.7 ms |
| `first_result_page` | 500 rows × 20 typed display cells | p95 draw ≤ 16.7 ms; update-to-frame ≤ 50 ms |
| `retained_grid_navigation` | 10,000 retained rows × 20 columns | p95 draw ≤ 8.3 ms; offscreen cells remain unshaped |

Benchmarks are comparison gates, not portable absolute scores. Record CPU, GPU,
display server, compositor, build revision, and benchmark output when publishing
a baseline. Compare release builds on the same machine. Do not use the ordinary
debug profile for product feel claims.

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
