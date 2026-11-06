# HTTP execution source attribution

Status: implemented and validated on Linux; graduated to ADR-158.

## Required behavior

HTTP ExecuteQuery history and change-ledger fields must describe the connection
selected by the server. A caller-supplied profile ID is an assertion, never an
attribution override. Capture the direct session connection before asynchronous
metadata work; derive principal, tenant, profile and configured database target
from that captured entry. Managed connections receive metadata even without
profile/room hints. Ad-hoc connections retain private history without claiming a
managed profile or tenant. Refuse profile claims on ad-hoc connections, mismatched
managed claims and source references whose tenant cannot be proven.

Validate a supplied versioned source even if room/profile hints are absent.
Commit/checkpoint inference must use real repository columns with deterministic
ordering and match the requested workspace revision. Never infer an older revision's
checkpoint/author onto a current workspace observation.
Require its workspace tenant to match the captured managed source; room-routed
requests additionally require the same room. A room profile assertion must match
the server binding. Refuse caller-session transaction references on shared-room
execution, which uses another session/connection. Preserve existing room-member,
vault, SQL policy and native execution authorization checks.

Pass the captured direct source into execution rather than looking it up afresh.
Recheck connection-generation identity before admission, then use the existing
native owner/gate checks at dispatch. Close/reconnect during metadata preparation
must fail before native execution rather than silently changing attribution.
Configured database target comes from the connection input, not an edited vault
profile. This identifies configured connection context, not every database/object
an arbitrary SQL statement can access or the driver's current database after SQL.

Shared-room caching must match the requested binder/tenant/profile/provider and
captured profile configuration. Reopen stale cached sources and refuse a newly
opened source if its configuration changed during preparation. Capture the actual
selected entry and use the same native generation check. This prevents a room
query being described by a new binding while using an old cached native source.
Room binding/policy changes are checked at admission; no atomic transaction across
metadata and a remote database, or continuing authority after dispatch, is claimed.

Create the existing query audit owner before preparation/admission. Refused claims
and preparation cancellation still produce one failed typed query audit with the
original principal/correlation and private diagnostics. Preserve owned settlement,
raw-EOF shutdown tracking and SQL privacy from ADR-155/156.

## Acceptance

Extend existing HTTP/room/shutdown fixtures: managed attribution without hints,
matching and mismatched profile assertions (including another tenant accessible
to the same actor), ad-hoc claims, metadata-free claims, source-only stale and
cross-tenant references, same-tenant different-room references, room profile/tx
refusal, cached room source changes and direct source replacement before dispatch.
Prove invalid claims never invoke execute or enter history/change ledger. Prove
successful entries use the captured profile/tenant/configured database despite
later profile configuration edits. Keep metadata-free direct queries and existing
room routing/owned settlement acceptance green.

Run formatting, strict workspace/all-target Clippy and the complete workspace
suite under the shared 10% host CPU/RAM cap. Public protocol and locked Driver
signatures stay unchanged. Other mutation/export attribution and provider-native
certification remain separate; Windows/macOS acceptance remains deferred.

## Completed evidence

Formatting, strict workspace/all-target Clippy and the complete workspace suite
passed: 1,759 tests passed, six existing cases ignored. All Rust/Cargo ran under
the shared 10% host CPU/RAM cap.

The existing API fixture now covers 17 attribution cases. Seven successful paths
check actual profile/tenant/configured target and private history; ten rejected
claims check no native execute and no history/change rows. Original actor and
correlation survive on one typed audit per request. The same actor owns both
candidate tenants. Direct connection input stays authoritative after its profile
configuration changes. A retained room cache must open a fresh native connection
on rebind or configuration edits, checked through driver open invocations.

A metadata-free fixture rejects profile/room/source claims before execute while
allowing ordinary personal-mode queries. A session fixture captures a source,
reconnects it, and proves generation refusal before native dispatch. Existing room
history, metadata-free execution, HTTP/WebSocket/export ownership and quota tests
remain green. The HTTP caller-loss fixture now uses actual managed provenance;
its quota admits Native Done while refusing the larger JSON response.

The first valid versioned-source request exposed the nonexistent repository_commit.id
ordering column. A metadata fixture now checks empty inference, deterministic
real-column timestamp ties, correct checkpoint/author mapping and refusal to infer
an older revision's commit/checkpoint/author into a newer workspace observation.
This fix requires no migration or public protocol change.
