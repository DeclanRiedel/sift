# Desktop transaction control source binding

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#ai-execution).

Status: implemented and verified; decision graduated to ADR-154.

## Problem and required behavior

Savepoint actions already bind session/connection/transaction/opened-at identity.
Desktop begin/commit/rollback still use unqualified commands and unqualified
transaction-changed replies. Changing connection or instance while work is queued
can redirect a command or apply a late reply to another target.

Freeze instance ID, profile ID, session and connection when a control is requested.
Assign a unique request ID. Begin requires no transaction; end also freezes the
transaction ID and opened-at identity. The serial SDK executor verifies the full
source and expected transaction immediately before dispatch. Wrong or unavailable
sources are refused before HTTP. Never silently retarget a queued command.

A control reply carries the complete original target. Only the matching pending
request for the still-active source may settle UI state, clear savepoints, or
report its outcome. Source changes invalidate pending reply ownership. A successful
begin payload must name the captured connection; end success clears only its
captured transaction. Failed end retains that transaction and existing failed
state; a lost acknowledgment does not prove native rollback or commit.

Connection activation remains a separate, source-bound metadata event. It restores
the parked context's transaction, followed by the existing authoritative snapshot
refresh. Activation must not be mistaken for a queued control acknowledgment.
Query snapshots preserve pending terminal-control ownership; authoritative
transaction absence clears obsolete ownership. Existing
server admission, Operation authorization/audit and protocol shapes stay intact.
Vim commands and footer controls use the same bound path. No Standard mode work.

## Evidence and scope

Extend existing GPUI transaction tests to verify captured sources, stale instance/
profile/session/connection and request IDs, transaction ID/opened-at mismatches,
source switches and preservation of a newer pending request. Extend the existing
SDK transaction fixture to prove refused commands issue no HTTP request, while
matching begin/end commands still dispatch to their exact transaction.

Run formatting, strict workspace/all-target Clippy and the full workspace suite
under the user's shared 10% CPU/RAM cap, with one Cargo job/test thread and the
existing low-memory codegen settings. Native Windows/macOS acceptance remains
deferred. Broader query audit ownership and provider-native certification are
separate work; this change proves desktop command/reply identity.

## Completed evidence

Formatting and strict workspace/all-target Clippy passed. The full workspace
suite passed 1,749 tests with six existing ignored cases. All four GPUI
transaction tests passed, including the new stale-control reply test. The SDK
fixture passed refusal-before-mutation-HTTP, captured begin/commit/rollback IDs
and failed-end retention. Rust/Cargo used one build job and test thread under
a shared 10% host CPU/RAM cap, retaining the existing low-memory codegen
settings. Native Windows/macOS acceptance remains deferred.
