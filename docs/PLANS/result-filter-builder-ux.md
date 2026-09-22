# Result filter builder redesign

Status: design proposal; not implemented. Scope: result-grid filtering, not the
Objects type toggles or global search. Preserve the retained grid renderer.

## Direction

Use a compact, inline panel above the grid. Keep conditions visible together,
with column, operator, and value on one row. Avoid cycling through operators or
switching between columns to understand a filter.

Reference: [Navicat Filter Wizard](https://www.navicat.com/manual/pdf_manual/en/navicat_16/linux_manual/navicat_en.pdf)
uses an above-grid panel, enabled conditions, groups, and explicit application.
Borrow those interaction patterns, not its visual styling. Use Sift's icons,
spacing, keycaps, focus outlines, and Vim interaction model.

## Proposed layout

```text
Filter       Scope: [Loaded rows v]      Match [All v] groups       Close
  Match [All v] conditions
  [x] [status      v] [equals       v] [paid                  ] [×]
  [x] [total       v] [greater than v] [100                   ] [×]
      + Condition     + Group
  Draft changes                                      Cancel   Apply
```

After Apply, collapse to a compact summary: `status = paid AND total > 100`,
scope, matched/loaded count, Edit, and Clear. Keep the active filter indicator
on relevant column headers. Header filter clicks open the existing draft focused
on that column; if no condition exists, add one. Sorting stays independent:
header clicks toggle direction, advanced sorting controls order and clearing.

## Interaction and safety

- Draft and applied state are separate. Typing never reruns a query or scans
  every loaded row. Apply validates then evaluates once; Cancel restores the
  applied configuration. Clear is an explicit action, not an empty-value hack.
- Column and operator pickers are searchable. Offer type-compatible operators.
  Null checks hide the value input. Empty string is a valid explicit value and
  must differ from a missing value. Invalid drafts remain visible with inline
  feedback, not repeated toasts.
- Enable/disable conditions without losing their values. Allow multiple
  conditions on one column, such as a lower and upper bound.
- All/Any logic is explicit both between groups and inside each group. Show
  grouping with modest indentation and a border, never color alone.
- Scope defaults to Loaded rows, showing `N of M loaded` and a partial-result
  warning when more rows exist. Switching to Database query changes Apply to
  `Run filtered query`; show connection/database context and a read-only SQL
  preview. Do not silently send loaded-row edits to the server.
- Database scope is enabled only where the existing execution path supports
  transforms. Preserve staged-edit guards, cancellation, timeouts, authorization,
  and audited operations. Bind values through existing transform handling;
  never concatenate user-entered values into SQL.
- Keep the previous applied grid during computation. Use revision tokens to
  discard obsolete work. Errors preserve the old result and the editable draft.
- Vim normal mode: j/k move between conditions, h/l move between fields, Enter
  edits/opens a picker, Escape leaves the field before closing the panel.
  Tab/Shift-Tab also traverse controls. Explicit Apply/Cancel buttons remain
  reachable. Background editor focus must not receive keys while editing.
- Fit short forms to content. Cap tall forms at a fraction of result-pane
  height, scrolling only the conditions while keeping scope and Apply visible.
  Narrow layouts wrap value controls below their column/operator, not offscreen.

## Repository constraints and implementation sequence

Current `ResultsView` stores one value/operator/group per column, rebuilds the
loaded-row projection on input changes, and exposes a per-column transform
editor. This cannot express two conditions for the same column. The existing
`ResultTransform` protocol already accepts multiple filters per group and
All/Any at two levels; use that structure without a protocol change. Arbitrary
recursive groups are explicitly out of scope for the first version.

1. Introduce UI-only condition IDs and separate draft/applied group models.
   Convert current filters losslessly; retain result-set-specific state. Do not
   send disabled or incomplete conditions in a transform.
2. Build the inline condition editor and searchable, typed pickers. Keep query
   search/find separate from filtering. Preserve wire column type/nullability
   metadata in the UI model rather than guessing from display type labels.
   Add the compact applied summary.
3. Route loaded-row Apply through prepared predicates. For large retained
   results, evaluate against an immutable snapshot off the UI thread and swap
   the index projection only if the result revision still matches.
4. Connect Database query Apply to the existing audited transform execution.
   Resolve numeric, null, case/collation, and date semantics explicitly; report
   where local semantics cannot match the selected database. Do not promise
   equivalent results unless tested for that type and driver.
5. Measure and verify before retiring the old builder. Saved filter profiles
   can follow separately; no new persistence or server endpoints in this pass.

## Acceptance and performance gates

- Tests: two predicates on one column; mixed All/Any groups; disabled conditions;
  null versus empty string; typed invalid values; Cancel; scope switching;
  staged-edit guards; switching result sets during an in-flight Apply.
- Keyboard checks: nested picker Escape, restored grid focus, and background
  editor isolation. Visual checks: narrow pane, long column names, empty state,
  and many conditions. No hidden Apply button or unnecessary outer scrolling.
- Benchmark typing, open/close, Apply, and scrolling with large loaded results.
  Target interactive draw p95 below 8.33 ms on the baseline fixture. Apply may
  take longer but must remain cancellable and not block input. Measure its
  computation time separately from draw time.
- Retain viewport culling and shape caches. Editing a condition must not
  invalidate every grid row. No per-keystroke full-grid scans or network calls.
