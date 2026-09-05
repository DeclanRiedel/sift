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
- [ ] Frontend: prepare active filter groups and normalized operands once per
  result-grid refresh. Row evaluation must allocate no temporary groups or
  normalized filter strings, and must short-circuit both group and outer logic.
  Preserve NULL handling, numeric comparisons, empty-group behavior, source row
  identity, stable multi-sort, and Vim controls.
- [ ] Add focused regressions for gate lifetime and grouped grid filtering;
  reuse existing rate-limiter tests for admission semantics.
- [ ] Run `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`,
  and `cargo test --workspace`. Record exact outcomes and any environment limits.
- [ ] Review the final diff and commit completed milestones.

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
