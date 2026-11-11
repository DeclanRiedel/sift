# Governed AI execution and unattended runs

Status: interactive human-approved SQL, typed database, query text and governed
extension Execute implemented and verified, opt-in only. Broader execution and unattended operation remain a design/implementation
backlog; this document does not enable them.
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
arithmetic and leave overflow/unrepresentable counts unavailable. ADRs 189/192
supply row and migration journal/ledger observations; subsequent execution families
must reuse those foundations before claiming complete Execute provenance.
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
- [~] Add opt-in execution policy, pure serde mode/action/grant/journal contracts,
      encrypted content and recovery/migration behavior without changing Driver APIs.
- [~] Implement human approval/revocation and atomic grant/dispatch claims with live
      policy/source checks and durable truthful replay/unknown states.
- [~] Expose reviewed SQL, typed database apply, revision-bound query/document edits
      and governed extension execution via server/SDK and all three isolated provider
      bridges; keep Read/Propose unchanged. Private editor changes require their live
      companion target; authoritative room text uses the existing CRDT actor.
- [~] Add source-bound Vim review, production/risks, pending approval, stop, receipts
      and activity supervision; agent output cannot impersonate a confirmation.
- [ ] Implement reviewed unattended delegation, headless companion and scheduled
      lifecycle, with no automatic retry of uncertain native writes.
- [~] Verify native PostgreSQL/SQL Server/SQLite execution and denied/stale/cancelled/
      caller-loss/revoked cases, concurrent immutable calls, public/private disclosure,
      restart/recovery, quotas and each provider's restricted live tool roundtrip.
- [~] Run formatting, strict Clippy and all package tests under the persistent 10%
      Rust/Cargo cap; commit coherent verified milestones and graduate stable ADRs.
      The interactive SQL milestone passes these checks (ADR-188); repeat them
      for the remaining execution families and unattended lifecycle.

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


## Interactive SQL implementation contract

The first execution surface is private-chat SQL on the initiating user's selected
managed connection. Instance policy explicitly enables Execute; existing runs keep
their read/staging base mode and an independent durable execution permission flag.
This avoids rebuilding the referenced ai_run table or rewriting old run identities.
All public readers derive the effective Execute mode from that flag. Read/Propose
never gain it. Public-room writes wait for a separate write-grant contract.

A provider submits one immutable SQL/bind request and receives a pending action.
One atomic journal insertion also reserves the shared tool/event budget; replayed
call IDs bind the original request, never a replacement native source. An encrypted
input pins run/actor/profile/session/connection/native generation/profile digest,
SQL/binds and production label. Human approval echoes its complete fingerprint and
production confirmation. The server rechecks run, source, permissions and absence
of an editor transaction before an atomic one-use Applying claim. Each run admits
one Applying action; an unknown native outcome blocks further execution in that run.

This milestone admits supported single read/DML/DDL statements with known session
autocommit state. Transaction/session control, opaque SQL, public-room execution,
other execution families and unattended delegation remain explicit follow-ups.
Execute SQL is not read-only: INSERT/UPDATE/DELETE and DDL use normal query policy.
Native EOF observations determine committed/unknown effects independently of result
observation, cancellation or result-size failure. No implicit LIMIT is appended.
The same original source is rechecked after native admission waits. Native owners
retain shutdown/quota through EOF; terminal journaling waits for that observation.
Active Applying claims prevent chat deletion, while interrupted claims become
unknown only during offline/startup recovery. All encrypted handles participate in
backup, rotation, retention and cleanup. Provider waiting/polling observes one action
without spending another mutation call or dispatching it again.

### Interactive SQL implementation — verified 2026-11-09

The first execution family now has private opt-in Execute runs; encrypted,
immutable SQL/binds and startup source bindings; a shared-quota request journal;
explicit human approval/denial and production confirmation; exact one-use
claims; normal bounded native execution; independent raw-EOF outcomes; and
unknown-outcome acknowledgment without retry. API/SDK and all three provider
bridges share this path. Vim review remains usable while the provider waits.

Run termination denies unclaimed actions. Applying actions retain their native
owner and deletion protection through terminal settlement. Startup/offline
recovery turns interrupted applies into unknown outcomes. New encrypted handles
participate in cleanup, rotation and coordinated tenant restore. Disclosure
rechecks chat authority after decryption, independently of trusted completion.

This does not finish the checklist above: typed database/document/extension
execution, public-room write grants, explicit approval revocation/delegation,
unattended/headless lifecycle, broader reconnect/revocation/cancellation fault
acceptance and native Windows/macOS isolation remain open. The SQL source label
is configured identity, not an attestation of every accessed native object.
ADR-188 records the stable interactive SQL contract.

Current milestone verification: formatting and strict workspace/all-target Clippy
pass with `sift-server/live-mssql`. The server package passes all 52 test groups:
671 passed, zero failed and 13 ignored. Lifecycle backup/restore fixtures now
expect schema version 58; their existing structured-output and redaction checks
both pass. Workspace excluding server passes all 47 groups: 1208 passed, zero
failed and 5 ignored, including the encrypted SQL journal and human-owned Vim
review tests. Together these cover all packages: 1879 passed and 18 ignored.
The server group used `live-mssql`; the other group used
`sift-server/live-mssql`. At this checkpoint a single combined workspace test invocation remained
unverified under the cap. The prior unchanged native AI fixture passed
PostgreSQL, SQL Server and SQLite (3/3), and the SDK coverage check passed.
Live restricted Execute tool roundtrips for all three provider processes remain
separate acceptance work; shared provider tool wiring alone does not prove them.

The 10% CPU/RAM cap remained enforced: shared CPUQuota=160% on the 16-logical-CPU
host, one-CPU affinity, one Cargo job, serialized tests, MemoryMax=3287567155
bytes and no swap. Final service peak memory was 3284054016 bytes. Server test
compilation used 32768 codegen units and a GNU bfd link with reduced memory
overhead after LLVM linking exceeded the cap; UI/workspace-UI used 256 and desktop
512 codegen units, with test package debug information disabled. Assertions and
overflow checks stayed enabled. All 36 changed source/fixture files retained
their recorded hashes through final staging. Native Windows/macOS acceptance
remains deferred; Driver signatures, dependencies and CI dispatch policy are
unchanged.

### Native provenance foundation before typed delegated apply

At the ADR-189 checkpoint both migration ledger consumers used the workflow
`MigrationRun.id` as `transaction_id`. ADR-192 replaces those conversions with
the same durable native journal projection. Workflow receipts alone remain
insufficient native evidence. The typed execution action family, reviewed grants
and its complete native fault matrix remain separate acceptance work.

The row-edit foundation is implemented and verified (ADR-189). Guarded AI
apply retains the captured managed transaction and confirmed owned rollback,
retained through observer deadlines by the ownership follow-up in ADR-191.
Attempted edit kinds come from the normal execution owner's actual native
admission rather than intent before admission. Direct grid capture uses the same
evidence. AI ledger entries include only those kinds, use checked per-kind signed
count aggregation, preserve known rollback and otherwise leave uncertain effects
explicitly unknown. A confirmed owned rollback produces a failed human proposal
receipt that says it rolled back; it never becomes a new automatic apply.
Tests extend guarded mid-set revocation and native commit/conflict/exact-replay
acceptance, plus zero/overflow count boundaries. Formatting, strict workspace/
all-target Clippy, all 29 focused edit/transfer tests and the extended native AI
fixture on PostgreSQL/SQL Server/SQLite (3/3) pass for the boxed orchestration.
The boxed poll boundary limits the large future embedded in HTTP/AI callers. Ledger
transaction identity is the numeric managed `TxId`, not an engine-internal XID
or a workflow UUID. Full server validation passes all 52 groups: 672 passed,
zero failed and 13 ignored. The largest server library unit-test build omits default LLVM
optimization passes and uses the reduced-memory GNU bfd link to fit the same
10% cap, while retaining assertions and overflow checks. Focused edit and native
fixtures passed with the normal compiler settings. Remaining workspace packages
pass all 47 groups: 1208 passed, zero failed and 5 ignored. Together the two
package groups pass 1880 tests, with 18 ignored; at that checkpoint a single combined workspace
invocation was unverified under the cap; subsequent ADR milestones below verify
the combined invocation. The server group used `live-mssql`
and the remaining group used `sift-server/live-mssql`. Shared CPUQuota=160%,
one-CPU affinity, one Cargo job, serialized tests, MemoryMax=3287567155 bytes and
no swap remained enforced; final service peak was 3283996672 bytes. Server test
codegen used 32768 units, UI/workspace-UI 256 and desktop 512, with package test
debug information disabled. All four changed Rust sources retained their
recorded hashes through staging. These are historical ADR-189 measurements;
ADR-192 supplies migration-group witnesses, while typed delegated execution and
native Windows/macOS acceptance remain open/deferred respectively.

Foundation items 1–3 below are implemented through ADRs 189–192. Typed execution
reuse and the full native engine fault matrix in items 4–5 remain acceptance gates:

1. Let guarded row edits retain the same captured native attempt data as direct
   grid apply, including expected plan and async dispatch checks. Aggregate each
   attempted edit kind with checked arithmetic; distinguish unavailable counts,
   observed rollback and uncertain completion.
2. Capture native transaction identity and terminal outcome per migration group.
   Preserve nontransactional groups explicitly; a workflow/run UUID must never
   populate a native transaction field. Persist optional/defaulted observations
   with the run so old receipts remain readable without invented witnesses.
3. Make direct migration and AI proposal ledgers consume those observations with
   their captured source/actor/correlation. Group outcomes must not collapse into
   a fictitious single native transaction. Keep known native outcomes through
   caller loss, observer deadline, revocation and receipt-delivery failure.
4. Reuse these owners and observations when adding the typed execution action
   family. All action families must share the same chat admission/unknown-state
   guard; a second family cannot bypass an active SQL claim.
5. Verify owned rollback, unknown commit, multiple migration groups, missing/
   overflowing counts, stale source and exact replay on native engines before
   exposing delegated typed apply. Then add its reviewed grants/provider/Vim
   integration; existing human proposal apply alone does not satisfy that step.

ADR-194 verifies those native outcome mechanisms through existing human apply
and migration owners, including reported-count faults and both later-group
refusal/native-error paths. It does not verify a typed request that has not yet
been implemented. Reuse these native fixtures through the new typed boundary,
then prove its atomic approval/claim, source/run revocation, deadline/caller-loss
ownership and provider/Vim receipt behavior before graduating that family.

#### Planned migration observation contract

Persist a versioned, default-empty group-observation collection alongside each
migration run, keeping its existing wire DTO unchanged. An empty collection means
an older receipt or no captured evidence; it must never
be reconstructed from `run.state` or statement status. Each observation binds the
plan's group ordinal, transactional flag, optional actual managed `TxId`, attempted
statement ordinals and a terminal native outcome. The managed ID witnesses the
transaction handle used by Sift; it does not claim an engine's internal XID.

Create a group observation before beginning native work. A failed BEGIN has no
transaction ID and no attempted statements. After successful BEGIN, retain its ID
even if a later statement, COMMIT or ROLLBACK fails. Mark a statement attempted
only at native admission, after dispatch guards succeed. Successful COMMIT proves
the group committed; successful owned ROLLBACK proves it rolled back. A failed or
interrupted terminal command without a successful owned rollback leaves the
outcome unknown. Cancellation before dispatch describes no attempted change;
cancellation after dispatch cannot itself prove rollback. Nontransactional groups
retain individual statement observations rather than treating their aggregate as
an atomic commit. Result delivery and final catalog refresh cannot replace these
native witnesses.

The ledger emits entries for observed transaction groups or nontransactional
statements, retaining workflow correlation separately from `transaction_id`.
Counts use checked arithmetic over observations in that entry's scope. An absent
count, overflow or unsupported aggregation remains unknown, including after a
known commit. A rollback entry is emitted only for an observed owned rollback.
Old runs lacking observations retain their workflow receipt but cannot supply
transaction or native-commit evidence to a new ledger entry.

Both direct migration apply and human AI proposal apply must consume the same
captured observations. Exact AI replay reads its existing durable receipt without
creating new native attempts or duplicate ledger entries. New typed delegated
apply can then use that same owner; it must not add a parallel implementation of
native settlement or bypass the chat's active/unknown execution admission gate.

`migration_run.outcomes_json` currently stores a bare statement-outcome array;
adding a field to the wire DTO alone would lose observations on restart. Store
group and statement witnesses in versioned child tables, validate their plan
ordinals and transaction boundaries on write, and read old rows without a
journal as unobserved. Incremental intent/settlement writes must not rewrite the
whole run's growing outcome array for every statement. Restore orders the journal,
groups and statements after their parent migration run, and deletes in reverse.
Do not silently replace that array with a new envelope understood
only by newer readers. Any feature-use minimum-reader floor must be raised in
the same transaction that first persists those witnesses, and tenant restore
must propagate it just as it does for SQL execution content.

Use a new audited read endpoint,
`GET /v1/metadata/tenants/:tenant/migration-runs/:run/native-outcomes`, with the
same tenant/run authorization as the existing durable run read. Its pure-serde
response carries a format version, run and plan identity, frozen source proof,
journal revision and a bounded statement page with corresponding group outcomes.
Pagination binds the journal revision; a concurrent update requires a fresh read.
An initial header/group page also represents groups whose BEGIN was not observed.
It includes fingerprints and native outcomes, never SQL or diagnostic text.
Add a corresponding Operation variant, SDK method and coverage-manifest entry.
This keeps existing `MigrationRun` responses readable: they reject unknown
fields, so simply adding a defaulted response field would break old consumers.
The internal settlement owner reads the same persisted observation contract,
without a second native execution path. Before graduating the design into an
ADR, prove old-row reads, restart preservation, endpoint authorization, reader
floor propagation and native outcome/ledger correspondence.

#### Migration crash and persistence boundaries

Persist a group/statement dispatch intent before its first possible native call;
if that write fails, refuse dispatch. Keep intent distinct from observed native
admission. A crash after intent but before recording admission cannot prove that
the statement ran or did not run. Represent that admission as unknown, never as
an observed attempt or a safe automatic retry. The live owner can use the same
native execution witness introduced for row edits to distinguish a refused
admission from actual dispatch; a recovered intent has no such live proof.

Persist completed group witnesses before moving to another group. A later
failure or restart must preserve prior known commits/rollbacks while leaving
only unresolved intents uncertain. Record transaction-end intent before issuing
COMMIT/ROLLBACK: statement success alone cannot prove the transaction ended.
A crash after native COMMIT but before the terminal metadata write still leaves
that group uncertain on recovery. Report native settlement and persistence
failure separately; neither a run-level success/failure nor a terminal timestamp
can fill the missing witness. Recovery must not overwrite a previously recorded
native terminal outcome when interrupting a workflow or canceling a run.

Acceptance needs gated failures before dispatch, after dispatch, before/after
transaction end, and between groups, plus restart/reopen checks. Verify that
each case preserves known earlier groups, marks only its unresolved boundaries
unknown, and produces no automatic native resend. ADR-192 records native execution
through this contract; the full native-engine failure matrix remains required
before delegated typed execution.

#### Human apply owner and observer deadlines — ADR-191

The ADR-191 implementation moves the inner AI timeout to its HTTP observer.
Journal storage alone cannot recover a late acknowledgement that no surviving
owner records. The existing bounded HTTP settlement owner retains the claim,
permit, actor/correlation and shared shutdown lifetime through native observation
and durable settlement. An HTTP observer deadline returns a pending receipt only
after that request's durable claim. A preclaim timeout asks the caller to inspect
the durable receipt. Neither case drops the owner or finalizes its audit/receipt
as unknown solely because the observer stopped waiting.

Use cooperative mutation cancellation to close subsequent statement admission
and request owned rollback at a safe boundary. Do not reuse the read-only AI
token path that aborts driver futures. Once COMMIT is issued, cancellation cannot
replace its eventual native acknowledgement with an assumed rollback.

The server-internal transaction-control observation delivered in ADR-190 records
admission and the existing begin/end worker's terminal native result. Structured
human apply owners now await it even when a method's observer timeout returns
first (ADR-191). This changes no Driver trait signature and creates no second
transaction execution path. ADR-192 persists begin/end intent before dispatch,
retains the actual managed handle, and persists observed group settlement before
admitting another group. For statements, retain raw
EOF/Done evidence independently of result delivery. Neither an observer timeout
nor a successful statement response substitutes for these witnesses.

Nontransactional commit evidence needs an explicit autocommit observation from
the execution owner. The absence of a transaction-unknown flag in a default
observation is not positive proof of autocommit. Refused or unresolved admission
must not create an attempted-change ledger entry.

A wedged native worker may retain one bounded settlement slot and its shutdown
reference; it must not freeze request handling or create unbounded replacement
tasks. A process restart closes unresolved durable intents as uncertain without
resending SQL. During live ownership, keep the durable claim pending until native
settlement or observed native uncertainty; do not close the journal early and
then discard a known late commit. Exact replay returns the pending or settled
claim without another dispatch.

Acceptance must distinguish the observer's response from final owner settlement:
hold native gates beyond the observer deadline, check that subsequent dispatch
is canceled, then release them and verify journal/ledger/receipt/audit agreement.
Cover an already-issued COMMIT's late success, rollback failure, caller loss and
revocation, with disclosure rechecks preserving known native outcomes.

#### Native control observation and HTTP ownership — ADR-190 milestone

The existing transaction worker now carries an optional shared observation for
BEGIN/COMMIT/ROLLBACK. Admission captures the actual managed handle, connection
generation and tracked transaction-state condition. Driver acknowledgement is
recorded before host publication/removal; a successful BEGIN that cannot be
published retains its compensating rollback result separately. Stable error codes
omit native diagnostics. Closing admission is atomic with the fallible host end
claim, so a refused command does not strand a transaction in its ending state.

HTTP controls use the existing bounded audit/settlement owner and shared shutdown
lifetime. The response observer can time out and close undispatched admission;
the owner retains its permit, actor/correlation and evidence through the existing
worker's terminal observation. Caller loss leaves that owner running. Successful
late publication or terminal acknowledgement can settle its audit without being
rewritten as a failed native command merely because response delivery ended.

This acknowledgement concerns the requested control operation. It is not proof
that earlier SQL committed when tracked transaction state was already unknown.
Migration conversion must require the captured generation and known owned scope
at native admission; an acknowledgement cannot upgrade an unknown raw/opaque SQL
scope. ADR-191 retains human apply ownership across the HTTP observer deadline;
ADR-192 wires the migration journal producer and both ledger consumers on top of
these existing native workers.

Before journaling BEGIN publication-refusal cleanup, place durable rollback
intent inside its existing worker before the compensating native call. Keep the
actual handle and cleanup acknowledgement even when publication fails. Metadata
failure must not suppress required cleanup: retain uncertainty in the journal
and report persistence failure separately from any observed native outcome.
Do not create a workload-change ledger entry for BEGIN-only cleanup.

Focused verification passes four control-observation tests and 124 HTTP,
transaction, shutdown and OpenAPI checks. Native PostgreSQL/SQL Server/SQLite
Read/Propose/apply/stop fixtures pass 3/3 with additional HTTP BEGIN/ROLLBACK/COMMIT
checks, including commit visibility from a separate connection. SQLite requests
its supported Serializable isolation explicitly; PostgreSQL/SQL Server use
ReadCommitted. Verification on 2026-11-10 passes formatting, strict
workspace/all-target Clippy and the combined workspace run (1,886 passed,
19 ignored by default). Explicit provider acceptance passes 5/5 library cases
and 4/4 HTTP cases. The persistent 10% CPU/RAM ceiling, zero swap, single Cargo
job and existing reduced-memory server test code generation remain in force.

#### Native migration journal implementation — storage/read/recovery foundation

V059 adds separate redacted journal/group/statement tables and an additive
lifecycle classification. Pure-serde source, phase, statement and paged-read
contracts leave the existing `MigrationRun` response unchanged. Metadata freezes
the reviewed manifest before admitting a one-use group/statement intent; native
admission, raw completion and transaction outcome remain separate observations.
An exact SHA-256 of the forward manifest prevents changed SQL from reusing a
coarse statement fingerprint. Stored plan/statement fingerprints retain the
producer's `migfp:`/`sqlfp:` formats; SQL text itself is not persisted here.
The first journal creation atomically raises the minimum reader to 59. Pages
bind a journal revision, retain exact unsigned counters, and reject stale
continuations. Offline snapshot/tenant sanitization preserves known terminal
groups and closes unresolved groups as uncertain without resending SQL.

Durable-store state-machine tests cover these contracts, source/manifest changes,
tenant/owner fencing, zero/maximum counts, reopen and recovery. Formatting,
full workspace/all-target strict Clippy, 180 metadata/protocol tests (one
ignored), a server library check, and focused route/OpenAPI/lifecycle checks
pass for the canonical fingerprints, exact manifest hash and read API.
The audited HTTP read, SDK method and coverage
manifest are now wired, with tenant disclosure rechecked after the blocking read;
route/refusal/audit checks pass. The SDK read roundtrip passes on PostgreSQL,
SQL Server and SQLite fixtures (3/3), including compatibility with workflows
that have no native journal. Daemon startup now recovers interrupted journals
before serving requests; ordinary metadata opens and admin reads do not interrupt
live owners. Startup holds the metadata migration lease for the serving lifetime
in every runtime mode, so a second server with another runtime directory cannot
recover a live owner's journal. Lease identity resolves existing database
symlinks and canonical parent paths; the lock test covers pre-creation exclusion,
serving ownership, Unix aliases and release. Recovery also handles the crash
between journal closure and the
final workflow write without changing native evidence or its revision. The
durable-store fixture exercises actual backup sanitization and tenant merge,
including preserved commits, unresolved admission, and reader-floor propagation.
At this storage-foundation checkpoint, the native execution producer and both
migration ledger consumers were not wired. ADR-192 below supplies that subsequent
milestone. Storage/read/recovery tests do not prove native database execution or
complete the migration provenance prerequisite above.

Verification on 2026-11-10: formatting and strict workspace/all-target Clippy
pass. The combined `cargo test --workspace` run passes 1,881 tests, with 19
ignored by default. Explicit executable provider acceptance passes all five
library cases and all four HTTP cases, including the relocated caller-loss
checks. PostgreSQL/SQL Server/SQLite AI Read/Propose/apply/stop acceptance and
the SDK's legacy-unobserved journal read pass 3/3. These fixtures do not prove
the native producer or ledger integration subsequently implemented in ADR-192.

All Rust/Cargo work retained the shared 10% CPU/RAM ceiling and zero swap.
Server unit tests use code generation without default LLVM optimization passes
and the reduced-memory BFD linker; runtime assertions and overflow checks remain
enabled. Moving HTTP boundary checks to the existing integration suite removes
the full router from the library test compilation graph and makes the combined
workspace run fit the cap. Native Windows/macOS acceptance remains deferred.

### Execution data and older metadata readers

V058 can remain an additive schema upgrade for installations that only use
Read/Propose. However, older binaries do not enumerate execution content handles
in backup/key rotation. Allowing one to manage a database after execution content
exists could retire a key still used by an execution input or receipt.

The first committed Execute run therefore raises `PRAGMA user_version` to at
least 58 atomically with that run. This uses the existing minimum-reader gate;
it does not change the wire protocol or impose a contract migration on unused
Read/Propose installations. Once Execute has been used, binary rollback requires
a reader supporting V058, or restoration of a backup from before that use.
Tenant-selective restore must propagate the greater source/destination reader
floor in its own transaction and reject floors newer than the current binary.
Tests must prove both the pre-use rollback compatibility and post-use gate,
and that tenant restore neither drops the floor nor partially changes a target
when the source needs an unsupported reader.

#### Human apply deadlines and retained native settlement — ADR-191 milestone

The HTTP observer deadline no longer drops a claimed apply's orchestration.
One cooperative admission context runs inside the existing bounded settlement owner.
Its absolute deadline also applies after caller loss; admission is checked again
inside the spawned native worker and atomically with statement observation or
transaction end claims. Deadline closure prevents further statements, BEGIN and
COMMIT, while owned ROLLBACK cleanup remains available.

Structured owners observe issued BEGIN/end controls through actual native
settlement rather than treating a method timeout as their outcome. BEGIN requires
published host state; a known owned end requires native acknowledgement and known
tracked transaction scope. Raw/opaque scope cannot become commit/rollback proof.
Await admitted statement EOF before cleanup, preserving delivery errors separately
from native termination. The HTTP observer returns Applying only after its own
durable claim, otherwise a timeout requiring receipt inspection. The retained
owner finalizes receipt, ledger and original audit; exact replay sends no writes.

Gated acceptance covers closed statement/control admission across task boundaries,
late BEGIN cleanup, late issued COMMIT, pending observer responses and exact replay
without early terminal audit, caller loss, native errors and revoked receipt
delivery. All 8 native-control unit tests and 128 focused HTTP tests pass. Formatting,
strict workspace/all-target Clippy and the combined workspace invocation pass
(1,890 passed, 19 default ignored); explicit provider tests pass 5 unit and 4 HTTP.
Real PostgreSQL/SQL Server/SQLite acceptance passes 3/3. Existing reduced-memory
compiler/profile settings retain assertions and overflow checks; aggregate CPU
and RAM remain hard capped at 10%, with no test stack increase. This milestone
does not itself supply migration journal production or ledger conversion;
ADR-192 supplies that subsequent milestone.

#### Native producer and common ledger conversion — ADR-192 milestone

Open the immutable journal after the parent Running receipt and before the first
group intent. Capture the actual connection generation/configuration and use that
same entry for BEGIN and every statement. Every group and statement needs its
one-use durable intent before native admission. Await actual control workers and
raw statement EOF, then persist observations before any subsequent group.

The existing BEGIN worker invokes an optional server-internal asynchronous cleanup
intent hook before rolling back an unpublished native handle. That hook records
the actual handle and rollback intent; metadata failure cannot suppress mandatory
cleanup or fabricate a durable known rollback. End acknowledgement and known
tracked scope are required for positive group outcomes. Failed terminal persistence
fences the source, refuses later work, and leaves recovery uncertainty intact.
Nontransactional statements retain a positive admission-time autocommit observation;
a default absent transaction-unknown flag is insufficient evidence.

Direct and human AI ledger owners read the same revision-bound journal pages.
They emit scoped transaction-group or individual autocommit-statement entries,
checked signed counts and actual managed TxId values. Unresolved/refused statement
intents and legacy runs create no attempted-change entry. Known rollback alone
permits a rollback entry; workflow UUIDs remain separate from native transaction
identity. Exact AI replay reads the original receipt without new native or ledger
writes. Acceptance extends existing gated HTTP fixtures and native engine flows,
including separate-connection DDL visibility, rather than adding smoke workflows.

The AI owner uses the same revision-bound projection for its receipt and ledger.
Only all frozen statements with native completion and known transaction commits
(or positive per-statement autocommit) can confirm the full reviewed migration.
An earlier committed group cannot imply completion of untouched later groups.
Known rollback, unresolved statement intents and uncertain terminal controls
remain distinct. A late native autocommit can confirm Applied despite a failed
legacy delivery/workflow receipt; the message explains that distinction. The
legacy MigrationRun DTO stays unchanged. A persistence failure after COMMIT must
not issue another native rollback or invent a known durable terminal outcome.

Native acceptance exposed a structural revision issue: estimated row counts enter
catalog graph extras and can change as PostgreSQL statistics settle. They are
already excluded from schema diffing. Structural revision hashing must exclude
that cardinality observation while preserving it in returned graph data and the
full content digest. A regression must prove count refresh keeps the schema
revision stable, visible observations still change, and actual DDL still advances
the revision. Keep every other catalog field in the conservative revision fence.

ADR-192 verification on 2026-11-10: formatting and strict workspace/all-target
Clippy pass. All 52 focused session tests and 128 focused HTTP tests pass. The
combined workspace invocation passes 1,895 tests with 19 default ignored; explicit
provider acceptance passes 5 unit and 4 HTTP cases. PostgreSQL/SQL Server/SQLite
native fixtures pass 3/3 with both transactional preferences and independent
connection visibility; SQLite retains forced transactions. Three additional
PostgreSQL repetitions pass after the cardinality revision fix. The lifecycle
fixture now checks separate nontransactional statement entries after raw EOF.
SQLite migration setup uses individual statements to retain known transaction
scope rather than opaque batches. All 13 Rust sources retain their recorded
validation hashes. Existing reduced-memory compiler/profile settings preserve
assertions/overflow checks under the hard aggregate 10% CPU/RAM cap. No Driver
signature, protocol DTO, metadata schema or CI dispatch policy changes were needed.
Typed execution reuse, its complete native failure matrix and unattended lifecycle
remain open; Windows/macOS acceptance stays deferred.

#### Shared mutation admission before typed execution — ADR-193

SQL Execute and human database apply previously claimed independently. Both now use one
chat-scoped admission check covering both applying and unresolved unknown calls,
inside each existing immediate SQLite claim transaction. Enforce it at the
storage boundary too, so neither family can race the other or bypass the gate.
Exact replay stays readable and never reacquires dispatch authority. Pending
requests/reviews may coexist; at most one native mutation owner may enter.

Unknown human applies need explicit reconciliation before this gate can be
released. Add separate acknowledgement metadata without changing the encrypted
original receipt or its state. An authorized original approver first inspects
redacted claim status and acknowledges the exact proposal, client request and
review digest. Refuse acknowledgement during Applying, after lost access, or
with altered bindings. Acknowledgement is not commit/rollback proof and never
resends the original action. SQL reconciliation keeps its existing semantics.
Raise the feature-use reader floor when human acknowledgement is first stored.

Chat deletion and retention must preserve either family's live owner. Recovery
turns applying claims into unknown without fabricating acknowledgements. SDK,
audited HTTP inspection/reconciliation and Vim controls expose this distinction.
The subsequent typed execution family will reuse this gate and the existing
native review/apply owners; this foundation grants no delegated authority.

Live retention may interrupt an expired model run, but must retain an applying
native claim and its chat. Only exclusive startup/offline recovery may mark an
abandoned native apply unknown; a wall-clock deadline does not prove that its
owner has stopped.

This foundation is implemented in V060 through shared transactional checks and
storage triggers, redacted status and original-approver acknowledgement endpoints,
SDK methods and source-scoped Vim controls. Acknowledgement raises the feature-use
reader floor without changing original receipts or granting new dispatch authority.
Formatting, strict workspace/all-target Clippy and the full workspace suite pass:
1,896 passed, zero failed, 19 default ignored. Existing real PostgreSQL/SQL Server/
SQLite human apply and SQL execution acceptance passes 3/3. The new metadata,
gated HTTP and Vim cases verify live-owner retention, cross-family refusal and
exact acknowledgement without another native call. Typed action dispatch and its
full native failure matrix remain the next implementation/acceptance work.

Explicit external-provider unit (5/5) and HTTP (4/4) acceptance also passes for
this milestone. The checked source/fixture snapshot remained unchanged through
staging; the shared CPU/RAM cap and existing CI policy remain in force.

#### Typed request reuse and real-engine failure gate — design

The next typed execution family references a proposal from the same private Execute
run and its original captured source. The request freezes the native review,
proposal content, source generation, policy/production label, expiry and an exact
fingerprint. Provider arguments contain neither approval nor risk confirmation.
The human inspects the frozen preview and approves that fingerprint; production
and destructive-risk acknowledgements remain explicit human inputs.

Reuse the existing database apply owner and V060 shared claim transaction. A typed
request must not create a parallel native owner or acquire dispatch merely by
being stored. Approval consumption and the existing one-use proposal claim must
be atomic. Replay observes that claim before live-run admission checks, while new
native admission intersects the initiating actor's live rights and original run
source/deadline on every statement and transaction control. Stop can close later
admission; issued work retains its native EOF/cleanup owner. Typed receipts project
the existing native journal/ledger without converting workflow IDs to transaction
IDs or turning unknown work into a retry. Denial and restart must not leave an
approval that can dispatch on another connection. Public write grants, document
and extension families remain separate contracts under the same admission guard.

Before exposing typed apply, extend real PostgreSQL/SQL Server/SQLite acceptance
through a test-only Driver decorator that delegates native operations unchanged
and can hide a successful commit acknowledgement. Independently query the database
to prove that a missing acknowledgement can coexist with a real commit; require an
unknown receipt, uncertain ledger, fenced source and exact replay without redispatch.
Also suppress an affected count after a real update and require confirmed owned
rollback without an invented count. Exercise committed/rolled-back/untouched
migration groups on each real engine. Fault injection describes the host evidence
boundary; it does not claim a physical network outage or a new production adapter.

The native multi-group gate exposed a journal encoding mismatch: historical audit
labels such as `duplicate object` are redacted but not valid bounded storage tokens.
Native observations must retain a separate canonical serde Code discriminator for
the journal, preserving historical audit/receipt labels. Extensible Other text
must be excluded before token conversion. Persistence errors must still fence the
source; valid fixed error kinds must not cause spurious persistence refusal that
prevents owned rollback. No journal validator is relaxed and no native success is
inferred from a label change.

The focused native gate now passes on PostgreSQL, SQL Server and SQLite: real
one-row updates with absent/oversized reported counts roll back with native
acknowledgement, and real row/migration commits with hidden acknowledgements stay
unknown and cannot redispatch through replay/reconciliation. Each engine also
passes the three-group dispatch-refusal and native-error cases. SQL Server can
reject rollback with native 3903 after its error already ended the transaction;
the group stays unknown/fenced and supplies no invented rollback ledger entry,
even when an independent later catalog read observes the table absent. The
separate refusal case proves known owned rollback on every engine. The canonical
token fix preserves the duplicate-object statement failure before either end.
Full workspace validation passes: 1,899 tests, zero failed and 19 default ignored,
with formatting and strict workspace/all-target Clippy. Three expanded native AI
tests and three two-scenario group tests pass on the real engines. Typed execution
remains pending.

ADR-194 final verification also passes explicit external-provider unit (5/5) and
HTTP (4/4) acceptance. All five changed Rust sources retained their checked hashes
through staging under the persistent 10% CPU/RAM cap. No CI policy changed.

#### Typed database request/claim implementation contract

V061 adds an ID-only request row referencing the existing encrypted proposal,
review and run source. It stores redacted source profile/digest, exact fingerprint,
expiry and pending/claimed/denied state. The public state and native receipt derive
from the existing proposal apply row. First use raises the reader floor to V061;
backup/tenant recovery retains the new references and denies unclaimed work when
its run stops. Each proposal has one execution request. Other run/call bindings,
changed call inputs and ordinary human apply cannot bypass that request.

A provider requests an existing same-run private Execute proposal, freezes a
preview on the original source and waits. Approval takes the fingerprint plus
human production/risk inputs. In one immediate transaction consume the typed
pending row and acquire the V060 shared native proposal claim. Replay reads the
old claim before new-run admission, and denial cannot authorize another attempt.
The existing bounded apply owner retains original actor/correlation, native
observations, shutdown tracking, ledger and encrypted receipt.

Attach a synchronous run/profile predicate to that owner's native admission
context, inherited by its actual statement and transaction workers. Recheck run,
expiry, pending/claimed identity, current chat rights and profile/policy digest at
native admission, including BEGIN and COMMIT. Closing the run refuses later
admission while allowing required owned rollback and already-issued settlement.
The normal source/generation worker checks remain in force. Capture the original
source again before native preflight; an inspection/review connection is never
substituted for an execution connection. Status retains native outcomes after
source loss but redacts returned bodies; full review requires current source proof.

Expose audited request/list/status/review/approve/deny/reconcile API and SDK paths.
The three provider adapters share the Sift-only request tool, with no approval
argument. Vim keeps source-scoped selection, exact review and human confirmation;
reopening reads pending/terminal requests without hidden resends. Verify the typed
boundary using the existing real-driver fault decorator and gated ownership tests.

Concurrent exact approvals serialize the native preflight through acquisition of
its durable proposal claim. Waiting duplicates inspect that claim before catalog
or row previews, avoiding concurrent operations on one engine handle. The gate
retains only active proposal IDs and does not replace the storage-enforced claim.
Explicit same-scope catalog refreshes invalidate under their shared fetch gate;
one refresh must not turn another's unchanged native schema into stale coverage.
The existing stale fallback remains available for ordinary catalog inspection;
it does not authorize a stale database proposal.

Interactive typed database execution passes metadata claim/retention acceptance,
strict workspace/all-target Clippy and native PostgreSQL/SQL Server/SQLite
acceptance, including concurrent typed approval, count refusal and lost commit
acknowledgement. All 103 focused HTTP tests pass, including typed lifecycle/
settlement and concurrent catalog refresh. The exact human-confirmation/stale-reply
Vim test and full workspace validation pass: 1,901 passed, zero failed, 19 default
ignored. Explicit external-provider regression checks pass five library cases
and four HTTP cases. All 32 changed Rust/SQL/JSON sources retain their checked
hashes through final review under the persistent 10% CPU/RAM cap. No CI policy
changed. The broader execution/delegation/unattended checklist remains partial.


#### Revision-bound query/document execution — ADR-196

The current `apply_ai_query_proposal` HTTP endpoint verifies/records an already
human-applied draft. It does not perform the text mutation and must not be reused
as evidence that an agent executed an edit. Introduce a separate immutable request
for a same-run query proposal, binding its exact content and base revision to the
initiator, reviewed target, expiry and one-use approval. Provider tool arguments
must not approve the request, alter its target or substitute a stronger actor.

For authoritative room text, admission must retain current write authority and
perform the revision-checked edit through the existing room document actor. CRDT
state remains limited to query text. For private editor text, the live desktop
companion must bind instance, desktop, tab/document identity, revision and request
nonce, then acknowledge the actual local edit. An HTTP proposal status update or
a provider transcript alone cannot prove that local text changed. A missing
companion cannot silently redirect the edit to another tab or room document.

Dispatch claims and text persistence span different authorities; record admission
and settlement separately. Caller loss, Stop, restart or a missing companion
acknowledgement must not automatically replay an issued edit. Preserve a durable
unknown state when actual text settlement cannot be established, with exact
inspection/acknowledgement instead of a claim of rollback. Verify concurrent
approval, stale revisions, tab/document switches, room revocation and late replies
against the actual actor/companion boundary before exposing provider execution.
This contract is implemented and verified in ADR-196.

##### Query execution storage and settlement contract

Use an additive V062 ID/hash-only action row referencing the encrypted proposal
and original run context. Freeze the complete original SQL, original desktop/tab
or room/document, proposal hash, initiating actor, source digest and expiry into
its fingerprint. Refuse incomplete original SQL, a mismatched content revision,
a different run/target and no-op replacement. The initial supported execution
visibility remains private; a private conversation may target a writable shared
room document, while public write grants remain a separate requirement.

Consume exact approval and the existing chat mutation gate atomically. Extend
that gate and its storage enforcement to query actions so unknown editor work
cannot overlap a later native action. Do not let the ordinary human-applied draft
endpoint consume a linked execution proposal. Stop denies unclaimed requests;
already-dispatched actions retain their one-use identity and outcome.

Room replacement uses the existing document actor. Compare complete original text
while holding the actor lock; validate the authored CRDT update through the usual
limits. Append that update and record its exact sequence/receipt/proposal status
in one metadata transaction, rechecking live run, expiry, actor, room write rights
and profile digest there. Import/broadcast only that durably committed update.
This removes the persistence/receipt gap without putting CRDT state in the action
row or changing the ordinary collaboration protocol.

Private-editor approval returns a fresh dispatch capability exactly once, only to
the approving human companion. Store only its digest; status/provider tools never
receive it, and duplicate approval never reissues it. The companion validates
instance, original desktop/tab, full original text, fingerprint, expiry and its
UI request nonce immediately before applying. Its acknowledgement is evidence of
a companion-observed local edit, not server persistence of a scratch buffer.
Applied acknowledgements must match exact before/after hashes; refusal means no
edit was dispatched. Missing acknowledgement becomes unknown, never an automatic
retry. A late exact acknowledgement is retained separately if an unknown receipt
already exists, preserving that original uncertainty and any explicit human
acknowledgement. Wrong-token, wrong-desktop or changed replay refuses.

Startup/tenant recovery makes unobserved dispatches unknown and preserves known
room commits. Active work prevents retention/deletion; historical receipt fields
are bounded hashes, IDs, sequences and stable outcome codes. No new plaintext
SQL, secret capability or encrypted-body inventory is introduced.

Query execution is implemented: V062, pure
serde contracts, shared chat admission/storage gates, one-use companion claims,
atomic document-actor persistence, recovery/retention handling and audited HTTP/
SDK paths, shared provider request tools and the Vim/private-editor companion.
Final verification passes as recorded below.
Focused strict workspace Clippy passes. Metadata acceptance passes 160 tests with
one default ignored, including atomic append/receipt rollback under injected
persistence failure. The actual room actor passes committed, stale-text, stopped,
policy-changed, revoked and update-limit cases with durable reload. HTTP/SDK
acceptance passes room commit, private companion acknowledgement, stale room text,
stopped work and late acknowledgement of unknown scratch work. The same test
proves public Execute refusal and zero SQL dispatch for text actions. A private
Execute conversation can now carry an authorized shared-document target while
retaining the initiating actor's own managed database source; room-only targets
and public Execute remain refused. Full workspace and companion acceptance
are complete, including actual companion admission/reply and captured-server
owner acceptance (see final verification below).

Room restart evidence is stronger than a missing scratch acknowledgement: because
room append and receipt commit atomically, a recovered room action still marked
applying proves that its append did not commit. Recovery records Refused/NoDispatch
for that case; it preserves confirmed room commits. Private editor dispatch still
becomes Unknown, with late companion evidence retained separately. Retention may
expire a missing companion acknowledgement, while actual room owners remain
protected through settlement.

The private companion should retain an owner on the captured server client while
UI dispatch and acknowledgement settle. Queue an exact original desktop/tab,
action fingerprint, review, expiry and UI request nonce. A one-use reply channel
must report rejection when a queued edit loses scope before admission; it must
never send the dispatch capability to the provider or resolve acknowledgement
against whichever server happens to be selected later. Close queued admission
before reporting timeout. Once local editing begins, keep its original owner
through the synchronous edit/reply boundary; loss after admission is unknown,
not evidence that no edit happened. Wrong-instance/chat/tab, stale text, stale
request nonce and duplicate events require real companion tests before exposure.


Companion wiring uses an eight-owner desktop bound, a captured server client and
an original-view desktop identity. The process-global fallback is retained only
for unscoped legacy/test commands; scoped desktop starts and approvals use their
AI view UUID, and UI admission independently requires that exact UUID. Matching
local tab IDs in a different window cannot authorize replacement. Run context is
loaded with the human review so an active provider waiting for approval does not
prevent exact original-text verification. Stop closes local queued admission.
The actual generated-SQL edit preserves normal undo and dirty tracking; its
receipt hashes the actual before/after text. An admitted reply remains owned
through settlement even after its expiry; a dropped queued event reports refusal,
while loss after admission is unknown. The dispatch capability stays in the
captured executor owner and never enters UI events or provider results.

#### Next family: governed extension action ownership

The existing human extension endpoint binds authorized context, consumes a
one-use operation approval for classified actions, then awaits the extension
dispatcher. Before exposing provider execution, retain a bounded owner through
actual dispatch and settlement after caller loss. An approval row or RPC deadline
does not prove an external action completed or did not occur.

Freeze the selected registration/package identity, action classification, input/
output schemas, exact arguments, target context, actor and grant revision for
review. A same-name action updated between preview and dispatch must require a
fresh review. Recheck current registration and authority at actual admission,
then acquire a durable one-use claim; keep the invoker and source identity pinned
through settlement. Provider tools may request that reviewed action, never submit
the human approval or switch credentials/context. Validate output with the frozen
contract and retain bounded redacted receipts.

Distinguish an admitted RPC with uncertain settlement from an extension-reported
success. Neither plugin annotations nor a successful RPC prove native database
commit/rollback or safe retry. Preserve that distinction through unknown outcome
inspection, explicit acknowledgement, recovery, shared chat mutation admission
and Vim review. Existing native database actions should continue through their
native owners instead of acquiring an unchecked extension bypass. This is the
next design candidate; extension execution is not implemented by query edits.


ADR-196 final verification passes formatting, strict workspace/all-target Clippy,
both actual editor/reply tests, all three native-engine and migration-group
regressions, and the full workspace: 1,909 passed, zero failed, 19 default ignored.
The workspace includes metadata atomicity/recovery, room actor, five-scenario
HTTP/SDK and captured-server dropped-companion acceptance. Explicit external-
provider checks pass five library and four HTTP cases. All 37 changed Rust/SQL/
JSON files retain their checked hashes through final review. The persistent 10%
CPU/RAM cap and manual-only CI remain unchanged. Extension execution, public write
grants, reviewed delegation, unattended lifecycle and native Windows/macOS
isolation acceptance remain open/deferred as previously recorded.

##### Extension dispatch settlement contract

Before provider authority, harden the existing human extension and governed-tool
paths through one prepared action and one retained dispatcher owner. Preparation
binds the server-derived target, validates the bounded input against the selected
schema, and freezes package/selection identity, classification, schemas, context
requirements, timeout/result bounds and actual invoker. Include that contract in
both direct-extension and governed-tool approval fingerprints. Same-name package,
grant or schema replacement invalidates a pending review.

V063 records redacted execution identity and a one-use approval link, actor/tenant,
contract/input/context hashes, correlation, terminal audit identity and timestamps.
Consume approval and insert the execution claim atomically. At settlement, append
one terminal audit and link it to the receipt in the same transaction; the existing
operation audit accepts terminal states only. Recovery performs the same atomic
settlement for interrupted owners. Sensitive arguments and results remain out of
this ledger. First use raises the reader floor. Completed
replay returns a redacted observation, not another RPC or invented result body.
Expose own execution inspection/list and approval-bound lookup; recheck current
scope before disclosure. Retain the original bounded ToolContext identifiers for
that check, alongside its hash; no live connection is required merely to inspect
an owner's redacted receipt after source loss. Current actor/tenant and applicable
room/profile authority still gate disclosure. Replay must match original input,
action/tool and target, never invent a new result body or reuse approval for a new
contract. Native admission and output delivery are separate.

At actual supervisor writer admission, recheck current registration, original
actor/token/context/policy, package/selection and tenant allowance after readiness
or writer waits. A closed deadline or revoked source refuses before a new frame;
a partially written frame or missing/invalid response is unknown. Retain the owner,
quota, original actor/correlation and shared shutdown guard after HTTP caller loss
or observation timeout. Never re-resolve a newer invoker midway through dispatch.

Keep separate Claimed, Dispatching, Acknowledged, Refused and OutcomeUnknown
states. Dispatching is a durable intent marker committed by the admission callback
before frame writing; it does not prove the writer was entered or a complete frame
was sent. Store the supervisor's actual frame-entry observation separately (absent
when interrupted). Acknowledged means a schema-valid extension response, not native database commit,
rollback or proof of purity. A refusal is known only before frame admission. Late
responses observed during cancellation grace must remain observable evidence;
workflow timeout cannot erase them. Audit the terminal RPC observation once.
Startup/offline recovery preserves known states, refuses unadmitted claims, and
marks unresolved dispatch intents unknown. A Claimed recovery refusal is valid
only because every frame path must await its durable intent marker first. Tenant
recovery must not revive approval
or issued dispatch authority. No automatic RPC retry follows unknown settlement.

Governed MCP/CLI and the desktop retain their ordinary completed result shape for
first successful observation and gain explicit redacted receipt handling for
pending/replayed/uncertain work. The AI bridge still receives no extension execute
tool in this foundation; its separately reviewed action family follows only after
these human ownership and real supervised-process checks pass.

##### Extension review identity acceptance (ADR-197)

Prepared action contracts and direct/governed approval bindings are implemented.
Changed package/grant revision, schemas, limits, tool descriptor, input or target
cannot reuse the original review; restored exact contracts can dispatch once.
Successful invocation now appends one terminal audit with the original actor and
correlation, fixing the unsupported `started` audit transition. Bounded final
formatting, strict workspace/all-target Clippy with live-mssql and workspace tests
pass: 1912 passed, zero failed, 19 ignored. All five changed Rust
files match their final checked hashes. Durable execution claims, retained owners,
actual writer-admission rechecks, own receipt inspection and recovery remain the
next coupled extension milestone; no AI extension execution tool is enabled.

##### Supervisor observation implementation contract

The observed request path reports whether frame writing was entered, an on-time
response (including remote errors), and a separate exact late response seen during
bounded cancellation grace. Frame entry proves neither complete transmission nor
native database admission. The old request API preserves its response/error shape
and timeout result. Admission runs after acquiring the writer; original deadline
and process drain are checked before and after the callback. Refusal before frame
entry removes pending work without killing a healthy generation; failed/partial
writes still stop it. Cancellation callbacks must fence detached storage work
independently, and higher-level owners must retain the request through cleanup.
This transport foundation does not itself retain HTTP owners or issue durable
extension receipts; those remain the next integration requirements.

##### Supervisor observation acceptance (ADR-198)

The observed transport path and four additional real-process cases are verified
under the persistent 10% CPU/RAM cap. Final formatting and strict workspace/all-
target Clippy pass; focused plugin-host checks pass 35 tests (14 real process
supervisor tests), the workspace passes 1,916 with zero failures and 19 default
ignored, and explicit external-provider acceptance passes five lifecycle and
four HTTP cases. All three changed Rust files retain their checked hashes through
staging. This delivers writer admission and late-response evidence, while durable
action claims, captured live authority callbacks, retained dispatcher owners,
receipt surfaces and startup/tenant recovery remain open.

##### Extension journal recovery identity

V063 execution facts are preserved across tenant rollback alongside the change
ledger; source snapshot rows and dispatch authority are not imported by that
selective operation. Selected target-side pending rows settle under the offline
lease. Ordinary serving startup can refuse an unmarked claim only from the
current crash-consistent database. Full restore instead marks every snapshot
pending claim unknown, including Claimed: it may have executed after capture.
Known terminal observations survive. Full restore deletes old operation approvals
while retaining opaque receipt links, preventing those bytes from authorizing a
fresh dispatch. Settlement and one terminal audit append share a transaction.

The dispatcher integration must also bind review to the original metadata source
identity, not only numeric target IDs. Hash the original tenant/room/profile
creation and configuration/policy identity after authorization; include the daemon
generation for ephemeral session:connection targets. Use the same source digest
in standalone/direct/tool review creation and approval consumption, then compare
it again after readiness and writer waits. A profile edit or reused target ID
requires fresh review. Own receipt inspection after source loss remains a separate
redacted path under current authority; it does not return the old result body.

V063 journal foundation verification passes formatting, strict workspace/all-target
Clippy with live-mssql and all 165 metadata tests (one default ignored), including
five new atomic claim/settlement and recovery cases. The ActionInvoker bridge now
has an observed admission path after process readiness; production dispatch still
uses the old invocation path until the retained-owner integration is completed.
These changes remain uncommitted pending that coupled HTTP/SDK/MCP/Vim work and
full workspace/real-process ownership acceptance. The 10% CPU/RAM cap remains in
force; native Windows/macOS acceptance remains deferred.

SDK invocation now also needs a stable per-intent request UUID, including reads:
classification is no proof of purity. One actor/request UUID and one approval ID
may each name only one durable claim. SDK authentication replay must serialize the
same UUID; exact replay returns a redacted receipt without another RPC or a stored
payload. Conflicting request/approval identities refuse. Ordinary raw HTTP calls
without a request UUID create a new intent, so retrying clients must preserve one.

The retained dispatcher now owns atomic claim, actual writer admission, response
validation and terminal settlement after observer loss. HTTP callbacks re-resolve
original credentials and current actor/context/policy; registration/package/grant
checks and original source digests are checked at admission and output delivery,
including after storage waits. Governed descriptors are independently pinned.
Owned list/get and request/approval lookup routes return only redacted receipts;
SDK, standalone MCP and existing Vim result views recognize Observed replies.
Focused metadata, real-process HTTP ownership/revocation/replay and Vim acceptance
is in progress. Independent desktop receipt browsing/refresh after lost replies,
document-associated source identity review, full workspace acceptance and the
signed commit remain outstanding. No AI extension execute tool is enabled.

The journal marker now compares one transactional metadata snapshot of principal,
tenant membership, room membership, document/profile binding, profile policy and
opaque credential bindings, plus selected package/grant revision and tenant
allowance. Verified API/access credentials carry only opaque row/creation references
for revocation/expiry checks in that same transaction. The callback still verifies
credentials and live native connection generation separately; no token bytes are
persisted. Tenant-scoped tokens cannot inspect receipts outside their scope or
borrow instance-admin extension authority. Routine token last-used writes do not
invalidate review.

The receipt browser is independent of active connections and uses only audited
owner reads, with instance/generation/account fences, stable selection on refresh,
and private-view clearing on account change. CLI `tools receipts` and a fixed
read-only standalone MCP receipt tool inspect the same records; explicit
`sift/requestId` metadata preserves an MCP intent across retries. Capturing the
queued desktop command's original principal/endpoint before HTTP dispatch remains
a follow-up, distinct from the retained HTTP owner's captured credentials.

##### Desktop queued extension source binding — ADR-200

Capture a trusted caller scope when the UI enqueues extension/governed invocation,
approval creation/approval, or receipt reads: selected instance identity, original
base URL, verified handshake instance/daemon generation and principal. Keep this
separate from display generations and connection binding. A reused logical instance
ID or a cleared private view does not cancel an already queued HTTP command.

The executor must refuse a mismatching scope before transmitting arguments or
approval bytes. Pin the original authenticated client credential after checking
that scope; do not read a mutable session token provider after a later account
switch. Any credential refresh must retain the original principal and session.
An uncached handshake detects replacement before dispatch, but cannot alone close
replacement between probe and invocation. Carry expected server identity in the
request and enforce it at the receiving boundary before extension processing;
probe-only checks are not a complete fence. Preserve the HTTP owner's existing
source and credential checks after request acceptance.

Use an explicit negotiated capability for this receiving fence. The SDK pins
expected instance/daemon identity and principal on its captured client and sends
those expectations with each scoped request. Reject server mismatch before routing
and principal mismatch after authentication, before action/approval processing.
Refuse queued scoped actions against servers that do not advertise support; an
older server silently ignoring expectation headers is not sufficient acceptance.
Scope events as well as commands so a reused display generation cannot publish
another account's result. Local/configured instances retain their manager-resolved
transport; compare the selected logical endpoint separately rather than assuming
its display URL is the manager's actual listening address.

Compatibility also needs a distinct scoped extension/approval route family. An
older daemon can replace a capable server after the last probe and ignore new
headers on a legacy route. Send scoped mutations only to routes that did not exist
before the fence, require all identity expectations on those routes, and map the
same retained handlers behind that boundary. An old server must return missing
route, never perform an unguarded legacy invocation. Capability negotiation and
the uncached probe remain useful preparation checks, while the scoped route and
receiving identity comparison provide the action-admission boundary. Do not
silently fall back to legacy routes on missing capability, mismatch or 404.

Return results only into the original UI/account/source generation. A stale event
must not repopulate private receipt rows or approval state. Preserve request UUID
for an exact original-intent retry; account/source changes require new review.
This is a prerequisite for AI extension execution, alongside a separate immutable
same-run extension request and the existing shared AI native-action gate.

Acceptance covers queued account switch, endpoint replacement under one logical
ID, daemon restart, token refresh/switch between client capture and send,
replacement after handshake, and late response delivery. Verify refusal before a
new action frame and preservation of the original receipt when the old HTTP owner
was already admitted. Use real server boundaries for transmission checks and a
focused Vim test for queue/event fencing.

##### Retained extension execution acceptance (ADR-199)

The final unchanged-source Linux pass under the shared 10% CPU/RAM cap passes
formatting, strict workspace/all-target Clippy with live-mssql and the complete
workspace suite: 1,923 passed, zero failed and 23 default ignored. The 35 focused
plugin-host tests also pass. Workspace coverage includes atomic approval/claim
and audit settlement, concurrent replay, conflicting approval identity, metadata
permission/credential/package admission guards, current-file and historical/tenant
recovery, governed dispatch and independent Vim receipt browsing. Explicit ignored
acceptance also passes five provider lifecycle tests, four real supervised-process
extension HTTP cases and four provider HTTP cases. The extension cases cover
caller loss with original actor/correlation/shutdown retention, one-RPC replay,
pre-frame revocation, post-frame result withholding with known acknowledgement,
Read timeout replay and document-associated profile review identity. All 37
changed Rust/SQL/JSON files retain their checked hashes. No queued caller-scope
draft or AI extension authority is included in these results.

##### Queued caller-scope acceptance (ADR-200)

Desktop commands and replies capture logical endpoint, verified instance/daemon,
principal/session and original native connection IDs. Missing authenticated scope
refuses enqueue; changed caller or target context withholds stale results and
approval publication. Receipt inspection requires no native connection.

The receiving boundary requires all four assertions on distinct guarded routes.
Caller mismatch retains the actual authenticated HTTP audit actor and creates no
native claim. The SDK pins one credential, validates approval UUIDs before URL
construction and disables redirects before probing, refreshing or invoking.
Missing routes and 307/308 responses never fall back to ordinary endpoints.

The unchanged-source Linux pass under the aggregate 10% Rust/Cargo CPU/RAM cap
passes formatting, strict workspace/all-target Clippy, the focused SDK credential/
404/307/308 and malformed-ID regression, 1,925 workspace tests (zero failed,
24 default ignored), and all five explicit real-process extension HTTP cases.
HTTP acceptance includes guarded Write approval, audit actor/path, one-RPC replay
and refusal before native claims. Vim cases cover account/session/endpoint/daemon
changes, missing identity and native-connection replacement with a pending reply.
All 15 implementation/test source hashes match the accepted run.

This does not enable AI extension execution or native provider certification.
The separate cancellable-supervisor draft remains outside this milestone.

##### Reviewed AI extension execution — ADR-203

The existing shared admission mechanism is a transactional predicate plus storage
triggers across SQL executions, proposal applies and query edits (V060/V062), not
a separate generic claim table. Extend both the predicate and triggers in every
direction for a new extension family. All extension classifications participate:
a declared Read action is not proof of purity. Direct native SQL/row/migration
actions continue through their existing native owners.

A private active Execute run may request one immutable governed action using a
call UUID and its lease. The provider supplies tool identity and bounded arguments,
never an approval, alternate actor, credential or freely selected target. Resolve
the allowed ToolContext from the original run context/source, prepare one exact
registration, and freeze descriptor, package/grant revision, classification,
schemas, limits, arguments and authorized source for human review. Store private
arguments/review material through encrypted AI content handles; V063 still stores
only identifiers, fingerprints and RPC observations. Enforce the canonical run
quota and call identity when recording the request. Repeated call IDs inspect the
same request; changed content or contract refuses.

Human approval must bind that exact review, original caller/source and any required
production/risk confirmation. Under one immediate metadata transaction, validate
active run/review and clear shared admission, consume the AI review, and create
the corresponding V063 claim plus any required operation-approval authority/link.
There must be no separately committed AI mutation slot, approved generic operation
or dispatchable extension claim left between those steps. Refactor the existing
extension claim into a locked helper rather than nesting metadata connections.
An exact replay returns the original AI/journal identities and never a second RPC.

Retain the existing bounded extension owner before claim await, including original
actor/correlation, deadline, source/credential proof and shutdown lifetime. At the
actual writer callback, check AI run status, Stop and original review expiry as
well as the existing live contract/source/package guards. Bound the RPC deadline
by the original action timeout, review expiry and run deadline; waiting for human
approval or process readiness cannot renew it. Link the AI request to V063 in the
claim transaction and derive its issued/terminal RPC state from that immutable
journal identity instead of separately committing a duplicate terminal state.
Admission predicates/triggers must follow that link for active and unacknowledged
unknown work. A missing restored link is unknown, never unissued or retryable.
Encrypted result availability is separate: failure to store or deliver a private
body cannot erase the journal observation/audit or reopen dispatch authority.

Stop after frame entry requires an explicitly cancellable observed supervisor
request, not dropping its future or unconditionally killing a healthy shared
process. Keep the owner through bounded cancellation grace and retain late
acknowledgements separately. Preserve the supervisor's existing generation fencing
for partial frames, failed Cancel writes or missing cancellation acknowledgement;
other work affected by that fence is uncertain, never implicitly rolled back.
Refusal before frame entry is known; partial frames and missing/invalid responses
remain unknown. Keep the primary Stop/deadline observation even if cleanup also
fails. Stop/deadline closes future admission immediately while the issued owner
retains its native/RPC shutdown lifetime. Test this transport extension before
wiring provider tools to it; legacy supervisor callers keep their existing behavior.

The production action boundary also needs this primitive: `ActionInvoker` has
both a conservative in-process fallback and the tenant process runtime adapter.
Do not wrap their current observed call in a select that drops an issued owner.
Carry Stop through the runtime to the actual supervisor; process readiness must
remain bounded and cancellation-safe before any frame. Preserve existing callers
through an always-pending Stop path. A future trusted in-process registration must
supply truthful cancellation observations or refuse delegated execution before
invoker entry; it cannot claim native-process cleanup it does not implement.

Targeted observed supervisor Stop passes Linux acceptance (ADR-201). An
always-pending Stop preserves existing callers; no-frame cancellation keeps a
healthy generation, and issued work retains bounded Cancel cleanup with late
acknowledgement independent of the primary Stop/deadline cause. Partial frames or
missing acknowledgements fence the generation. Seventeen real-process supervisor
cases pass, including writer waiting, synchronously ready Stop after admission,
late/no acknowledgement and interrupted large-frame transmission. Formatting,
strict workspace/all-target Clippy and 1,928 workspace tests pass (zero failed,
24 default ignored), with both changed Rust source hashes unchanged throughout.
This exposes no AI extension authority. Production runtime/invoker, atomic review
and journal integration remain part of the coupled AI family below.

Production runtime readiness and bounded startup pass Linux acceptance (ADR-202).
The original deadline/Stop covers tenant waits, restart backoff, Hello and Welcome;
startup retains its generation permit through child cleanup. The same owned Stop
reaches native RPC without spending provider restart budget. Conservative
in-process invokers refuse delegated entry. Twenty real-process supervisor cases,
one ordinary readiness case and two explicit native runtime cases pass, including
repeated Stop without quarantine and actual adapter-to-RPC late acknowledgement.
Formatting, strict workspace/all-target Clippy and 1,932 workspace tests pass
(zero failed, 26 default ignored); all five changed Rust hashes remain unchanged.
This enables no AI extension authority by itself.

The AI receipt describes RPC settlement only. Timely valid result/error replies
can complete the RPC workflow without claiming database commit, rollback, affected
rows or safe retry. No-frame refusal denies the request. Missing/invalid replies,
timeout or interrupted dispatch preserve unknown state and block shared admission
until explicit human acknowledgement of the exact original claim. A late valid
reply augments journal facts without silently reopening the stopped/expired run or
authorizing retry. Output revocation withholds private bodies while retaining known
observations. Acknowledgement never rewrites the original receipt or issues work.

Startup and full/tenant recovery must project the preserved target V063 journal
into linked AI state without reviving review or operation approvals. Historical
pending claims may have executed after snapshot capture. Prevent chat/run deletion
while an owner is active and preserve one-use request/approval identity through
unknown acknowledgement and recovery. Resolve the journal by immutable linkage;
restored AI rows must never recreate dispatch authority from old encrypted bytes.

Expose audited request/list/get/approve/deny/status/acknowledge paths, SDK and Vim
review on the original captured caller, plus fixed request/status provider tools
for all three adapters. Provider tools cannot approve, acknowledge unknown work or
invoke the human dispatch route. Acceptance must combine real supervised-process
HTTP ownership/replay/revocation/Stop/late-response cases, concurrent cross-family
admission and rollback-injection tests, encrypted storage/recovery, and actual Vim
review/caller fencing. Public writes, reviewed delegation and unattended lifecycle
remain separate explicit authority contracts; this design adds none by itself.

ADR-203 implements the reviewed extension contract above. The frozen review,
classification and production confirmation feed one atomic V064/V063 claim; all
four action families share admission. Exact replay cannot replace the first
owner's encrypted output. Redacted source-lost receipt acknowledgement never
requires reconnecting or approves another action. Native and workflow evidence
remain separate, including historical missing or mismatching journal links.

Acceptance: Six real extension-process HTTP cases pass, covering captured-caller
concurrent approval/replay, post-retention and in-flight registration revocation,
source revocation, Stop with a late acknowledgement, and invalid late bodies
without losing the primary interruption cause. Seven metadata cases cover atomic
rollback, shared admission, encrypted result loss/cleanup, historical full/tenant
restore, durable pre-frame Stop and mismatching journal acknowledgement. Two actual
Vim interaction cases prove human production/classification approval, caller/source
fencing and exact unknown acknowledgement after connection loss. Metadata and
process HTTP cases also run with JSON key-order preservation enabled; canonical
review hashes remain interoperable with the desktop build. Formatting, strict
workspace/all-target Clippy and 1,942 workspace tests pass (zero failed,
32 default ignored), with all 36 changed Rust/SQL/JSON source hashes unchanged.
All Rust/Cargo validation uses the persistent aggregate 10% CPU/RAM cap.

Public write grants, reviewed run delegation, unattended/headless lifecycle and
native Windows/macOS acceptance remain open or explicitly deferred; this milestone
does not grant them. Third-party native database semantics require separate
provider certification.

##### Run delegation and unattended lifecycle — next coupled contract

ADR-204 defines the next authority/lifecycle work. It includes exact and bounded
reviewed rules plus separately enabled arbitrary supported SQL, cumulative budgets
consumed in the actual action claim, current initiator authority, distinct approval
provenance, original native/editor source, and independent settlement after Stop or
revocation. Model request shapes do not gain approval or grant-selection fields.
A recurring definition needs its own reviewed expiry/workload envelope and creates
one fresh run-bound delegation per existing scheduler occurrence; a prior uncertain
workflow cannot continue automatically through a new run/chat or expired lease.
Provider credentials stay on the initiating machine. Public-room write authority
and deferred native Windows/macOS acceptance remain separate requirements.

ADR-205 delivers the UI-independent local provider runtime prerequisite, with a
typed private-debug-safe observer sink and captured desktop scope adapter. The
normal dependency graph excludes UI/server/metadata crates, and all three installed
Linux provider isolation/tool roundtrips pass. Existing interactive behavior and
Linux isolation remain intact; this prerequisite grants no unattended authority. A shared lifecycle owner, captured receiving
identity, delegation/occurrence storage and atomic claims, SDK/CLI/provider flows,
Vim controls and end-to-end recovery acceptance remain part of the coupled work.

Acceptance: The normal dependency graph has no UI, server or metadata crates.
All 13 moved default runtime tests pass; the three installed, signed-in Linux
Codex/Claude Code/OpenCode isolation and Sift-only tool roundtrips pass explicitly.
The desktop adapter test proves immutable view capture, private-text debug
redaction and closed-observer reporting. Formatting, strict workspace/all-target
Clippy and 1,943 workspace tests pass (zero failed, 32 default ignored).
All 19 implementation/manifest snapshots, including removed desktop paths,
remain unchanged through acceptance. Validation retains the aggregate 10% CPU/RAM
Rust/Cargo cap. Provider implementations and existing tests match their former
bodies after the declared type/import substitutions. ADR-204 remains proposed;
this prerequisite creates no unattended authority or headless entrypoint.

The delegation policy layer is implemented: `sift-protocol::AiDelegationPolicy` and
`sift-core::ai_delegation` describe and match encrypted reviewed rule material.
Exact SQL/row edits/migration plans, bounded typed rows/schema changes/query text,
and frozen extension contracts with bounded offline arguments have distinct rules.
Arbitrary SQL additionally requires current instance and tenant opt-in; submitted
row/change bounds make no claim about triggers, external effects or native work.
Pure reservation arithmetic covers cumulative tool calls, input/output bounds,
per-rule uses and submitted rows/changes. The evaluator does not persist or grant
authority. Native preparation, source/actor/policy/credential/lease/expiry checks,
atomic durable usage and journal claims, replay, revocation, receiving identity,
SDK/CLI/Vim review and unattended/scheduled owners remain unfinished. ADR-204 and
the parent product checklist remain partial until that coupled acceptance passes.

The next storage/owner integration follows these transaction boundaries:

1. A human delegation preview owns encrypted policy and source bodies, an exact
   review digest, initiator/chat/tenant, current policy/credential scope and an
   absolute expiry. Approval records the actual human approver. An approved
   preview is consumed once by run startup; it cannot be reused by another run.
   The active grant expires at the earlier of that absolute expiry and the
   original run deadline. Stop/revocation/recovery never reopen it.
   Run creation and binding its original Execute source now share an immediate
   startup transaction. Consuming an approved delegation preview must join that
   same transaction when grant activation is implemented. Adding it as a later
   step would leave partially initialized authority when startup fails. The
   focused startup regression proves rollback of run/events/chat revision/reader
   floor and all four encrypted bodies, concurrent exact replay, unchanged source
   identity and refusal after cancellation. Workspace acceptance for this startup
   prerequisite passes; it enables no delegation by itself.
2. Before preparing a broker request, a unique `(run_id, call_id)` reservation
   binds its operation, immutable input digest, input bytes and reserved response
   bound. Reads, staging, status and refused preparation retain that reservation.
   Exact replay observes its existing reservation; changed input refuses. This
   is distinct from consuming an action rule, so preparation failure cannot
   erase call quota and native admission cannot double-charge a broker request.
3. Each native claim transaction proves that the original request reservation
   is unused, reloads the current grant and durable counters, evaluates the
   actual prepared action and consumes its rule use/row/change counters. It
   links that same call and action fingerprint to the existing family journal
   in the same immediate transaction. An injected journal failure rolls back
   the action debit/link, while the earlier tool-call reservation remains spent.
   Shared chat admission and the original native owner remain authoritative.
4. Approval provenance distinguishes individual human approval from delegation
   and records the real approver. SQL/typed-database settlement currently uses
   `approved_by == actor`; delegation must separate that human identity from
   the initiating native actor without requiring a still-active grant to retain
   an already-issued outcome. Extension approval consumption remains inside
   the same transaction as its RPC journal; no provider-selected approval ID is
   introduced. Query edits still require native document/editor evidence.
5. Restore, retention, content-key rotation and deletion must include the new
   encrypted handles and journal links. Missing restored claim evidence is
   uncertain, not an unused grant. Companion and scheduler claims precede any
   provider startup; neither lease replacement nor a fresh chat can retry an
   uncertain occurrence. These ownership and recovery paths require coupled
   acceptance before the delegation checklist can become complete.

Bounded migration admission additionally needs a native preparation check for
embedded SQL. The current renderer copies default expressions and some constraint
definitions into generated statements; attaching a change ID/risk to a statement
does not prove that the fragment preserves the intended typed operation. Before
automatic bounded admission, prove the rendered statement shape and original
object binding, or require exact reviewed/frozen SQL material. A supplied fragment
must not introduce another statement under an allowed change ID. Keep arbitrary
SQL behind its separate current instance/tenant opt-in. This validation is about
the admitted command shape; it does not contain function, trigger, affected-row or
external effects. This remains coupled implementation work, not acceptance proven
by the pure matcher tests.

Pre-start human review must resolve the actual native material behind opaque
policy hashes: migration command/rollback text and risks, extension package/tool/
schema/classification/context, and original query text. Private editor text is a
local observation until the original live companion verifies the actual edit;
room text comes from the authorized committed document. A hash alone is not a
human-readable review. Exact migration matching uses immutable plan content and
the selected native changes, excluding generated plan/run IDs, their derived
digest and expiry. Each actual native plan still retains its own identity and
deadline checks. Otherwise pre-start approval could never match a freshly
prepared plan. Native preparation must also identify the submitted changes
actually emitted by the plan; implicit child nodes belong to their parent DDL
action rather than inventing independent dispatches. The complete generated SQL
remains subject to native command-shape validation and exact-content review.

The review store is now implemented in the coupled delegation work. V065 keeps
policy/material bodies in encrypted external content, with immutable review and
request fingerprints, explicit human approval, original target/desktop/source,
tenant policy revisions, expiring one-use run identities and monotonic usage/link
tables. Referenced review bodies participate in backup/key inventory and durable
cleanup; their first write raises the reader floor to 65. Policy replacement
revokes pending reviews and returns original runs for retained cancellation.
Full and coordinated tenant recovery revoke unconsumed approvals and interrupt
grants while preserving consumed review identity. Three review-store cases cover
concurrent replay, production confirmation, policy gates/revisions, private debug
output, required material, rotation and cleanup; a tenant restore case proves the
authority projections. All 179 metadata tests and strict metadata/protocol Clippy
pass under the 10% resource cap. The subsequent coupled work implements atomic
run activation and per-call reservations; native journal/provenance integration,
receiving guards, public human routes, SDK/CLI/Vim and companion/scheduler owners
still require completion and coupled acceptance. The parent checklist stays partial.

Policy-layer acceptance: ten focused cases pass in both default JSON mode and
key-order-preserving mode. They cover exact SQL/rows/plans, bounded typed rules,
original query target/revision/hash, frozen extension package/context/classification,
strict offline argument constraints, separate arbitrary-SQL opt-ins, overlapping
rule/global quota limits, overflow/corrupt usage, private debug redaction and
separate call reservation/action debit after preparation refusal. Formatting,
strict workspace/all-target Clippy and 1,953 workspace tests pass
(zero failed, 32 default ignored) under the aggregate 10% CPU/RAM cap.
All five implementation snapshots remain unchanged through acceptance. This is
pure policy matching and arithmetic; no durable grant, automatic native action,
companion, scheduler or unattended authority is enabled. ADR-204 stays proposed.

Atomic startup acceptance: the source, run, start event, chat revision and reader
floor commit in one immediate transaction, and losing/failed startup removes its
four encrypted bodies. Concurrent exact replay retains one original lease/source;
source-generation replacement and canceled-run restart refuse. Formatting, strict
workspace/all-target Clippy, the focused disk-backed rollback/replay regression
and 1,954 workspace tests pass (zero failed, 32 default ignored) under
the aggregate 10% CPU/RAM cap. Both implementation snapshots remain unchanged.
No run delegation, model tool, unattended owner or scheduler entry is enabled.

Delegation implementation in progress: approved reviews now activate exactly once
in the same transaction as the original run, source and start event. Startup
rechecks target/source/desktop and current tenant policy after decryption; exact
concurrent replay preserves the original lease and absolute grant deadline.
Immutable per-call reservations charge input and reserved output before preparation,
including preparation refusals. Changed requests refuse; observations after run
closure do not renew admission or refund quota.

The SQL and query-edit metadata claims now evaluate actual immutable action
material and atomically debit the matched rule, link the call, and claim the
existing native journal. SQL failure injection proves those writes roll back
together while the prior request reservation remains spent. Query claims issue
the original editor capability only once and never manufacture an applied receipt.
Delegated extension claims use the same frozen-input validation as human claims;
the approval consumption, RPC journal and delegation link share that transaction,
and the stored RPC deadline is clamped to the original grant deadline. Claim
evidence records the actual run-review approver, approval time, rule and run.
These are server-internal metadata entry points, not exposed provider authority.
Typed database claims also consume their existing proposal apply and execution
journals in that transaction, taking immutable row edits from the saved draft and
migration commands from the saved preview. Migration change material is a trusted
server-only preparation input; no model-facing material/approval field is added.
Native migration command validation, complete public
provenance delivery and the broker/receiving/UI/companion/scheduler owners remain
coupled unfinished work. Existing human execution paths remain the shipping flow;
ADR-204 and the parent delegation checklist stay proposed/partial.

Metadata acceptance for this in-progress state: ten focused delegation tests and
all 186 metadata library tests pass (zero failed, one existing ignored), as does
strict metadata/all-target Clippy, under the aggregate 10% CPU/RAM cap. Extension
failure injection rolls back its generated approval, RPC journal, rule usage and
claim link together; exact replay emits no second RPC dispatch, and the persisted
RPC deadline equals the shorter original grant deadline. These metadata fixtures
also prove typed row claims roll back the proposal journal and usage together,
then admit one dispatch and debit one submitted row under concurrent replay. They
do not prove native provider execution, live editor observation or complete
workspace acceptance of the still-unfinished feature.

The server migration preparation boundary must retain the renderer's original
diff evidence alongside `StoredMigrationPlan`; material for a rule comes from
the change IDs actually emitted by that retained plan, not a new model-supplied
change list. Implicit children represented by parent DDL remain part of the
rendered command and its review, rather than counting as extra dispatches.
Lookup must intersect the original proposal, plan digest/deadline, principal,
managed profile, session/connection generation and current policy. Only then may
the server construct the non-wire migration material passed to the metadata claim.

For bounded migration rules, validate the actual complete command using the
engine grammar and bind its command family, target and typed operation back to
that retained evidence. Scalar default/index predicates must parse as complete
expressions; copied constraint definitions must remain the intended constraint,
without injecting a second statement or extra ALTER operation. Recognizing a
DDL prefix, accepting a plan change ID, or reparsing into any SQL statement is
insufficient. Native special forms need equivalent typed evidence. Exact frozen
command review is a distinct rule path and cannot silently stand in for a bounded
rule. Neither grammar checks nor object/row bounds contain functions, triggers,
cross-object native dependencies or external effects; that disclosure remains
part of the human run review.

Durable authority-layer workspace acceptance: Formatting, corrected backup/restore lifecycle fixtures, strict workspace/all-target Clippy and 1,965 workspace tests pass (zero failed, 32 default ignored) under the aggregate 10% CPU/RAM cap. All 23 implementation snapshots remain unchanged through acceptance. This covers the internal metadata authority and existing human flows; native bounded-migration preparation and complete public/client/companion/scheduler acceptance remain unfinished. ADR-204 and the parent feature remain proposed/partial.

Native preparation implementation continues with complete grammar checks in the
UI-independent semantic crate for copied native types, defaults, index predicates
and constraint definitions. AI migration preview now checks changed forward and
rollback material before retaining the rendered plan, including columns absorbed
into parent CREATE TABLE commands. Fragments cannot add another command/operation
or absorb generated suffixes with comments; errors identify only the change, not
the sensitive fragment. SQL Server sequence defaults and PostgreSQL NOT VALID
CHECK/FK suffixes have explicit grammar support. These checks do not yet establish
the full retained native material/object-binding capability required by bounded
delegation, and do not contain native functions, triggers or dependency effects.
Public delegation and unattended execution remain unfinished.

Captured transport adoption requires the desktop/provider boundary as well as
HTTP aliases. `AiViewScope` now retains the original endpoint and instance,
daemon, principal and authentication-session assertions alongside tenant/view
IDs. Caller changes invalidate old replies and clear private prompts/title/search
state while Stop keeps its original owner. The provider runtime, adapters, harness
and Sift-only broker now require ScopedAiClient; desktop startup captures it from
the queued caller before policy, chat, history or lease creation. Snapshot/public
observation and post-apply refresh use the captured type too. Startup cancellation
and completion/Stop/deadline retain the same transport. Full acceptance and all
three real isolated Linux provider roundtrips must pass before this runtime
adoption is recorded as verified. Native review/approval/setup, external-source
vault setup, room acknowledgements and durable companion ownership remain pending.

Public review wiring reuses the existing four-header receiving boundary under
`/v1/scoped/`: it checks instance/daemon before routing and authenticated principal/
session before handler dispatch. Explicit scoped AI/setup aliases advertise their
dedicated capability. Capture a fixed endpoint, credential and expected identity
before SessionCreate/ConnectProfile or queued startup; keep redirects disabled and
refuse unavailable scope support.
The captured AI transport is now implemented as an explicit private-transport
SDK type, with one shared definition of existing AI HTTP operations. Its new
capability requires guarded caller discovery before setup; managed session/
connection setup and all AI aliases use the original handlers and four receiving
assertions. Scoped operations document the required headers. Ordinary Client
transport mutation/rebinding is not exposed. SDK behavior tests cover frozen
credentials after account replacement, missing routes and disabled redirects.
Full HTTP/workspace acceptance passed; desktop/provider adoption remains in progress;
the earlier extension-only capability does not imply this broader coverage.
Scoped setup aliases now resolve through canonical session/cursor ownership and
tenant/rate-limit classifiers, including percent-decoded numeric path/query IDs
matching typed handler extraction. This preserves the existing runtime-owner
lookup. HTTP acceptance exercises wrong caller assertions before setup, foreign
sessions under ordinary/scoped/encoded paths, and captured managed setup/turn
startup; complete coupled owner/companion acceptance still remains.

Extension review does not need weaker contract hashing or a sample-argument field.
The dispatcher already clears argument annotations before freezing the prepared
operation and registration/package/schema/source contract. A shared registry
method now provides that existing preparation before concrete arguments exist;
it passes the server check, while public review wiring remains pending. The
invocation path still performs input-schema validation, computes its exact
argument binding and intersects the separately reviewed offline argument rule.
Keep target, classification and package/registration fields in the frozen digest.

Native identity evidence must not reuse the catalog's `database_identity` as an
attestation: `SessionStore::catalog_graph_for_operation` currently derives its
`dbfp` from provider/configuration (and the handle for in-memory SQLite). Catalog
revision and this configured identity support cache/source binding, but do not
observe a PostgreSQL database OID/role or SQL Server database GUID/token. Review
and scheduled admission must distinguish these configured constraints from the
explicit observable native evidence they actually validate. Preserve captured
source generation and refuse transparent reconnect when collecting that evidence.

AI migration fragment acceptance: All 32 semantic tests, formatting, strict workspace/all-target Clippy and 1,967 workspace tests pass (zero failed, 32 default ignored) under the aggregate 10% CPU/RAM cap. All 4 committed implementation snapshots match the accepted sources. Grammar cases cover PostgreSQL/SQL Server/SQLite escapes, comments and constraint kinds, including native sequence/default forms. The renderer regression proves implicit child columns are checked without echoing sensitive fragments. This does not complete retained native command/object proof or public/unattended delegation; ADR-204 stays proposed and the parent checklist stays partial.

Captured HTTP transport acceptance: Formatting, all 16 SDK tests, real HTTP caller/setup/turn acceptance, four OpenAPI contract cases, strict workspace/all-target Clippy and 1,969 workspace tests pass (zero failed, 32 default ignored) under the aggregate 10% CPU/RAM cap. All 14 committed implementation snapshots remain unchanged through acceptance. The shared SDK operation bodies and exact literals match their prior definitions apart from formatting/trailing parameter commas. Coverage includes capability refusal before identity/setup, static credentials after account replacement, no legacy/redirect fallback, wrong caller assertions before native work, ordinary/scoped/encoded foreign ownership and managed setup/turn startup. Desktop/provider adoption, durable companion ownership and the full delegation lifecycle remain unfinished; ADR-204 and the parent inventory stay proposed/partial.

Desktop caller adoption design: extend the immutable AI view scope with an optional
captured endpoint and four-part caller identity derived only from an authenticated
handshake advertising `ai.caller-scope@1`. Compare that value when admitting replies
and synchronizing presentation, so account, authentication-session, endpoint, server
instance and daemon changes roll the view and clear private AI state. Preserve the
old scoped sender when requesting Stop; never rebind it to the replacement caller.
Clear draft/submitted prompts and private thread title/search inputs on caller changes instead of restoring private text
into the replacement account. Missing identity/capability has no captured caller.
This is a presentation and queue-identity prerequisite; provider/runtime HTTP
adoption and refusal before setup remain separate required implementation work.

Room-document proposal acknowledgements and queued native reviews must also retain
the original captured transport through settlement. Their existing UI scope alone
is insufficient: several handlers still obtain an ordinary client at dequeue or
completion. Do not claim full caller adoption until these paths use the captured
transport and original owners; changing presentation must not transfer authority
or fabricate successful settlement of an already issued room/native edit.

Desktop AI caller presentation acceptance: Formatting, strict workspace/all-target Clippy and 1,970 workspace tests pass (zero failed, 32 default ignored) under the aggregate 10% CPU/RAM cap. All 5 implementation snapshots match accepted source. Seven account/session/endpoint/daemon/server/sign-out/capability changes reject old replies, clear private prompt/title/search state and preserve the original Stop owner. Provider event forwarding retains the complete caller tuple. Runtime transport, native review/room acknowledgements and durable companion ownership remain unfinished; the parent inventory and ADR-204 remain partial/proposed.

Provider captured-transport adoption design: require the private ScopedAiClient at
the shared runtime, provider adapters, harness and Sift-only broker boundaries.
Desktop startup must obtain it from the original queued caller before policy,
chat, history, lease creation or model work. Do not rediscover a replacement
expected identity. Keep startup cancellation and run completion/Stop/deadline on
that same transport. Chat snapshots and public observation must also capture the
queued caller before reading private data, including post-apply refresh.

Existing native approval/review/setup, external-source vault management and room
proposal acknowledgement owners still need the same adoption; presentation scope
alone does not enforce receiving identity. Durable one-turn provider/companion
ownership and recovery remain required. Do not treat this transport change as
completed unattended delegation or cross-platform acceptance.

Remaining native caller adoption must preserve existing functionality and retain
actual owners. Approval/status helpers can accept ScopedAiClient, but review's
automatic managed connection setup must also capture before SessionCreate or
ConnectProfile and retain its original session/connection through preview/apply
settlement. Reusing an ordinary QueryContext client or disabling automatic setup
is not equivalent. Existing active session IDs may be reviewed only through the
original captured caller and receiving owner/source checks.

For legacy room-document proposal application, retain a bounded executor owner
with the captured transport before permitting the local edit/CRDT update. Return
an opaque owner/work identity to the original scoped view; completion after the
original flush addresses that owner, not a newly selected server/account. Discarded
view replies must not cause a new edit, and Stop/account switches must not invent
a successful acknowledgement or silently erase issued-work settlement. Reuse the
existing typed query-editor work/observation patterns while preserving Propose-mode
human application; do not replace it with a narrower Execute-only workflow.
External source management also needs captured vault discovery, not an ordinary
client escape beside guarded AI methods. These owners and their concurrency/late
response tests remain required before caller adoption is complete.

Native provider gate observations during captured-transport adoption: the reduced
build-service PATH initially omitted user-installed launchers. With the verified
installed launcher directories, Codex and Claude Code isolation/Sift-only probes
pass. OpenCode reaches the selected provider but returns a sanitized request error
on two attempts; full OpenCode acceptance is not proven. No model/authentication
preference is changed to make this gate pass. A bounded error-metadata classifier
now distinguishes reported HTTP failures and nested connection codes while never
surfacing messages, response bodies or credential bytes. Its tests exercise
redaction, invalid statuses, timeout metadata and depth bounds. All implementation
changes require fresh acceptance snapshots; prior native results alone do not
certify the modified error path.

Provider captured-transport acceptance: Formatting, strict workspace/all-target Clippy and 1,971 workspace tests pass (zero failed, 32 default ignored) under the aggregate 10% CPU/RAM cap. Installed Linux Codex and Claude Code isolation/Sift-only roundtrips pass with installed user launchers. The explicit OpenCode probe fails with reported HTTP 429; full OpenCode native acceptance remains pending provider capacity. All 9 implementation snapshots match accepted source. Real guarded-router startup coverage refuses seven missing/changed caller states before policy/chat/model work. Original startup-response cancellation and completion/failure/Stop/disconnect/deadline supervision pass through the fixed caller. Chat snapshots/public observation retain the captured client. The initial native gate refused missing/unsupported launchers on the reduced build-service PATH; the current capped validation uses verified installed user launchers and the exact workspace-built test artifact. The current OpenCode probe reports HTTP 429 and is not accepted as a passing native roundtrip; restore provider capacity and repeat that gate to complete its evidence. This commit records verified implementation and the outstanding external gate; it does not complete provider/platform acceptance. Native approval/review/source-vault/room-settlement adoption, durable companion ownership, public delegation and cross-platform acceptance remain unfinished; ADR-204 and the parent inventory remain proposed/partial.

Native approval caller integration: SQL and typed database approval/denial/status
and reconciliation capture the original queued AI caller before their first HTTP
request. Query edit approval and the retained private-editor observation/receipt
owner now accept ScopedAiClient, including acknowledgement after an original view
is dropped. The existing HTTP dropped-companion acceptance uses guarded capture.
Full verification remains pending. Managed legacy review setup, external source
vault discovery, legacy room proposal settlement and durable companion ownership
remain required; this change does not close ADR-204 or the parent inventory.

Complete ordinary AI view requests through the captured caller as part of native
caller adoption: publication and external-source management, chat/thread actions,
attachment previews, draft apply/discard/status and result selection must not
obtain an ordinary client beside guarded native approval. Add only typed guarded
vault-list and room-result-list aliases, reusing the original ACL/tenant/audit
handlers and OpenAPI/SDK contracts. Legacy room proposal acknowledgements should
refuse caller rebind now; their retained pre-edit transport owner still remains
required for reliable late settlement. Automatic legacy managed review setup also
remains pending and must keep its existing functionality.

Source-manager identity and guarded vault discovery run before a source mutation,
so a server without the required guarded metadata alias refuses before discovery,
activation or revocation. Later state refresh still uses the same captured caller.

AI publication/metadata requests use the existing bounded asynchronous request
pool so guarded identity discovery cannot stall the executor or queued Stop.
Issued room proposal acknowledgement keeps an independent task through observer
loss; a retained pre-edit owner is still required. Timeout feedback asks for state
refresh before retrying. The real HTTP responsiveness fixture now covers blocked
managed review setup and blocked captured publication requests while later
captured source reads complete, retaining each original reply scope.

Native approval/view caller acceptance: Formatting, strict workspace/all-target Clippy and 1,971 workspace tests pass (zero failed, 32 default ignored) under the aggregate 10% CPU/RAM cap. All 7 implementation snapshots match accepted source. The original private-editor owner settles a dropped work event through guarded HTTP without attesting an edit. Real HTTP tests cover guarded vault parity, room result lookup and wrong-principal refusals. OpenAPI tests verify required caller headers, unique operations and exact SDK coverage. Blocked managed review setup and captured publication requests leave later captured source reads responsive; reply identity is preserved. Native SQL/database/query approval and ordinary AI view requests now capture original callers. Legacy managed review setup and retained pre-edit room ownership, durable companion/delegation and the broader inventory remain unfinished. OpenCode native completion remains pending its reported HTTP429 usage limit; no new provider roundtrip success is claimed here.

Managed review ownership design: caller capture must precede proposal/source
preflight and any managed session/connection setup. Preserve explicit reuse of an
existing source, and preserve automatic setup when none is open. Retain the exact
source through preview and native settlement; session close purges plans and
requests migration cancellation, so dropping a connection on an observer timeout
is not a valid ownership boundary. A missing claim/status after a lost response
does not prove the request can never dispatch. Already issued work retains its
original owner and native EOF/settlement, without fresh credentials or reconnect.

First pin every trusted database review producer to the actual connection's
execution generation and enforce that field before any fresh native apply. Numeric
session/connection IDs and configured catalog identity do not prove that handle
survived restart or replacement. Read older encrypted reviews without changing
their digest, but require a fresh proven review for new work when generation is
absent. Exact already-claimed request replay still returns its original durable
receipt before source checks; it never issues work on the replacement handle.
Server-owned managed source leases are preferred for automatic review setup: they
can survive client/view loss and be retained directly by native settlement owners.
They must be bounded, caller/source/review-bound, and cannot turn an absent runtime
owner into an implicit reconnect. Keep managed source origin distinguishable after
restore/restart so a missing owner never falls back to an unrelated numeric handle.
The complete factory/lease/expiry/replay and room pre-edit ownership changes remain
required; connection generation alone is not the finished ownership capability.

Connection-generation regression evidence: the pre-fix real SQLite test reproduced
an accepted fresh apply through a replacement runtime reusing the same numeric IDs, catalog
identity and revision. Trusted review production now pins and rechecks the captured
source and records its execution generation in the signed encrypted body. Fresh
apply checks it before native catalog/claim/dispatch; exact previously claimed
request replay still observes the original receipt first. The focused fixed test
passes replacement refusal with unchanged native rows and original-request replay.
An additional compatibility case retains readable old signed bodies while refusing
fresh apply without generation proof; fresh full acceptance remains required.
This is runtime-handle proof, not native database/role attestation or completed
managed-source ownership.

Managed source lease implementation constraints: retain the lease on captured
connection/source clones so nested detached native/transaction workers keep it
through EOF and durable settlement, not just until the outer HTTP worker returns.
Keep the source slot occupied until physical connection close completes;
`close_session` schedules closes and returns before their execution barriers.
Registry expiry may remove its own reference while a native worker retains the
lease. Use weak links back to the original SessionStore to avoid cycles and stale
cleanup on a new runtime. An expired weak reference cannot be reclassified as an
unowned/manual source while its original session is still awaiting cleanup.
These constraints are design requirements, not shipped managed-source ownership.

Captured desktop review setup implementation: before the server-owned lease
factory lands, automatic desktop reviews must use a fixed `ScopedAiClient` for
session creation, profile connection, preview, and cleanup. Keep these sources
separate from ordinary query contexts so no general client or semantic task can
escape the captured caller. Reuse requires the same endpoint, caller, tenant,
and profile; retain at most eight prepared sources, including in-flight setup.
Already-open query sources remain reusable through a newly captured review
client. This transport step does not provide server-owned lifetime, expiry,
proposal preflight before connection setup, or native EOF settlement ownership.

Database review generation acceptance: Formatting, focused real SQLite regression, strict workspace/all-target Clippy and 1,971 workspace tests pass (zero failed, 32 default ignored) under the aggregate 10% CPU/RAM cap. All 7 implementation snapshots match accepted source. Pre-fix SQLite reproduction accepted fresh apply after numeric handle IDs/catalog identity/revision were reused. The fix rejects replacement handles with unchanged native rows, keeps old signed bodies readable while refusing their new dispatch, applies the proven original review once, and replays its durable receipt from the replacement runtime without issuing work. Managed source leases, native database/role evidence, room pre-edit ownership and full companion/delegation remain unfinished. OpenCode native acceptance remains pending reported HTTP429 capacity.

Captured desktop review transport acceptance: Formatting, both focused desktop regressions, strict workspace/all-target Clippy and 1,972 workspace tests pass (zero failed, 32 ignored) under the aggregate 10% CPU/RAM cap. The checked desktop source hash matches the committed implementation. HTTP regressions prove missing/mismatched caller refusal before session creation, original caller headers on setup and cleanup, bounded retained source ownership, and responsiveness while scoped setup is blocked. Automatic setup uses one captured connection and no ordinary query/semantic context; existing query connections remain reusable with tenant checking and captured preview. Server-owned leases/expiry, proposal preflight before connect, native settlement ownership, pre-edit room ownership and full delegation remain required.

#### Server-owned automatic review sources — ADR-207 milestone

The scoped-only managed-review endpoint derives its tenant/profile from the
trusted staged proposal. It checks actual principal/auth session against the
original instance/daemon/caller, then validates private source or room publication
and preview permission before setup and again immediately before native connect.
Canonical session/profile setup preserves authorization, resource limits, vault
use and audit. The SDK exposes the factory only on `ScopedAiClient`; unsupported
servers fail without ordinary-client fallback. Desktop review retains explicit
reuse of an existing matching query source, and otherwise requests this factory.
It no longer owns or closes automatic review sessions.

A durable V066 attempt identity is recorded before native connection setup.
An identical request can observe its retained original preparation/review; an
expired, failed, or replaced owner cannot implicitly reconnect. Nonces already
used for ordinary reviews are refused before connecting. Each proposal permits
at most 64 automatic attempts, and the process retains at most 32 sources, with
the existing settlement admission also bounding duplicate preparation owners.
Attempt identities follow proposal retention and coordinated tenant recovery;
no connection credentials enter this table.

A source is bound to its exact caller, proposal, request nonce and prepared review.
The registry expires its reference after five minutes; captured source entries,
bounded native workers and pending opens keep their original strong owner through
EOF and late cleanup. Weak session markers cannot become ordinary sessions after
expiry. Final cleanup retains its slot until physical close completes, including
when a revocation worker already removed the connection. General session/cursor
HTTP access is refused, and SessionStore source authority prevents body-based
query/transfer paths from acquiring these connections. Review/apply aliases bind
the owner only after checking actual original caller identity. Already-claimed
exact apply replay still returns its receipt before live source validation.

Acceptance: Formatting, four native lifecycle regressions, real SQLite review/apply/replay acceptance, both desktop regressions, strict workspace/all-target Clippy and 1,976 workspace tests pass (zero failed, 32 ignored) under the aggregate 10% CPU/RAM cap. All 21 implementation snapshots match.
Native database/role attestation, retained room pre-edit settlement, public
delegation, companion/scheduling recovery and the broader product inventory remain
required; this does not complete Execute mode.

#### Retained human room proposal commits — ADR-208 milestone

Room proposal approval now queues a scoped server commit before any editor
mutation. The desktop captures its original authenticated client and the server
retains bounded ownership through preparation, native document admission,
persistence and publication, independently of the original view/HTTP observer.
The existing acknowledgement endpoint retains its old semantics; it cannot
silently acquire a new editing side effect for older clients.

The server derives the original full query and target from immutable proposal/run
content, checks the exact reviewed content hash and base revision, and authors
through the normal DocumentActor. The Immediate transaction checks current SQL
document kind, room/tenant write permission, principal/session, original admission
deadline and shared AI mutation admission. It appends the CRDT update, settles the
proposal/event and records V067 before/after hashes, actor and document sequence
atomically. Deadline expiry refuses future admission while an admitted commit
retains its owner through settlement. No database SQL is executed.

Receipts survive document update-log compaction. Exact replay observes immutable
proof before considering current text; a changed approval, different actor or
legacy acknowledgement cannot produce another edit or a server receipt. Current
caller/chat authorization controls delivery separately from the committed fact.
Normal room broadcasts update open editors; the AI reply only updates proposal
status. View rollover drops pending UI state and rejects old replies without
canceling issued server work. The SDK exposes commit and receipt inspection only
through the captured client. Receipt ownership follows proposal retention and
coordinated tenant recovery.

Acceptance: Formatting, three room integration tests, the native scoped/no-optimistic-edit UI regression, strict workspace/all-target Clippy and 1,980 workspace tests pass (zero failed, 32 ignored) under the aggregate 10% CPU/RAM cap. All 22 implementation/fixture snapshots match.
The integration cases cover durable compaction/replay, current caller refusal,
original observer loss, stale authority/text, deadline and atomic tail rollback;
the UI queues captured intent and leaves local CRDT editing to the room broadcast.
Durable private companions, public delegation and unattended/scheduled recovery,
native database/role proof, deferred platform gates and the broader inventory
remain required.


Individual delegation withdrawal (ADR-204, implementation in progress): metadata
entry points now deny an unapproved review or revoke an unused review/original
run grant using the initiator and exact review fingerprint. An immediate
transaction closes the active grant, cancels the running run and appends one
stopped event. A consumed review retains its immutable original run link;
withdrawal never refunds reservations or rewrites issued native action journals.
Exact retries return the revoked run identity again so the receiving owner can
retry cancellation notification after a lost response. Withdrawal does not
require decryption of unavailable review content, live source access or currently
enabled execution policy. Public human routes still need captured caller checks,
auditing and original-owner cancellation delivery; no provider or unattended
authority is exposed by these metadata entry points. ADR-204 stays proposed and
the parent inventory remains partial.

Individual withdrawal acceptance: Formatting, all 188 metadata tests, strict workspace/all-target Clippy and 1,982 workspace tests pass (zero failed, 32 ignored) under the aggregate 10% CPU/RAM cap. All 4 implementation snapshots match. Cases cover exact fingerprint/initiator checks, denial versus revocation before approval, missing encrypted content, cancellation failure rollback, concurrent retry with one stopped event and repeated original-owner notification, stale startup refusal, rejection of reserved but unissued native work, and late truthful settlement of issued SQL. This proves internal authority transitions; receiving HTTP/SDK/Vim owners, native material proof and unattended lifecycle remain unfinished.


Retained native migration review material (ADR-204): all
three native preview producers now attach opaque renderer evidence containing
the original forward-statement change material and a full-plan content hash.
Diagram and schema-diff preview reads bind the original connection before native
catalog work, matching AI preview ownership. Registry lookup intersects the
original principal/tenant/profile/policy, proposal, native generation and expiry;
AI human migration approval checks that retained plan against the encrypted
review. Validation and apply reject a replaced connection generation, and
validation binds its native source through catalog/transaction work. Synthetic
settlement fixtures may retain plans without evidence but cannot produce review
material. Implicit child DDL remains in its parent command review rather than
being counted as another dispatch. This supplies actual material for exact
review; full bounded native command/object proof and native database/role
attestation, public human lifecycle and companion/scheduler recovery remain
unfinished. ADR-204 and the parent inventory remain proposed/partial.

Retained migration material acceptance: Formatting, two focused native migration review regressions, real SQLite AI migration review/apply, strict workspace/all-target Clippy and 1,984 workspace tests pass (zero failed, 32 ignored) under the aggregate 10% CPU/RAM cap. All 7 implementation snapshots match. Native tests cover emitted change identities and implicit child commands, altered SQL with copied digest, changed native run/expiry, original registry scope and missing evidence, and generation replacement before review/validation/apply without native catalog or transaction work. This does not prove bounded command effects or native database/role identity; public delegation and unattended lifecycle remain unfinished.
