# WebSocket query settlement

Status: implemented and validated on Linux. Decision graduated to ADR-156.

## Required behavior

Accepted WebSocket queries need the same typed ExecuteQuery audit invariant as
HTTP. Socket/ack/send failures must not bypass terminal history/ledger settlement.
The upgraded connection task already owns the socket independently of the HTTP
upgrade observer; keep its admitted query settlement alive after network loss.
Bound active query settlement with the existing independent owner semaphore.

Use a shared audit owner that records once and retains actor/correlation. A drop
before a native outcome is observed records an explicitly uncertain failure,
not a rollback claim. HTTP can reuse this owner for its exceptional-drop fallback.
Native diagnostic text can echo SQL/bind values. Persist codes and generic
query error messages, while the authorized wire response keeps native details.
Operation replay omits result transforms because filters contain private values.
Metadata settlement still uses existing bounded writer/failure semantics rather
than claiming a new atomic durable transaction across every record family.

Track native observations separately from delivery errors. A received Done/Error
is recorded before pacing, byte quota, or network sends; losing delivery afterward
cannot overwrite that native result. A stream without a terminal observation is
not successful. Preserve observed row counts, explicit cancellation, native errors,
and uncertain disconnect/transport outcomes. Lost observations and uncertain
execution and cursor/buffer/resource/result-shape failures use the existing
OutcomeUnknown ledger state. Completed
statements within an open transaction remain partial. Share the HTTP uncertainty mapping,
including internal supervision failure.

Watch socket close while awaiting cursor publication, even with observation
timeouts disabled, and control/close while awaiting native pages/progress, so a silent
producer cannot conceal consumer loss. Validate cursor/connection ownership on
cancellation and keep explicit cancel authorization. Bound cancel observation;
safety cleanup uses captured server state and the existing bounded cleanup path.
Never let a wedged cancel freeze the transport task. Explicit cancel requests
retain their CancelQuery audit, separately from the terminal query observation.

Capture one shared shutdown reference into native dispatch. Keep it through raw
EOF after terminal delivery, cancellation, disconnect, or observation timeout.
History/ledger writes and transport settlement also hold that same logical-query
guard; no double counting. ADR-157 adds export terminal observation ownership.

## Evidence and boundaries

Extend existing real WebSocket fixtures for normal completion, cancellation,
invalid acknowledgment, close with a silent native producer, native error, and
known terminal outcome followed by delivery refusal. Verify actor/correlation,
SQL/bind privacy, one terminal query audit, current metadata attribution,
unknown/partial writes, and native shutdown retention through raw EOF. Keep
legacy pages and explicit execution-event modes green; no public wire or locked
Driver signature changes, UI/shared dependency changes, or Standard mode work.

Run format, strict workspace/all-target Clippy and complete workspace tests under
the shared 10% CPU/RAM cap. Native provider certification and Windows/macOS native
acceptance remain separate, with Windows/macOS deferred by the user.

Validation: `cargo fmt`, strict workspace/all-target Clippy, and the complete
workspace suite passed (1,753 tests passed; 6 ignored). All Rust/Cargo work ran
under the shared 10% host CPU/RAM cap. The real-socket fixture covers 11 cases,
including pre-cursor disconnect while native execution remains blocked, cancel
observation timeout, buffer-quota loss and post-terminal delivery refusal.
The audit-drop fixture also verifies that replay excludes result-filter values.
