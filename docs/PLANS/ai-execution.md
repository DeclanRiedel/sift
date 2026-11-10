# Governed AI execution and unattended runs

Status: interactive human-approved SQL, typed database and query text Execute
implemented and verified, opt-in only. Broader execution and unattended operation remain a design/implementation
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
scope before disclosure. Native admission and output delivery are separate.

At actual supervisor writer admission, recheck current registration, original
actor/token/context/policy, package/selection and tenant allowance after readiness
or writer waits. A closed deadline or revoked source refuses before a new frame;
a partially written frame or missing/invalid response is unknown. Retain the owner,
quota, original actor/correlation and shared shutdown guard after HTTP caller loss
or observation timeout. Never re-resolve a newer invoker midway through dispatch.

Keep separate Claimed, Dispatched, Acknowledged, Refused and OutcomeUnknown states.
Acknowledged means a schema-valid extension response, not native database commit,
rollback or proof of purity. A refusal is known only before frame admission. Late
responses observed during cancellation grace must remain observable evidence;
workflow timeout cannot erase them. Audit the terminal RPC observation once.
Startup/offline recovery preserves known states, refuses unadmitted claims, and
marks unresolved admitted RPCs unknown. Tenant recovery must not revive approval
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
