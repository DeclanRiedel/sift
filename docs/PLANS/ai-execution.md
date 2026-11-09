# Governed AI execution and unattended runs

Status: design and implementation backlog. Read/Propose remains the shipped
baseline. Execute and unattended operation are not enabled by this document.
The existing Linux provider isolation remains mandatory; native Windows/macOS
acceptance is deferred. This plan covers execution, approvals, durable outcomes
and a companion that can run without an open chat panel.

## Contract and scope

Add an explicit Execute mode, distinct from Read and Propose. Execute includes
normal governed reads/staging, reviewed SQL execution, typed row/migration apply,
reviewed query/document edits and approved governed extension actions. SQL execution must use the existing
ExecuteQuery operation, policies and native settlement paths; it is not limited
to SELECT or translated silently into a proposal. Typed mutations continue through
ApplyEdits/ApplyMigration and their existing native authority/preview checks.
Provider-native shell, filesystem, arbitrary networking and unrelated MCP tools
remain unavailable. Remote source writes need an explicitly supported governed
execution adapter, not a remote annotation or a proposal masquerading as apply.

Execution is disabled by default in instance policy. A permitted human chooses
Execute and the target; model output cannot select another actor or grant itself
an operation. Instance/tenant/profile policy and current user/room membership
intersect on every admission. All existing read-only restrictions remain in force.
Public-room execution requires a separately reviewed write authority; the existing
schema/row publication grants only disclose information and cannot authorize writes.
Read and Propose never acquire apply authority, including through crafted tool
names, arguments, historical leases, external intents or provider configuration.

## Approval and delegation

Interactive execution stages an immutable action, obtains a native preview where
available, and waits for human approval of the exact target, SQL/binds or typed
change, source generation/digest, effects and risks. SQL without a provable affected
row bound says so; a result-row limit is not a write limit. Production confirmation
and migration risk acknowledgements come from the human review endpoint and are
bound to that action. Provider tool arguments cannot supply an approval, manufacture
a production acknowledgement or enlarge an existing grant.

A server-owned approval binds tenant, chat, execution run, initiator, approver,
operation, canonical input fingerprint, target profile/native generation, policy
revision and expiry. Freeze actual database/catalog identity where native evidence
supports it; a configured target alone does not attest current native database state. A different human reviewer must have approval authority;
execution still intersects the initiating user's live rights and uses the initiating
source's credentials. Approval must not replay the reviewer's stronger connection.
Preview/source identity must be proven for that executor, including per-user vaults.
Sensitive bodies use encrypted AI content handles, never
plaintext SQLite fields. Approval consumption is atomic with the immutable dispatch
claim. Duplicate call IDs with identical bindings observe the claim/receipt; changed
bindings refuse. Consumption does not prove execution, rollback or commit.

Unattended runs use a separately reviewed, expiring run delegation with explicit
operation families, targets, production choice, call/time/byte limits and an approval
strategy. Exact preapproved actions and bounded typed changes can be narrower than
arbitrary SQL. If arbitrary SQL is delegated, the review explicitly grants its
potential unbounded affected rows/external effects; no parser/transaction promise
pretends to contain them. Tenant policy can refuse that family entirely. A delegation
cannot exceed the initiating user's current authority or outlive its run. Stopping,
revocation, credential/configuration changes or source replacement close admission.
Already dispatched native work settles independently and is never retried merely
because observation or authorization was lost.

## Ownership and durable outcomes

Before exposing agent apply, make the existing human database apply path retain one
bounded owner through preflight, durable claim, native settlement, ledger, receipt
and one terminal typed audit. Capture the original actor/correlation and propagate
one shared shutdown guard into every native/cleanup owner. Pin the native source
as soon as the review target is resolved, before asynchronous native validation;
a reconnect must not redirect a reviewed action.
Returned-row publication and response authorization are separate from native outcome:
losing delivery cannot turn a known applied receipt into a failed database audit.
Native diagnostics, extensible error text and bind/row values never enter sanitized
audit or ledger error codes. Preparation refusals are audited without dispatch.

Each execution call has a durable dispatch journal with immutable input/grant/source
bindings and explicit not-dispatched, applying, known terminal and outcome-unknown
states. Actual native transaction IDs, counts and commit observations come from
existing settlement receipts. An uncertain commit, expired observer or lost companion
never becomes a retry. Recovery interrupts live runs, preserves claims/receipts and
requires reconciliation before any new action can reuse an affected workflow.
Migration workflow run IDs are not native transaction IDs. Grouped migrations need
per-group native transaction observations; a run summary cannot invent one transaction
identity for multiple groups. Native count aggregation must use checked per-kind
arithmetic and leave overflow/unrepresentable counts unavailable; the legacy AI
ledger aggregation must be reconciled with those observations. The existing migration-run journal and legacy ledger
fields need this reconciliation before claiming complete Execute provenance.
Author, approver and delegated executor provenance remain distinct; an agent is not
a stronger principal and model text is never an execution receipt.

Stop and run deadlines close new admission, request native cancellation and preserve
native EOF/transaction cleanup ownership. They cannot promise rollback or that native
work ended at the client deadline. Mutation calls are serialized per run until their
native settlement permits reuse; read concurrency shares existing global/run limits.
All calls count toward the same harness budgets and preserve ordered activity events.

## Companion, unattended lifecycle and Vim

Keep model credentials on the initiating machine. Extend the isolated Linux desktop
companion into an explicit headless lifecycle that can supervise an authorized run
while the chat panel is closed. Do not introduce server-stored provider credentials
or unrestricted provider-native tools. Scheduled entry uses existing scheduled-task
identity/policy checks and a fresh authorized run lease; expired delegations or unknown
prior writes prevent automatic continuation. Provider/companion loss becomes visible
interruption. Start replay cannot create a second provider turn or native dispatch.

Vim exposes mode/delegation selection, complete action/production review, waiting
approval state, active/queued calls, stop and durable receipt inspection. Source or
input changes invalidate the review. Reopening shows pending/unknown outcomes without
a hidden resend. Public activity/result delivery rechecks disclosure rules; durable
outcome metadata survives loss of access without revealing private returned values.

## Implementation and acceptance

- [~] Own existing human AI database apply through native/receipt/audit settlement.
      Bounded ownership, captured source/actor/correlation, separate delivery checks,
      native EOF tracking and sanitized codes are implemented (ADR-187). Gated caller
      loss, pre-cursor timeout, native-error/replay and room revocation during commit
      acknowledgement tests pass; native row/migration fixtures pass on all three
      engines. Formatting, strict Clippy and all package tests pass. Broader
      pending-dispatch revocation/reconnect cases remain open before full Execute
      acceptance.
- [ ] Add opt-in execution policy, pure serde mode/action/grant/journal contracts,
      encrypted content and recovery/migration behavior without changing Driver APIs.
- [ ] Implement human approval/revocation and atomic grant/dispatch claims with live
      policy/source checks and durable truthful replay/unknown states.
- [ ] Expose reviewed SQL, typed database apply, revision-bound query/document edits
      and governed extension execution via server/SDK and all three isolated provider
      bridges; keep Read/Propose unchanged. Private editor changes require their live
      companion target; authoritative room text uses the existing CRDT actor.
- [ ] Add source-bound Vim review, production/risks, pending approval, stop, receipts
      and activity supervision; agent output cannot impersonate a confirmation.
- [ ] Implement reviewed unattended delegation, headless companion and scheduled
      lifecycle, with no automatic retry of uncertain native writes.
- [ ] Verify native PostgreSQL/SQL Server/SQLite execution and denied/stale/cancelled/
      caller-loss/revoked cases, concurrent immutable calls, public/private disclosure,
      restart/recovery, quotas and each provider's restricted live tool roundtrip.
- [ ] Run formatting, strict Clippy and all package tests under the persistent 10%
      Rust/Cargo cap; commit coherent verified milestones and graduate stable ADRs.

Keep CI manual-dispatch-only. Native platform and publisher/signing prerequisites
remain explicit in their existing acceptance matrices rather than inferred from
Linux unit tests or a provider's own approval settings.


## Human apply ownership milestone (ADR-187)

The existing HTTP/SDK human apply path now owns asynchronous preflight through
ledger/receipt/terminal audit under the shared settlement bound and shutdown guard.
It pins the native source before catalog validation and awaits claimed work within
that scope. Delivery authorization is separate from native outcome. Four gated
migration scenarios pass: caller loss, timeout before a cursor exists, an
extensible native error and room revocation while commit acknowledgement is held.
They verify original actor/correlation, one terminal AI
audit, tracking after Done/error until raw EOF, no repeated native DDL on exact
replay, and private diagnostics absent from receipts/audit/ledger. Revocation blocks
receipt delivery while preserving the known Applied receipt, original actor and
successful audit for authorized inspection. Existing native
AI acceptance passes on PostgreSQL, SQL Server and SQLite. Formatting and strict
workspace/all-target Clippy pass; all package acceptance passes as recorded below.

This is a required execution foundation. It leaves mode/policy, action/grant/journal,
provider-facing execution, human approval/delegation UI and unattended companion
items above open. Existing migration workflow IDs still require the native-group
identity reconciliation described above; no new transaction provenance is claimed
by this milestone.


### Verification — 2026-11-09

The final gated catalog/migration test passes all four scenarios through the actual
router. Native `ai_engine_acceptance` passes 3/3 with `live-pg,live-mssql` on the
configured local PostgreSQL, SQL Server and SQLite fixtures. All package tests pass
in two groups: server with `live-mssql` (52 groups, 670 passed, 13 ignored), then
workspace excluding server with `sift-server/live-mssql` (47 groups, 1205 passed,
5 ignored). There are zero failures and 18 existing ignored tests; those ignored
cases were not all executed. A combined single workspace test invocation remains
unverified under the cap. Formatting and strict workspace/all-target Clippy with
`sift-server/live-mssql` pass.

The final service retained shared CPUQuota=160% on the 16-logical-CPU host,
MemoryMax=3287567155 bytes and no swap, with one-CPU affinity, one Cargo job and
serialized tests. Peak service memory was 3284152320 bytes. Test package settings
used 8192 server, 256 UI/workspace-UI and 512 desktop codegen units, package test
debug information disabled, and jemalloc
`narenas:1,dirty_decay_ms:0,muzzy_decay_ms:0,retain:false`. Assertions/overflow
checks stayed enabled. All five validated Rust sources remained unchanged through
staging. No public wire/Driver signatures, dependencies, environment variables or
CI workflows changed. The added MockDriver commit gate is test infrastructure for
the permission/commit race. Native Windows/macOS acceptance remains deferred.
