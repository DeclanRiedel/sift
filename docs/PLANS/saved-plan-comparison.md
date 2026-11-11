# Saved Profile plan comparison

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#performance-workbench).

Implemented contract. Compare two private benchmark snapshots' attached
Profile plans without executing work. Keep Profile clocks separate from ordinary
benchmark samples. Require valid attachments and the same engine and analyzed
state. Different SQL, parameters, versions, settings and unproven data/cache
conditions remain explicit compatibility warnings, never a controlled-regression
verdict. Missing native counters remain unavailable. Compare provider-reported
counters as reported; do not normalize loop/rebind semantics or sum node clocks.

The pure core compares normalized operator trees and their engine-specific
attributes; it does not parse raw plans. At each paired parent, align ordered
children by (operator, relation) using LCS only when sibling keys are unique.
Pair unmatched gaps positionally as replacements; excess children are additions
or removals. Moves may therefore appear as removal/addition. These are heuristic
correspondences, never semantic node identity. Duplicate sibling keys use
explicitly labelled positional comparison. Bound each input to existing attachment
limits (4096 nodes/64 levels), all LCS work to one million matrix cells, and output
to at most the sum of input nodes. If alignment would exceed the budget, retain
all changes using labelled positional comparison rather than hiding nodes.

Each row retains A/B structural paths, operator/relation, reported numeric fields
and bounded engine attributes, with added/removed/changed/unchanged status. Child
count changes are visible. Raw plan text, SQL and bind lists stay out of comparison
rows. Aggregate only change counts, never costs/time. Expose captured-at/run IDs
and isolated top-level native/server clocks as separate observations. Incomplete
attachment or cross-engine evidence gives a clear refusal, not an empty diff.

Vim saved-run browsing pins A and opens B as today. An explicit plan-comparison
action freezes both loaded captures, computes on the background executor and
opens a virtualized embedded comparison view. Request/source/record fences reject
late results; Escape cancels pending presentation. j/k navigate nodes, Enter inspects bounded selected-node details, y copies
private comparison JSON with a clear identifier/attribute warning, and Escape
returns to the saved-run browser. Scope checks fence server/tenant changes.
No dispatch or query execution occurs. A changed baseline/detail clears cached
comparison; annotations and retention cannot leave a stale comparison presented
as current. CLI comparison adds a separate explicit plan-output option; default
measurement-only output and AI/MCP views remain unchanged. The two existing
audited private snapshot reads cover CLI comparison authorization.

Acceptance covers identical/replaced/inserted/removed/reordered operators,
ambiguous siblings, global work budget, exact paths, missing/zero counters,
provider attributes, unavailable and cross-engine evidence, frozen/stale Vim
state, navigation and execution-free copy, plus explicit CLI opt-in. Run normal
workspace gates, graduate ADR-137, update inventory and commit. Untimed result
comparison and controlled A/B execution remain separate work. Native Windows/macOS
acceptance remains deferred.

## Acceptance — 2026-11-04

Implemented and graduated as ADR-137. Core tests cover identical trees,
insertions/removals with stable aligned paths, replacements, moved siblings,
missing versus zero counters, native attributes, unavailable/cross-engine
captures, duplicate-key fallback, oversized alignment matrices and a global
budget exhausted across separate sibling groups without losing nodes.

The real-file SQLite CLI test proves unchanged measurement-only output, explicit
`--plans` rows, separate threshold output and duplicate-flag refusal. The actual
Vim interaction test covers missing-A refusal, pin/open/compare, row navigation,
selected-node inspection, private JSON copy without raw plans, stale-server
navigation/copy refusal, Escape return, refreshed-record invalidation and no
network/execution dispatch. Comparisons compute on the background executor;
request/source/record guards fence late completion and Escape cancels presentation.

Linux validation passed: `cargo fmt --check`,
`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`, and
`cargo test --workspace --locked --offline` (1,702 tests passed, five ignored).
No protocol, Driver, schema migration, CI workflow or environment-variable change.
The UI links pure core comparison logic; no UI dependency enters shared crates.
Bounded untimed decoded-result verification and controlled paired ordering
are implemented (ADRs 144/182); mathematical/later-data equivalence and
controlled cache/load remain open. Native
Windows/macOS acceptance remains deferred.
