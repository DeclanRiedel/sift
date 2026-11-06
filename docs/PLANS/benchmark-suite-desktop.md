# Desktop benchmark suites

Status: implemented and verified with Linux GPUI acceptance (ADR-165).
Native Windows/macOS acceptance remains deferred.

Extend ADR-164's runner with a command-palette entry and a Vim-only structured
composer. Edit named queries and named parameter cases separately; add/remove
rows, navigate queries/cases, edit multiline SQL and typed Value-array parameters,
and configure the five shared benchmark limits. Show explicit serial ordering,
case counts and shared-state/side-effect limits. Do not require a raw suite JSON
document. The existing typed parameter wire values remain explicit in case fields.

Opening, editing or reviewing never executes SQL. Preview validates the complete
suite and freezes every query/case/binding and budget; a distinct x confirmation
is required. Enter never confirms. Bind draft/review to original instance, tenant,
profile, query tab, native session/connection and connection generation. Capture
the original editor statement as an anchor, independently of editable suite SQL;
changed editor intent before confirmation or changed source refuses dispatch.
Rebuild/compare the complete draft at confirmation so hidden/programmatic field
changes cannot execute an obsolete review. Existing operation availability and
current server policy remain mandatory. Desktop dispatch repeats captured source
matching before calling the API.

Use the existing progress/cancellation transport, registration and Performance
panel pending state. Closing the composer does not cancel an active suite or drop
its result owner. The composer can reopen for the original tab while execution
runs; late results must match the pending run and original source/generation.
Source changes hide private fields/reports and refuse further actions. Retain all
case reports, including unattempted cases after failure. Navigate them explicitly
and open one case at a time in Performance, preserving its suite context. Never
pool populations, silently use another case as baseline, or attach an unrelated
Profile plan/workspace source observation. Existing private save, export and
explicit saved-run baseline controls apply to the selected case; show query/case
labels and incomplete-suite state in Summary. A new suite starts only through an
explicit fresh composer, and cannot overwrite an active benchmark/Profile.

Extend existing GPUI acceptance for add/edit/navigation, review without dispatch,
Enter versus x, changed editor/source/generation and changed drafts, exact frozen
requests, progress/cancellation, duplicate/late replies, case selection and private
report identity. Native runner acceptance remains ADR-164's three-engine fixture.
Run formatting, strict all-target workspace Clippy and workspace tests under the
10% CPU/RAM cap. No new smoke scripts/workflows. Native Windows/macOS acceptance
remains deferred. Independent case limits are implemented in ADR-167; additional
ordering is now implemented in ADR-168.


## Acceptance evidence (2026-11-06)

The focused GPUI acceptance passes real command-mode keystrokes for field entry/
Escape, add/remove/re-add with distinct IDs, multi-query/case drafts, review and
Enter-without-dispatch, and x confirmation of the exact frozen payload. Hidden
parameter edits, changed editor SQL and changed connection generation refuse
confirmation. Captured execution includes original instance/tenant/profile and
native session/connection. Closing/reopening the composer preserves its active
owner; progress acknowledges through the existing bounded transport, and z emits
cancellation for the exact suite run.

Wrong-run replies are ignored; valid replies retain all three independent cases.
A relabelled case fails frozen-request validation. A refused new composer retains
all prior reports. Selecting another case opens its own labeled report in
Performance with no workspace source observation, and a fresh suite rejects a
late result after reconnect without accepting a duplicate older reply. Existing
private-save, paired, Profile, source-context and API/CLI acceptance stay green.
The unchanged native runner retains ADR-164's PostgreSQL/SQL Server/SQLite evidence.

Formatting and strict workspace/all-target Clippy pass. The focused GPUI test
passes, and full workspace tests pass 1,775 with six existing ignores and zero
failures. Rust/Cargo used the shared 160% CPU quota on the 16-logical-CPU host,
one-core affinity, one Cargo job/test thread, no swap and the unchanged
3,287,567,155-byte memory maximum. The final validation service peaked at
3,263,012,864 bytes. Normal UI optimization remained enabled; the four established
large test-package overrides used codegen-units=256 and debug=0. No smoke scripts,
workflows or Driver signatures changed. Independent case budgets now ship in
ADR-167; round-robin and seeded shuffled rounds ship in ADR-168. Native
Windows/macOS acceptance remains deferred.

Suite round-robin and seeded shuffled rounds now extend the original serial
runner/composer in ADR-168 ([design/evidence](benchmark-suite-ordering.md)).
Native Windows/macOS acceptance remains deferred.
