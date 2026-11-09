# Governed AI execution and unattended runs

Status: interactive human-approved SQL Execute implemented and verified, opt-in
only. Broader execution and unattended operation remain a design/implementation
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
`sift-server/live-mssql`. A single combined workspace test invocation remains
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

### Next native provenance work before typed delegated apply

Source inspection confirms that both the legacy AI migration ledger and direct
migration ledger currently use `MigrationRun.id` as `transaction_id`.
`MigrationApplyObservation` retains a workflow run, not native group transaction
witnesses. Row-edit capture already has an `EditsApplyObservation` with attempted
kinds, native transaction ID and rollback observation, while the guarded AI path
currently returns only `ApplyEditsResult`. The legacy AI count sum is unchecked.
These are concrete prerequisites to typed delegated execution, not evidence that
its native provenance is complete.

The next coupled change must carry native observations out of the existing
execution owner, rather than infer them from a successful workflow response:

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
group witnesses in their own versioned/default-empty metadata column, validate
their plan ordinals and transaction boundaries on write, and read old rows as
unobserved. Do not silently replace that array with a new envelope understood
only by newer readers. Any feature-use minimum-reader floor must be raised in
the same transaction that first persists those witnesses, and tenant restore
must propagate it just as it does for SQL execution content.

Use a new audited read endpoint,
`GET /v1/metadata/tenants/:tenant/migration-runs/:run/native-outcomes`, with the
same tenant/run authorization as the existing durable run read. Its pure-serde
response carries a format version, run and plan identity, and bounded observations;
it includes fingerprints and native outcomes, never SQL or diagnostic text.
Add a corresponding Operation variant, SDK method and coverage-manifest entry.
This keeps existing `MigrationRun` responses readable: they reject unknown
fields, so simply adding a defaulted response field would break old consumers.
The internal settlement owner reads the same persisted observation contract,
without a second native execution path. Before graduating the design into an
ADR, prove old-row reads, restart preservation, endpoint authorization, reader
floor propagation and native outcome/ledger correspondence.

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
