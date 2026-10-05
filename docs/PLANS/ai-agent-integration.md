# AI agent integration contract

Status: **Codex private-chat Test 1 complete and testable; broader AI integration open.**
ADRs 096–098 hold the accepted boundary. The later agent harness has its own
design phase. The product inventory in
`ide-parity-and-provider-extensibility.md` remains the wider feature checklist.

Test 1 includes the right AI dock, local signed-in Codex app-server bridge,
server-governed schema/diagnostics/estimated explain/bounded SELECT, ordered
encrypted chat events, Read/Propose mode, staged complete SQL drafts, and a
human apply path with a fresh SQL-content check. A room-document draft is
marked applied after its CRDT update is acknowledged and the reviewed SQL is
still present. The desktop uses Linux Bubblewrap for this first verified
adapter; unsupported Codex installations fail closed. The AI policy is enabled
in the demo `sift.toml` and disabled by default in other instances.

Test 1 is private. Forced room-public continuation, other provider adapters,
generic external MCP, row/migration proposals, per-tenant retention jobs,
and encrypted-content backup/restore remain later work. Tenant restore
currently refuses snapshots that contain AI chats, so it cannot silently
restore opaque handles without their encrypted blobs and keys.

### Test 1 checklist

- [x] Private chat creation, replay, and turn attribution to the initiating Sift user.
- [x] Locally signed-in Codex launch in a restricted mount namespace with no workspace mount.
- [x] Dynamic Sift tools: shallow schema, SQL syntax diagnostics, estimated explain, and bounded SELECT.
- [x] Read and Propose modes; human-reviewed SQL draft apply and discard.
- [x] Right AI dock, active context preview, live text, saved tool work log, and stop action.
- [x] Tool authorization, quotas, result bounds, audit, and failed/duplicate tool-call tests.
- [x] Signed-in Codex dynamic-tool roundtrip test (manual ignored test).
- [ ] Publication checks and room-public chat continuation.
- [ ] Production retention, key rotation, and encrypted-content backup/restore.
- [ ] Claude Code/OpenCode adapters and the later harness.

## Product contract

### Remaining implementation scope (2026-10-05)

The user requested decision prompts before a development loop and authorized
recommended defaults when no overrides were supplied. The following defaults
define that loop's scope; ADRs 096–098 remain authoritative. This section records
product choices, not completed implementation or provider validation.

- Complete Read/Propose integration on Linux. Execute mode, unattended runs,
  and Windows/macOS isolation validation remain separate follow-up scope.
- Target Codex, Claude Code, and OpenCode. Expose a per-chat provider/model
  selector, using the installed CLI's default model when none is selected.
  Unsupported versions/installations remain unavailable until restricted
  launch is verified. Missing provider installations or sign-ins must be
  recorded as validation blockers, never treated as passing evidence.
- Enable room-public viewing and continuation after server publication checks.
  Database rows require explicitly room-publishable connection resources;
  authorization and publication are checked before provider delivery.
- Add insert/update/delete and migration/schema proposals wherever existing
  typed preview/apply paths support them. Any currently authorized human
  reviewer may apply a shared proposal, retaining existing production
  confirmations and distinct author, approver, and executor provenance.
- Retain chat until explicit deletion by default. Add tenant-configurable
  expiry constrained by instance policy, resumable content-key rotation, and
  portable encrypted backup/restore with explicit recovery-key handling.
- Design and implement a Sift-specific bounded multi-step harness: governed
  tools, staged proposals, cancellation, and continuation from saved history
  with visible context truncation. General filesystem/shell agents are outside
  this scope. Continuation creates a new run after interruption.
- Support explicitly registered external MCP read tools through the governed
  gateway. External writes require an explicit supported proposal adapter and
  human apply; they never become direct agent mutations.
- Expand context/tools to execution errors, saved/analyzed plans, explicit
  result-row attachments, query history, and object DDL. Provide context
  disclosure and publication checks. Generating an analyzed plan requires
  human-authorized execution through the normal operation path.

Loop work order and completion gates:

- [x] Verify the existing private Codex baseline and preserve current changes.
      Milestone `802a92ae`; workspace format, strict Clippy, and tests passed.
      The signed-in isolated Codex dynamic-tool roundtrip also passed locally.
- [~] Design publication labels and checks; implement room-public workflows.
      ADR-099 defines committed room-document publication. Public SQL context,
      shared SQL draft review/application, visibility disclosure, and bounded
      incremental desktop observation are implemented. Public database tools
      still require explicit publication grants and a shared-credential path.
- [ ] Design typed row/migration proposal binding, review, and human apply.
- [ ] Add richer diagnostics, plans, history/DDL tools, and explicit attachments.
- [~] Design encrypted recovery/key lifecycle; implement retention, rotation,
      and coordinated backup/restore with meaningful failure-path tests.
      ADR-100 defines the coordinated recovery boundary. Versioned, identity-bound
      encryption and resumable tenant-admin key rotation are implemented, with
      legacy migration and failed-rotation regression coverage. AI routes now
      honor scoped API-token tenant membership. Retention and archive integration
      remain in progress.
- [ ] Implement provider selection and verified Claude Code/OpenCode adapters;
      broaden supported Codex installations with equivalent isolation proof.
- [ ] Design the bounded Sift harness and external MCP gateway before their
      tightly coupled implementation; graduate stable choices into ADRs.
- [ ] Validate authorization revocation, future room membership, stale proposals,
      cancellation/disconnection, bounds, replay/idempotency, recovery, and
      provider-native tool isolation. Cover all three supported database engines
      where a feature depends on engine behavior.
- [ ] Run `cargo fmt`, strict workspace Clippy, and workspace tests; reconcile
      the canonical product inventory with actual completion and remaining
      validation blockers. Keep CI manual-dispatch-only.

The Sift desktop runs an installed, already signed-in Codex, Claude Code, or
OpenCode provider for each active AI turn. Sift's server owns chat identity,
visibility, durable history, run state, governed tool dispatch, staged changes,
authorization, and audit. Provider credentials stay on the initiating desktop.
There are no Sift-managed model API keys in v1.

V1 has **Read** and **Propose** modes. Each run uses a Sift-managed AI
subprofile of its initiating user, with narrower Sift permissions selected by
Sift and rechecked per tool call. Read can inspect permitted Sift context and
use bounded read tools. A live SELECT may use that user's active database
credential, even when the database login has write grants. Sift restricts the
admitted SQL and enforces timeout, cancellation, row/byte limits, and audit.
This is Sift-restricted, not database-enforced read-only: database functions
invoked by SELECT can have side effects. The UI and audit must identify this
limit accurately. Propose can also create reviewable
drafts. Neither mode can apply a database or workspace change. A person applies
a proposal through the existing Sift operation after a fresh authorization and
revision check. Execute mode and unattended runs are later work.

Provider-native shell, filesystem, arbitrary network, and MCP tools are
unavailable to Sift AI sessions in v1. Provider traffic to its model service
still works. Only the Sift-governed tool gateway can touch product resources.
A provider adapter is available only after its launch path proves this
restriction; a prompt or provider approval setting alone is insufficient.

## Ownership and run flow

```text
desktop UI -> Sift server: start turn + selected context snapshot
server -> desktop: authorized run ID + bound run lease
desktop -> local provider: prompt + Sift tool definitions
provider -> desktop -> server: tool request
server: reauthorize concrete operation, enforce bounds, dispatch, audit
server -> desktop -> provider: bounded tool result
desktop -> server: model output and activity events
server: persist ordered events and proposals; relay to viewers
```

The server is authoritative for tool-call and proposal receipts. Desktop-sent
model text is untrusted content, not evidence that an operation occurred. A
run lease binds chat, turn, principal, desktop instance, mode, selected scope,
and expiry. It cannot authorize a different chat or target. Every tool call
checks current principal, room, connection, and policy; a run's initial check
never grants lasting access. The server must use the concrete `OperationKind`
for core tools, rather than infer authorization from a broad tool class.

One active run per chat prevents interleaved provider output and proposals.
Other room members can observe a public run and later start their own turn with
their own local CLI and credentials, if they have the needed Sift access. Every
turn identifies its initiating principal and provider kind/model; it never
implies a shared provider account. If the initiating desktop disconnects, the
CLI turn stops, the run becomes `interrupted`, and acknowledged events and
proposals remain. Continuing creates a new run in the same chat; it does not
claim the terminated CLI process survived.

Run events have monotonically increasing server sequence numbers and stable
IDs for deduplication and reconnect replay. Expected events include turn
started, message delta/completed, tool requested/completed/denied, proposal
created, interruption, and terminal outcome. Only the server emits canonical
tool and proposal outcome events. Cancellation reaches the provider child and
in-flight Sift operations; timeouts and quotas still apply.

### Activity timeline

Desktop AI chat has a dedicated right-side dock. It shares the right-side slot
with Inspector: opening AI hides Inspector, and opening Inspector hides AI.
Switching preserves each dock's scroll, selected chat, and inspection state.
The editor and result view remain visible in the center while chat is open.
The dock follows Sift's Vim-only interaction model.
Chat context follows the active IDE tab and selected connection. Switching
tabs or connections refreshes the pre-send context preview. Each sent turn
captures its own fixed snapshot; an in-progress run does not retarget when
focus changes. Closing a tab makes the preview follow the newly active tab;
if no SQL tab remains, the chat stays open and the next turn has no automatic
SQL context. Room-public context still omits private scratch SQL and requires
a publishable room document.

The first provider adapter targets Codex. A usable chat can ship when this one
adapter passes native-tool isolation tests; Claude Code and OpenCode follow
their own proof. Test 1 is a private chat with governed schema lookup, SQL
diagnostics, explain without analyze, and bounded Sift-restricted SELECT.
Codex uses the initiating desktop's signed-in CLI default model. Each new CLI
turn receives bounded recent saved turns and a visible notice when older turns
are omitted. Room-public continuation waits for publication checks. The local
T3 Code checkout provides a presentation reference:
keep messages, proposed changes, and work activity as distinct timeline rows;
normalize provider events into stable, sequenced activity records; group
consecutive work rows and expand them on demand. Sift's server remains the
authority for tool and proposal receipts.

The default chat view shows the latest work step during a run, with earlier
steps in a collapsible work log. Each step shows a short action, target,
running/succeeded/denied/failed state, and elapsed time. Expanded details show
the governed Sift tool, authorization decision, bounded counts, truncation,
and sanitized error. A staged proposal appears as its own review card with
target, kind, revision, diff/preview, authoring turn, and status; it does not
masquerade as an applied change. A room-public chat uses the same room-public
publication check for every visible activity detail.

Persist canonical, bounded semantic events: user/assistant messages, tool
request and server receipt, proposal lifecycle, run state, and interruption.
Do not persist provider raw event envelopes as the chat trace. Raw SQL bind
values, credentials, result rows, and unrestricted tool payloads stay out of
activity events and sanitized audit. Provider-supplied progress summaries
are displayed in the work log when available and retained under the same chat
retention policy. Raw model reasoning is not required or retained for activity
transparency.

The first API slice uses normal authenticated Sift endpoints to create/list/read
chats, start/cancel a turn, submit bounded desktop provider events, invoke a
tool, and create/review/apply/discard a proposal. A resumable event stream
accepts a last-seen sequence; server acknowledgments let the desktop retry
after a broken connection without duplicating a message or tool call. Every
mutation uses a client idempotency key or expected revision. New public
`Operation` variants cover chat/run/proposal lifecycle actions and enter the
audit sanitizer. Applying a proposal also emits its underlying operation,
correlated to chat, run, and proposal IDs. The desktop never supplies the
authoritative principal or approval identity.

## Visibility and publication

`sift.toml` sets one instance policy: `private` by default or forced
`room-public`. There is no per-chat visibility toggle in v1. A room-public chat
must belong to a room; every room member may read it. Changing the instance
policy affects new chats only. Existing private chats remain private and need
an explicit future migration workflow before sharing.

A room-public transcript may contain Sift-sourced context and tool data only
from room-shared resources readable by all room members, including members who
join later. A person can still type or paste sensitive text into a public
chat; the UI must make its visibility clear before sending. The server checks this
**publication eligibility** separately from whether the current turn's
initiator may invoke a tool. If eligibility cannot be established, the context
or tool result is excluded from the public turn; the tool must not first send
it to the provider and then attempt to redact the transcript. Private profile
data and local scratch SQL cannot enter a public chat merely because its
initiator can read them. A public chat's current SQL can be attached
automatically when it comes from a room-readable document or other explicitly
room-publishable source. If a private scratch tab is active, Sift omits its SQL
from automatic context until the user moves it to a room document; the UI shows
that omission before sending the turn.

Connection-policy changes and room membership changes must not reveal content
that was private when produced. Public content therefore needs a stable
room-public publication label, not a one-time enumeration of present members.
Chat read authorization and publication checks live on the server.

## Automatic Sift context

Each user turn captures the active Sift context with target IDs and source
revisions. The snapshot is stable for that turn; later tool calls obtain live
data under fresh authorization. The desktop includes:

- selected instance, tenant/room, connection profile and active database;
  provider/dialect, environment label, connection state, and read-only policy;
- active query tab or result source, title, document identity/revision, full
  current SQL within a bounded size, selection and current-statement offsets;
- current query error, semantic diagnostics, or plan reference when visible;
- transaction state and count of already staged edits.

For a result tab, SQL and connection provenance come from the execution that
produced the result, not the current editor or newly selected connection.
No credentials, bind values, or result rows are automatic context. Result rows
enter only through explicit user attachment or a separately authorized,
bounded tool result. Schema details, object DDL, plan bodies, history, and
other documents are fetched by tools. Room-public context also passes the
publication check above.

The UI shows which context fields will reach the provider before sending the
turn. Content sent to local provider CLIs may leave the machine under those
providers' own signed-in accounts; Sift does not claim local-only inference.

## Durable records

Proposed protocol entities (pure serde in `sift-protocol`):

- `AiChat`: ID, tenant/room, owner, visibility fixed at creation, title,
  created/updated timestamps, revision.
- `AiTurn`: chat ID, initiating principal, provider kind/model, mode, context
  reference, client idempotency key, created timestamp.
- `AiRun`: run ID, turn ID, desktop identity, status, limits, sequence cursor,
  start/end timestamps and reason.
- `AiRunEvent`: run ID, sequence, type, timestamp, bounded content reference,
  and correlation IDs; tool receipts are server-generated.
- `AiProposal`: immutable ID, source run, type, target, base revision or row
  identity, content digest/reference, preview, status, creator and timestamps.

Chat, message, event, and proposal persistence belongs to `sift-metadata`.
By default, chat bodies and proposals remain until their owner or a tenant
administrator deletes them. A tenant may set a shorter retention policy;
instance policy can bound that setting. Deletion removes content, not the
independent sanitized audit or database change ledger. To honor Sift's rule that
secrets never live in SQLite, SQLite stores metadata and opaque content
handles; potentially sensitive prompt, response, result excerpt, and proposal
bodies need a bounded encrypted content store outside SQLite. Provider
credentials never enter that store. The content-store key lifecycle and
backup/restore coupling require review before production use. Sanitized operation audit and
the database change ledger keep their independent retention and never copy
raw chat or proposal bodies.

The content store is owned by the Sift server, including for desktop-launched
AI. It uses opaque blob IDs, per-tenant encryption keys held through
`SecretStore`, atomic blob writes, and orphan cleanup after metadata commits.
The metadata and encrypted blobs must enter the same backup/restore lifecycle;
restore cannot claim chat recovery if either component or its key is missing.
Tenant admins can delete retained chats without gaining read access to private
chat bodies. Run traces store bounded model output and sanitized tool metadata;
raw tool arguments, bind values, credentials, and result bodies are not copied
into the trace.

## Proposal and apply boundary

Proposal types can cover query/document patches, parameterized row edit sets,
and migration or schema drafts. Each carries its source target and expected
revision; row changes also carry stable keys and expected original values.
Creating a proposal does not mutate the target. The server previews it through
existing typed paths where available. The desktop review shows exact SQL or
diff, target connection and environment, affected objects/rows where known,
and source agent turn.

A human applies or discards a proposal. Apply rechecks target binding,
authorization, policy, revision/conflict state, and any normal production
confirmation. The first SQL patch apply targets its original scratch tab or
room document and applies the complete reviewed diff; a stale or missing
target conflicts without modification. It then uses the underlying Sift operation (`ApplyEdits`,
document update, migration apply, etc.). Audit records the human executor plus
chat/run/proposal correlation. The change ledger keeps separate author,
approver, and executor provenance. Agent output cannot mint approvals or
become an execution receipt.

## Tool and MCP boundary

The gateway unifies first-party Sift tools and the existing governed extension
registry. Every descriptor declares input/output schema, concrete Sift
operation or extension classification, context requirements, result ceilings,
and whether its output may be published to a room. Listing filters by current
scope and mode; invocation repeats all checks. Tool output is data, not an
instruction with authority over later calls.

First-test core tools: describe selected connection and policy; search schema;
inspect an object; diagnose current SQL; explain without analyze; and execute
one bounded Sift-restricted SELECT through the initiating user's active
database credential. The SELECT tool runs within policy without a per-query
approval prompt; its activity, limits, and cancellation stay visible.
Propose adds draft/query-document patch and typed row-edit proposal tools.
Migration/schema proposals follow after their existing preview paths can
return a revision-bound draft. Existing extension tools classified as direct
write, destructive, or administrative are hidden from both v1 modes; a future
proposal adapter must be explicit rather than treating approval as a draft.

Initial hard ceilings for design validation: 64 KiB of automatic SQL context,
64 KiB per tool result, 100 rows per live read, 20 tool calls and 10 minutes
per run. Instance policy may lower these. Truncation is explicit in tool output
and the activity trace, never silently presented as complete data. Final
values should be validated against existing Sift result and quota limits.

The existing `sift mcp` server remains an external client surface. Sift AI
uses the same governed Sift operations through its desktop companion. External
MCP servers, generic provider-native tools, and a broader harness are outside
v1. They must not bypass this gateway when added.

## Implementation sequence after design approval

1. Prove a restricted launch for each of Codex, Claude Code, and OpenCode using
   existing desktop sign-in, with native tools and unrelated user MCP servers
   unavailable. Mark unsupported providers unavailable rather than weakening
   Sift permissions.
2. Add manifest policy, protocol entities, metadata schema/content storage,
   retention, chat ACLs, and ordered run-event transport.
3. Add core governed read tools and the desktop companion bridge. Verify
   authorization, room publication, cancellation, bounds, reconnect, and
   disconnected-run behavior.
4. Add SQL/document proposals and review, then row edits and schema drafts
   through existing typed preview/apply paths. Link human apply to audit and
   the change ledger.
5. Add Vim desktop chat, context disclosure, activity trace, proposal review,
   and public-room viewing/continuation.

## Acceptance conditions

- A private chat is visible only to its owner; forced public applies only to
  new room chats; a later config change does not expose old private content.
- Sift cannot auto-publish private profile content, private scratch SQL, or
  tool output that lacks room-public eligibility into a public chat.
- A Read or Propose agent cannot use provider-native tools or execute a Sift
  mutation, even with a crafted prompt, tool arguments, or provider config.
- Agent SELECT goes through the user's Sift AI subprofile and Sift's constrained
  admission path, never a provider-native database connection. Its UI and
  receipt label it Sift-restricted, not database read-only. Side-effecting
  SELECT functions are a known limit of this chosen policy.
- Policy revocation blocks the next call; stale proposals fail without
  changing the target; a human apply records distinct actor provenance.
- A disconnected desktop stops its provider run, while acknowledged events
  and proposals replay without duplication after reconnect.
- AI provider credentials, AI tool bind values, and raw AI tool result rows are
  absent from SQLite, logs, and sanitized audit; content retention deletes chat
  bodies and proposals without altering independent audit/ledger records.

## Remaining engineering validation

- Exact `sift.toml` field names and tenant retention API should follow the
  manifest's existing schema/lock pattern. Default retention is until explicit
  deletion; initial run/tool ceilings above are design targets.
- Engine-specific SELECT admission tests, including side-effecting functions,
  multi-statements, SELECT INTO, procedural calls, and cancellation.
- Encrypted content-store path, key rotation, and backup/restore mechanics.
- Provider-specific restricted-launch proof and fallback behavior when an
  installed provider cannot meet it.
