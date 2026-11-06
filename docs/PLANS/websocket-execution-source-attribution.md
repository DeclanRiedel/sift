# WebSocket execution source attribution

Status: implemented and validated on Linux; graduated to ADR-159.

Capture and authorize one connection entry before asynchronous metadata work for
each admitted socket Execute. Derive managed principal, tenant, profile and
configured database context from that entry, not a later vault-profile lookup.
Use the same captured entry in stream execution; the shared generation check and
native gate must refuse replacement before dispatch. Keep current SQL policy,
transaction, cancellation, bounded settlement and raw-EOF ownership behavior.
Configured database context describes connection input, not SQL object access or
the driver's current database after session-changing SQL.

Versioned source references require metadata and a proven managed tenant. Validate
workspace access/revision/checkpoint/Git observations, then require its tenant to
match the captured source. Ad-hoc connections remain usable without claiming a
managed tenant or workspace provenance. Record ordinary preparation refusals as
failed typed query audits, preserving original actor/correlation and privacy;
reserve uncertain observer-loss fallback for exceptional loss. Refusal must occur
before native execute and must not create query-history/change-ledger rows.

Validate optional source-workflow labels against the ledger's existing nonempty,
100-byte limit in the shared metadata validator. This protects both HTTP and socket
execution from applying SQL before discovering the requested ledger annotation
cannot be stored. Retain optional/default workflow behavior and valid labels.

Extend real socket fixtures for valid source, cross-tenant and stale/missing
workspace references, ad-hoc and metadata-free claims, and invalid workflow labels.
Prove refusal before native dispatch, one private audit with original correlation,
and unchanged history/ledger counts. Edit a profile after opening the connection
and retain original configured target across the existing settlement fixture's
completion/error/cancel/disconnect/quota cases. Extend the existing reconnect
fixture to exercise captured stream generation refusal as well as HTTP. Extend
HTTP/metadata acceptance for shared workflow limits.

Run formatting, strict workspace/all-target Clippy and full workspace tests under
the 10% host CPU/RAM cap. No protocol/Driver signature changes or new smoke/CI
infrastructure. Other mutation/export attribution and native provider certification
remain separate; Windows/macOS native acceptance remains deferred.

## Completed evidence

Formatting, strict workspace/all-target Clippy and the complete workspace suite
passed: 1,760 tests passed, six existing cases ignored. Final formatting/lint and
targeted source checks also passed after refining the refusal fixture to permit
an error frame while forbidding Started/rows. All Rust/Cargo ran under the shared
10% host CPU/RAM cap.

The new real-socket fixture covers ten cases: valid managed source, unannotated
ad-hoc query, accessible foreign-tenant source, stale/missing workspace, ad-hoc and
metadata-free source claims, empty/oversized workflow labels and mismatched
variable descriptors. Two succeed with expected metadata; eight refuse before
native execute and without history/change rows. Each produces one private audit
with original actor/correlation; deliberate refusals use failed observation rather
than the exceptional observer-loss code. The actor owns both candidate tenants.

All eleven existing settlement cases now edit the profile after native open and
retain the original configured target through completion, native error, cancel,
close, malformed acknowledgment, quota refusal and pre-cursor observer loss.
Captured HTTP and stream execution both refuse a replaced connection generation.
The HTTP attribution fixture now covers 19 cases, including invalid annotation
labels, and shared metadata checks accept the 100-byte boundary/default while
rejecting empty/oversized labels. Existing transaction, source, HTTP/export/socket
ownership and native-EOF shutdown acceptance remains green.
