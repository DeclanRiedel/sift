# Backend and frontend cleanup

Scope: an implementation pass over observed hot paths, preserving current
product behavior. No API migration or compatibility work is required.

## Design and milestones

- [x] Inspect architecture, existing decisions, and current feature inventory.
- [x] Record design before implementation and track execution in temporary JSON
  outside the repository. Commit this plan, then each implementation milestone.
- [x] Backend: make schema fetch gates release through ownership. A gate must
  stay discoverable while any caller holds it, including queued callers after
  a provider error, reconnect failure, cancellation, or an early cache hit.
  Remove manual cleanup, which can let a new caller bypass existing waiters.
- [x] Backend: use fixed-size principal/tenant admission candidates, preserving
  atomic charging and retry timing without per-request temporary heap vectors.
- [x] Frontend: prepare active filter groups and normalized operands once per
  result-grid refresh. Row evaluation must allocate no temporary groups or
  normalized filter strings, and must short-circuit both group and outer logic.
  Preserve NULL handling, numeric comparisons, empty-group behavior, source row
  identity, stable multi-sort, and Vim controls.
- [x] Add focused regressions for gate lifetime and grouped grid filtering;
  reuse existing rate-limiter tests for admission semantics.
- [x] Reconcile the result-copy feature inventory with existing desktop command
  routes and format coverage: CSV, JSON, SQL, and Markdown are implemented.
- [x] Run `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`,
  and `cargo test --workspace`. Record exact outcomes and any environment limits.
- [x] Review the final diff and commit completed milestones.

## Evidence and limits

Initial inspection found manual fetch-gate removal while other callers can still
hold the mutex, two temporary vectors inside rate admission, and per-row filter
group vectors plus repeated operand normalization in the result grid. These are
structural improvements; no wall-clock speedup is claimed without measurements.
Existing operations and audit boundaries remain the product entry points.

This pass introduces internal prepared-filter machinery, not an unrelated new
product surface. Broader feature work remains in the canonical feature inventory.

Backend validation: focused schema-cache and rate-admission tests passed.
Workspace Clippy passed with warnings denied. Gate cleanup holds the map lock
while releasing the caller's reference, so concurrent final drops cannot leave
an idle entry behind; the workspace run includes that regression as well.

Frontend implementation extracts prepared predicates into `results/filter.rs`.
For R rows, G groups, and F active filters, group membership preparation moves
from repeated per-row scans (O(R * G * F)) to one pass (O(G + F)); row matching
visits only relevant predicates and short-circuits. Operand strings and numeric
filter values are prepared once. Result indices and sorting remain unchanged.

Final validation (2026-09-05): `cargo fmt` and workspace Clippy with warnings
denied passed. The initial workspace test build could not link the desktop
because this host lacks the development `libxkbcommon-x11.so` alias. Its runtime
`libxkbcommon-x11.so.0` is installed; providing an alias in the temporary tracking
directory and running `LIBRARY_PATH=<temporary-directory> cargo test --workspace`
passed (exit 0), including all 404 workspace-UI tests. No repository build
configuration or system libraries were changed. Existing ignored tests remain
ignored; this run does not claim live database integration or measured latency.

Milestones: plan `050f4ed`, backend `333850f`, followed by the frontend/filter
and validation commit containing this completed checklist.
