# Private benchmark source context

Capture optional workspace/Git observations for benchmark snapshots. This is
client-reported source context, not database environment evidence, execution
provenance or permission to access the referenced workspace. Keep the immutable
measurement report and portable artifact formats unchanged. API/SDK clients may
explicitly supply bounded context when saving; legacy requests and saved payloads
default to no context. Retention policy and attached Profile plans remain separate.

The context contains the Sift instance identifier, workspace identifier/revision,
relative document path and local capture timestamp. An optional cached Git
observation includes its own timestamp and workspace revision, head object ID,
branch, dirty/unknown state and truncation flag. A truncated empty status cannot
claim a clean worktree. Never collect native root paths, remotes, URLs, diffs,
file contents, bind values or credentials. Validate identifiers, paths, Git hashes
and bounded printable branch/instance strings. Deny unknown context fields.

The desktop captures only an explicitly workspace-bound query whose document
exists in the selected workspace snapshot. Capture before opening workload
review; freeze through review, execution, result and later save. Never infer a
source from the workspace selected at save time. Reusable definition runs freeze
the same context while reviewing the exact bound query. Stale results cannot
replace the context of another run; failed runs preserve the previous report's
context. Context lives with its report view, not an unbounded shell history map.

Saving context is opt-in in the private saved-run browser, off by default.
Show the exact captured context and cached-Git timestamps before saving. Vim
`o` toggles inclusion; save uses the frozen candidate and selection. Context is
immutable after save; notes/tags edits preserve it. Existing get/delete and
owner/tenant/revocation boundaries apply. Store all context inside the encrypted
SecretStore payload; SQLite indexes, audit records, list summaries, AI/MCP
measurement projections and portable measurement artifacts omit it. Explicit
private saved-snapshot JSON copy includes context and labels that disclosure.
Display missing context as unavailable; do not synthesize it on reopen.

Acceptance covers optional/legacy decoding, malformed/oversized context,
immutable annotation edits, encrypted storage and reopen, API/SDK ownership and
audit privacy, AI/MCP/artifact exclusion, frozen UI capture across workspace/Git
changes, opt-in saving, old-result rejection and preservation after failed runs.
Run formatting, strict workspace Clippy and the full workspace tests, record
ADR-134 and commit verified work. Native Windows/macOS acceptance is deferred.

## Linux acceptance

Implemented through optional pure serde types, validated encrypted snapshot
storage, the existing audited SDK save path and the Vim Performance/library
workflow. Workspace and cached-Git capture use explicit bound-document/status
fixtures; no fresh Git invocation or database-environment equivalence is claimed.

The file-backed library acceptance proves encrypted context persistence and
reopen, annotation immutability, legacy defaults, invalid-path refusal and
existing owner/membership boundaries. HTTP/SDK acceptance proves typed round
trips, rejection and audit/list redaction. Native SQLite governed-AI and standalone
MCP acceptance prove source context remains outside measurement projections and
portable artifacts. The Vim test freezes a bound source before workspace changes
and dispatches context only after opt-in; the Results state-machine test covers
stale source attachment and preservation of prior context after cancellation.

Passed locally:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`
- `cargo test --workspace --locked --offline`

Editable retention, attached Profile plans, advanced comparisons and native
Windows/macOS acceptance remain separate work in the canonical checklist.
