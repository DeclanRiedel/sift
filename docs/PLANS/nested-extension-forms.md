# Bounded nested extension command forms

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#extensions-and-mcp).

Design: extend the existing trusted desktop contribution renderer from flat
scalars to explicit object properties and JSON-array fields. Extensions supply
schemas and data, never UI code. Keep the existing invocation, context resolution,
classification, current authorization, one-use approvals, audit and complete
server-side JSON Schema validation authoritative. No Driver or public wire
signature changes are needed.

Project object groups and scalar leaves in deterministic preorder, with at most
16 controls and four nested object levels. Object controls explicitly select
presence using true/false: required objects start true, optional objects false.
An omitted ancestor suppresses required-child checks, but entered descendant
values must produce an enable-parent error rather than being silently dropped.
Construct objects using property-path components, preserving literal dots,
slashes, tildes and repeated leaf names. Explicit empty objects are representable.

Array fields accept JSON arrays through the host text input. Validate JSON and
array shape locally, cap total raw/encoded arguments at 64 KiB, and reject integer
tokens outside the lossless signed/unsigned 64-bit JSON representation. Finite
floating-point values retain the existing JSON/f64 contract. The server validates
item shapes and every additional schema constraint. References/composition and
other unsupported property types must not be guessed into different shapes.

Password/writeOnly annotations propagate through containers. Aggregate JSON fields
with sensitive descendants or schema references are conservatively masked; do not
resolve references or load schemas over a network. Bound sensitivity scanning and
mask when its inspection budget cannot establish absence. Defaults containing
values are not applied. Optional blank scalars stay omitted; required empty strings
are sent as strings so the server can enforce minLength/pattern accurately.
Never put entered values in labels, errors or presentation state.

Retain Vim selection, field navigation, exact frozen approval/invocation arguments,
and stale-generation/context refusals. Unsupported or oversized forms must fail
closed before dispatch. Provide a scrollable field region for the bounded controls.
Verify optional/required groups, empty objects/strings, literal property paths,
array shape/numbers, inactive descendants, masking and work/byte limits, plus
actual Vim editing and frozen nested dispatch/approval behavior. Run formatting,
strict workspace Clippy and workspace tests under the existing 10% Rust/Cargo cap,
then update extension docs/inventory, record an ADR and commit. Broader runtime
families and provider-native quality graduation remain separate work.


## Verification (2026-11-06)

Implemented in the host form projection and contribution dialog; no server,
protocol, Driver, dependency or CI changes. Run and approval review now share the
same availability, size and argument construction checks. Approved Run also
compares the complete current typed request with the frozen request. Shift-Tab
is explicitly admitted through the modifier guard, and field traversal reveals
the selected row in the scroll region.

Eight new behavior tests cover nested reconstruction, literal property names,
optional ancestors and required descendants, explicit empty objects and strings,
array shape and exact integer tokens, conservative sensitive-schema masking,
control/depth/raw/encoded byte limits, actual Vim typing and reverse traversal,
last-control visibility, unsupported/oversized dispatch refusal, correction and
retry, exact nested invocation, and frozen approval arguments. The focused
extension selection passed all 13 tests.

Final validation passed with Rust 1.96.1, locked/offline dependencies and
`sift-server/live-mssql` enabled: formatting check, strict all-target workspace
Clippy, and the complete workspace tests including enabled native SQL Server
integration tests. The workspace run passed 1,806 tests, with zero failures and
six existing ignored tests. No native desktop artifact was regenerated or
accepted by these tests. Windows/macOS acceptance remains deferred.

All Rust/Cargo work ran under the shared `app-sift-rust.slice` with CPUQuota=160%
on this 16-logical-CPU host, MemoryMax=3,287,567,155 bytes and no swap. Cargo used
one build job, one CPU affinity, nonincremental compilation and one test thread.
Test codegen units were 512 for server/desktop and 256 for UI/workspace-UI, with
package test debug=0 and the normal UI optimization retained. The final service
exited successfully with status 0 and a memory peak of 3,262,902,272 bytes. Final
phase logs are retained in `~/.cache/sift-goal-logs/sift-nested-verified-{fmt,clippy,workspace}.log`;
the focused test log is `sift-nested-verified-extension-ui.log` in that directory.
