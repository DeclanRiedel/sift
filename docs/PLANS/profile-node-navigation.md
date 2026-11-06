# Profile node inspection and linked observations

Design: preserve the captured plan's preorder node identity in the Performance
Plan view. Each bounded scan/sort/native-spill observation links to its exact
node index and captured run identity, even when operators have identical labels or multiple findings
refer to one node. The truncation notice has no target. Inspection is local to
an already-authorized live or owner-private saved Profile; it performs no new
query, API request or database operation.

Select nodes by clicking a finding/row or using Vim j/k, gg/G. Use n/N to cycle
forward/backward through distinct nodes represented by observations, wrapping
at either end. Bind these keys only in the Performance Plan context outside
text inputs. Selection scrolls the virtualized plan list and reveals wrapped
operator, relation, estimate, actual, row-delta and runtime details. Native loop
semantics and instrumentation/inclusive-counter caveats remain explicit.

Cache findings with each accepted Profile, reset selection/scroll for every new
live or saved Profile, and preserve the existing run-ID refusal of stale replies.
Queued finding/row clicks also recheck the captured run ID before selecting.
No selection from a prior plan may point into a replacement plan. Bound finding
work to 4096 preorder nodes, output to 32 observations including a truncation
notice, and pending traversal memory by depth rather than catalog width. Keep
node identity consistent with the rendered preorder list.

Verify repeated operator names, multiple findings on one node, wide/deep bounded
traversal, keyboard wrap/edge behavior, virtualized selection, exact finding
clicks, replacement resets, stale replies and saved-plan execution-free behavior.
Run formatting, strict workspace all-target Clippy and workspace tests under the
existing 10% Rust/Cargo resource cap. Native GUI acceptance remains separately
scoped in the artifact matrix.

## Verification scope

The focused GPUI scenario uses a 100-child plan with identically named Sort
operators at different preorder indices. It drives j/k, gg/G and n/N through
actual action dispatch, clicks a finding, checks that its distant node becomes
visible in the virtualized list, and inspects selected-node details. It also
covers inactive-section refusal, stale live replies, new-plan reset, and private
saved-plan navigation without starting a workload. A compact-panel wheel event
must make the selected-node details reachable through the Plan content scroller;
the node list retains its own fixed-height virtualized viewport.

The finding fixture covers multiple observations on one node, repeated operator
labels, and 5000-wide and 5000-deep plans with explicit untargeted truncation.
Deep fixture teardown is iterative as well, so recursive model destruction is
not confused with traversal behavior. Existing native-evidence and row-delta
refusal tests remain in place. Native artifact visual evidence still applies to
the exact earlier package/source recorded in the artifact matrix, not this new
source tree.

On 2026-11-06, `cargo fmt --all`, strict workspace/all-target Clippy and the full
locked/offline workspace test suite pass: 1,782 tests, zero failures, six existing
ignored tests. The final run includes compact-pane reachability, captured-run
click refusal and both wide/deep traversal cases. The existing low-memory test
profile overrides retain UI optimization. All Cargo/compiler/test descendants
run with one build job/test thread, single-core affinity, the shared 160% CPU
quota on 16 logical CPUs, 3,287,567,155-byte memory maximum and no swap. Peak
memory is 3,247,796,224 bytes; the completed service exits 0 with no remaining
processes. No native package was regenerated or promoted by these tests.
