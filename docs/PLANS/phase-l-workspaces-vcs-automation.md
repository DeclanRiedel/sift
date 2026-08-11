# Phase L — Workspaces, VCS, And Execution Automation

Status: accepted implementation contract; ADR-034 is graduated. Implementation
is in progress.

Milestones: L0 contract lock, L1 virtual workspaces/history, and L2 projections
and offline DDL sources are complete; L3–L7 remain.

This plan expands Phase L in `server-build-list-v2.md`. It is intentionally
ordered design-first: slice L0 locks the topology before any public workspace
contract or metadata migration lands.

## Outcome

Phase L gives every thin client the same durable database-development model:

- SQL files and folders survive client and server restarts;
- collaborative SQL text retains the existing Loro document contract;
- local history, offline DDL models, and live-database mappings are
  server-authoritative;
- an optional server-side repository projection provides Git status, diff,
  commit, fetch, and push without assuming the client can see server files;
- run configurations execute immutable multi-script manifests with durable
  state, cancellation, scheduling, and safe recovery; and
- import/export recipes stream through bounded core orchestration, with
  untrusted formatters kept behind the Phase I process boundary.

The first useful vertical slice is a virtual workspace containing collaborative
SQL files, not Git or scheduling. Each later slice must leave the workspace
usable without the next one.

## Existing foundations

Phase L extends these contracts rather than replacing them:

- ADR-007: rooms are the durable collaboration boundary.
- ADR-008: SQLite contains only opaque secret handles.
- ADR-014: Loro is used only for query text.
- ADR-020: central authorization is deny-wins and server-owned.
- ADR-021: an SSH remote keeps product state on the remote server.
- ADR-022/031: extensions are supervised, capability-scoped, and audited.
- ADR-032/033: semantic documents and catalog graphs are server-owned.
- Phase D export and CSV import already provide bounded database data paths.
- Phase G documents already provide durable Loro snapshots, update logs,
  offline catch-up, and room membership checks.
- Phase J provides OpenAPI/SDK parity gates and state backup/restore.

`V003__workspaces.sql` introduced an older principal-owned
workspace/session/tab model. ADR-007 superseded it, and
`V006__rooms.sql` already dropped `tab`, `session_snapshot`, and `workspace`.
L1 creates a new room-owned schema; no legacy rows are reinterpreted as
collaborative resources.

## Zed Git architecture adopted as design input

Phase L's Git boundary was checked against Zed commit
[`c6b01d8a`](https://github.com/zed-industries/zed/tree/c6b01d8a203a6dc8c06a89e05ee4dec69de55cf0)
(2026-08-11). Sift adopts the architectural patterns, not Zed's UI or source
code:

- a repository trait separates canonical VCS operations from state/UI
  orchestration;
- status, staged state, branches, diffs, and remote results are parsed into
  typed models rather than exposing Git porcelain output as product API;
- Git runs on the machine owning the worktree, so remote clients receive state
  updates and invoke operations without accessing that filesystem;
- path-level pending operations make in-flight stage/unstage work explicit;
- commands use a fixed Git executable and structured arguments without a
  shell; and
- untrusted repositories disable hooks, credential helpers, external diff,
  fsmonitor, extension protocols, pagers, and interactive optional behavior.

Sift strengthens these patterns for a hosted server: the virtual workspace is
canonical, every Git action is admitted/audited, credentials come only from a
one-operation `SecretStore` helper, and output/process lifetime is bounded.

## Non-goals

- A client-only project model or transparent synchronization of arbitrary
  client folders.
- CRDTs for paths, folders, Git state, run state, logs, results, or catalogs.
- A general remote filesystem browser or arbitrary server path access.
- Replacing Git hosting, code review, CI/CD, Flyway, Sqitch, or similar tools.
- Automatically merging SQL or replaying an indeterminate write after a crash
  or transport loss.
- Running shell snippets as run-configuration tasks.
- Loading formatter or VCS plugin code into `sift-server`.
- Scheduling connected-database backup/restore; that remains a separate Phase J
  surface.

## ADR-034 — canonical virtual workspaces with optional projections

### Graduated decision

A workspace is a tenant resource attached to exactly one room. The room owns
membership and collaboration; the workspace owns a revisioned virtual tree,
configuration, history, repository bindings, DDL sources, recipes, and run
configurations. A one-member room gives local mode the same model.

The canonical file identity is an opaque server id. A normalized relative path
is mutable metadata, not identity. SQL file content is held by the existing
room `document` and Loro update log; a workspace file points to one document
instead of storing a second text body. Non-SQL assets are not part of the first
workspace slice. L6 may add immutable artifact-backed file nodes for recipe
output; those bytes do not use CRDTs.

Filesystem and VCS integrations are projections of a committed workspace
revision:

```text
thin client
    |
    | HTTP + room WebSocket
    v
server-owned workspace tree ----> room documents (Loro SQL text)
    |                                      |
    | explicit reconcile/materialize       | immutable content checkpoint
    v                                      v
root-confined server checkout -----> VCS adapter / run manifest / DDL model
```

The virtual tree works with no filesystem binding. A binding is allowed only
under an operator-configured workspace root and is identified by an opaque
binding id. Protocol payloads use workspace-relative paths; they never accept
an arbitrary absolute server path. Symlinks, hard-link escapes, `..`, device
files, case-fold collisions, reserved names, and root changes are rejected or
reported as explicit conflicts before mutation.

A desktop client may import/export a local folder through public APIs, but its
filesystem never becomes hidden authoritative state. On an SSH remote, the
checkout and Git process live beside the remote server. On a network-hosted
instance, operators may disable filesystem and VCS bindings while virtual
workspaces and automation remain available.

### Conflict model

- Tree mutations use an expected workspace revision and fail with a typed
  precondition conflict.
- SQL text converges only through the existing Loro document channel.
- A projection records the workspace revision and per-file content digest it
  last materialized or imported.
- Reconcile returns a deterministic plan with `unchanged`, `workspace_only`,
  `projection_only`, `both_changed`, `renamed`, and `deleted` entries.
- Reconcile never silently chooses a side for `both_changed`. The caller must
  import, overwrite, create a sibling, or abandon through an audited operation
  preconditioned on the same observed digests.
- Git operations consume one immutable materialization revision. A commit
  cannot accidentally include later collaborative edits.

### History model

Loro update history is collaboration machinery, not the user-facing local
history API. Phase L adds immutable workspace checkpoints containing the tree
revision, file identities, content digests/frontiers, author, timestamp, and
reason (`automatic`, `named`, `before_reconcile`, `before_run`, or
`before_vcs`). Checkpoint content is deduplicated and retention-bounded.
Restore creates a new head revision; it never rewrites history. Restoring SQL
text submits a server-authored, audited Loro replacement through the document
actor so connected replicas can resynchronize.

### Consequences

- Workspace metadata must be room-aware and cannot reuse the legacy
  principal-owned table as-is.
- File rename/move does not change document identity or invalidate semantic
  identity derived from content.
- Git and filesystem availability become explicit capabilities.
- Backups must include workspace metadata, checkpoints, execution definitions,
  and portable projection state, but not arbitrary checkout bytes or Git
  credentials.
- A future GUI can render client-local conveniences without changing the
  server contract.

### Locked v1 selections

- A room may contain multiple workspaces; a workspace belongs to one room.
- L1 exposes folders and SQL documents. Immutable artifact nodes arrive with
  recipes; general editable text and binary files are deferred.
- Automatic history checkpoints occur at meaningful operations, not on a
  timer. Users may also create named checkpoints.
- V003's legacy tables were already retired by V006. L1 uses new room-owned
  tables and has no legacy importer or implicit reinterpretation path.
- Git lands local-first (status/diff/stage/commit), followed by authenticated
  fetch/push. Clone, merge, rebase, force-push, and forge UI are deferred.
- A schedule is owned by a normal principal. Service principals and event
  triggers are deferred.
- Run steps are SQL documents or governed tools declared schedulable; arbitrary
  shell commands are excluded.
- Values use typed parameters, identifiers use separately validated
  substitutions, and raw untyped string substitution is forbidden.
- Schedules use five-field cron plus an IANA timezone. Writes never retry
  automatically.
- XLSX import/export remains in Phase L alongside HTML and Markdown export.
- Phase L is additive under ADR-016 and retains protocol v1. A later breaking
  contract change would require a deliberate bump.

## Resource model

The protocol should use opaque ids and optimistic revisions throughout:

- `Workspace`: room, name, head revision, capability summary, timestamps.
- `WorkspaceNode`: stable id, parent id, normalized name/path, folder, SQL
  document, or immutable artifact kind, optional content reference, metadata
  revision, and timestamps. L1 exposes only folders and SQL documents.
- `WorkspaceCheckpoint`: immutable tree revision plus file content references,
  author, reason, and retention metadata.
- `ProjectionBinding`: adapter id, opaque configured root, mode, last observed
  workspace revision, adapter generation, and health. Absolute paths remain an
  admin-only inspection detail.
- `RepositoryBinding`: projection id, VCS adapter id, repository identity,
  branch/head observations, optional credential handle, and revision.
- `DdlSource`: dialect, included workspace roots/files, derived model revision,
  diagnostics, and zero or more live connection/schema mappings.
- `RunConfiguration`: ordered steps, target profile/schema, variable schema,
  secret-handle references, transaction/error policies, pre-tasks, schedule,
  enabled state, and revision.
- `Run`: immutable resolved manifest, trigger/actor, lifecycle state,
  cancellation state, step attempts, bounded logs, and timestamps.
- `TransferRecipe`: direction, source/sink, format contribution and version,
  validated options, transformation steps, destination policy, and revision.

All list surfaces use additive keyset pagination from their first release.
Potentially large diffs, logs, artifacts, and transfers are streamed or paged.

## Authorization baseline

L0 must encode these as central policy inputs, not handler-local exceptions:

| Action | Minimum room authority | Additional gate |
| --- | --- | --- |
| Read tree/history/DDL model, VCS status/diff, run/log, or artifact | Viewer | Tenant visibility and resource ownership |
| Edit SQL/tree, restore a checkpoint, or edit a run/recipe | Editor | Optimistic revision and tenant quota |
| Reconcile a projection, stage/commit/fetch, or run interactively | Editor | Instance capability plus operation classification; database runs also require profile/SQL policy |
| Bind/unbind a server root, repository, or VCS credential | Owner | Instance-admin configured root/grant; secret-handle ownership |
| Push to a remote or apply projection overwrite/delete | Editor | Explicit write/destructive authorization and approval when policy requires it |
| Create/enable/disable a durable schedule | Owner | Current principal is stored as schedule owner; target/profile policy is rechecked at every occurrence |

Tenant and instance administrators retain their existing administrative
controls, but administration does not manufacture room membership. Capability
discovery must explain when filesystem, Git network access, a provider, a
format contribution, or scheduling is unavailable.

## Security and reliability invariants

1. Every user-visible mutation or execution has a typed `Operation` variant;
   raw SQL, variable values, formatter input, Git credentials, and secret bytes
   are excluded from audit summaries.
2. Workspace access first resolves room membership, then applies the Phase F
   capability/policy evaluator. Repository and run actions cannot widen it.
3. Secret values are resolved just in time from `SecretStore`, delivered only
   to the admitted connection or supervised helper, dropped promptly, and
   zeroized where the owning buffer supports it. SQLite stores handles only.
4. Filesystem operations are root-confined and use descriptor-relative or
   equivalent race-resistant traversal. Validation followed by a raw joined
   path is insufficient.
5. The bundled Git adapter invokes a fixed executable with structured argv and
   no shell. Credentials do not appear in URLs, argv, inherited environment,
   stdout/stderr, or Git config. Network authentication uses a bounded
   one-operation helper channel backed by a scoped secret handle.
6. Git output, formatter frames, logs, artifacts, file counts, bytes, process
   time, and concurrency have instance ceilings and tenant admission guards.
7. A run captures exact workspace node ids, content digests/frontiers,
   formatter/provider versions, variables, and target identity before its
   first database action. Later edits affect only a later run.
8. Database work retains the existing timeout, cancellation, driver isolation,
   transaction, and query-history paths. The automation executor never invokes
   a driver inline or invents a second connection lifecycle.
9. A lost lease marks an active write step `outcome_unknown`; it is never
   automatically retried. Read-only steps may be retryable only when the
   immutable manifest and explicit policy allow it.
10. A schedule is owned by one durable principal; v1 has no separate delegated
    authority. Scheduled runs re-evaluate that principal's current tenant role,
    policy, profile access, extension availability, and quotas at admission
    time. Deletion or revocation disables execution rather than preserving
    ambient authority.
11. Pre-tasks are a bounded ordered list of core operations or Phase I tools
    declared `schedulable`; no shell commands, recursive runs, or implicit
    administrator identity are allowed.
12. Plugin formatters and adapters cross the Phase I framed RPC/supervision
    boundary. Core validates schemas, record/byte limits, cancellation, and
    output before committing product state.

### Locked initial ceilings

The first implementation enforces 32 workspaces per room, 10,000 nodes per
workspace, 100 mutations per atomic tree batch, 200 checkpoints per workspace,
64 MiB of captured snapshot bytes per checkpoint, 256 MiB of distinct retained
checkpoint content per workspace, and 100 rows per checkpoint-history page.
Later slices add their process/diff/run/artifact ceilings before exposing the
corresponding capability.

## Execution semantics to lock before L4

### Script and variable resolution

A run step references a workspace SQL file by stable node id and chooses either
`pinned` content or `latest_at_run_start`. Both resolve to an immutable digest
in the run manifest. SQL variables are declared with a type and required flag.
Non-secret input may be supplied per invocation and is stored only when the
definition permits it. Secret variables contain handles, never values.

Substitution is dialect-aware and limited to declared placeholders outside
comments and string literals. It must reuse the Phase K parse artifact where
possible. Raw string replacement is not acceptable. The driver receives the
resolved SQL, but query history stores the template plus redacted variable
metadata, never a secret-substituted statement. Resolved SQL also stays out of
audit and automation logs.

### Transaction and error policy

V1 supports these explicit transaction policies:

- `none`: each script uses normal connection behavior;
- `per_script`: one transaction per script; and
- `all_scripts`: one transaction for the full SQL step sequence when the
  provider supports it and no pre-task crosses the transaction boundary.

Error policy is `stop` or `continue` for `none`/`per_script`; `all_scripts`
always rolls back and stops. Configuration validation rejects unsupported or
ambiguous combinations before a run is created. SQL Server batch separators
and PostgreSQL statement boundaries use the dialect service, not newline or
semicolon splitting.

### Durable state machine

```text
queued -> admitted -> preparing -> running -> succeeded
   |         |            |          |------> failed
   |         |            |          |------> cancelled
   |         |            |          `------> outcome_unknown
   |         |            `-----------------> blocked
   |         `------------------------------> rejected
   `----------------------------------------> cancelled
```

State transitions use revisions and an owner lease/heartbeat. Startup reclaims
expired `queued`/`preparing` work, but an expired `running` database step is
marked `outcome_unknown`. An explicit audited rerun always creates a new run id
pointing to the prior immutable manifest; it never changes the old record.

Schedules store an IANA timezone, normalized schedule expression, next fire
time, misfire policy (`skip` or `run_once`), and concurrency policy (`forbid`,
`queue_one`, or bounded parallelism). The scheduler uses a database lease so
only one server process enqueues a due occurrence. Time changes and restart
catch-up are covered by deterministic clock tests.

## Import/export recipe contract

Core owns admission and streams canonical records between a source, zero or
more bounded transforms, and a sink. A recipe cannot grant access to a source
or destination that the caller could not use directly.

Bundled v1 formats:

- preserve the existing CSV, TSV, JSONL, and JSON-array export paths;
- add HTML table and Markdown table export;
- add XLSX export and import with explicit sheet selection and type handling;
  and
- route CSV import through the same decoder/record contract after parity is
  demonstrated.

Large output is a server-owned expiring artifact or an explicit workspace file,
not an arbitrary server pathname. HTTP consumers may stream the artifact.
Artifacts have content type, digest, byte length, expiry, and ownership.

An installed `import_format` or `export_format` contribution declares a
versioned JSON option schema and a streaming framed contract. Core never sends
database credentials or unrestricted filesystem access to a formatter.
Formatter crashes, malformed frames, size overruns, and cancellation leave no
selected partial artifact; workspace destinations use stage-and-commit.

## Ordered implementation

### L0 — Graduate topology and freeze contracts

1. Graduate ADR-034 in `docs/DECISIONS.md`.
2. Decide the explicit fate of the V003 legacy workspace/session/tab tables and
   add a compatibility-classified migration plan.
3. Add protocol types for ids, revisions, paths, conflict reports, capability
   flags, and stable errors. Keep `sift-protocol` pure serde.
4. Add typed `Operation`/`OperationKind` entries and authorization mappings for
   workspace, projection, VCS, DDL source, run, schedule, and recipe actions.
5. Classify the additive public surface and enum growth under ADR-016, deciding
   the protocol/release version change before fixtures or SDK code land.
6. Specify instance ceilings and capability discovery before exposing routes.

Exit: the topology, ownership, conflict, path, history, and secret rules are
reviewed; no unresolved choice can change public resource identity.

### L1 — Virtual workspace, SQL files, and history

1. Add room-owned workspace/tree/checkpoint metadata migrations and repository
   methods with tenant/room authorization and optimistic revisions.
2. Reuse `document` for SQL content; add a strict one-to-one file/document link
   and transactional create/delete rules. Folder and path changes remain normal
   metadata, never CRDT operations.
3. Add workspace/tree CRUD, batch mutation, checkpoint list/create/restore, and
   paged history routes.
4. Extend room events so clients observe tree-head/checkpoint changes while SQL
   text continues over existing document synchronization.
5. Add SDK methods and OpenAPI operation parity in the same change sets.

Exit: two clients can concurrently edit, rename, checkpoint, disconnect,
reconnect, and restore SQL files without duplicate content ownership or a lost
update.

### L2 — Projection and offline DDL sources

1. Introduce a server-internal workspace adapter boundary and a rooted
   filesystem implementation with scan, plan, materialize, import, and conflict
   resolution operations.
2. Add operator configuration for allowed roots and per-instance enablement;
   hosted mode defaults to virtual-only until configured.
3. Make reconcile planning read-only and deterministic. Apply requires the
   observed workspace revision and projection digests.
4. Add DDL sources over selected SQL files/folders. Reuse Phase K parsing,
   diagnostics, catalog graph identity, snapshot persistence, and schema diff.
5. Add explicit source-to-live connection/schema mappings and stale/partial
   coverage reporting; mapping never implies automatic database mutation.

Exit: the same workspace produces the same DDL graph locally and over SSH, and
projection conflicts require an explicit audited choice.

### L3 — VCS adapter and bundled Git

1. Add a VCS adapter contract for discover/init/bind, status, diff, stage,
   commit, branches, fetch, and push. Adapter responses use bounded canonical
   models rather than leaking porcelain output into the public protocol.
2. Implement bundled Git against one projection snapshot. Start with local
   repository status/diff/stage/commit, then add authenticated fetch/push after
   the one-operation credential helper and redaction tests exist.
3. Separate workspace reconcile conflicts from Git index/merge conflicts and
   report both with stable typed states.
4. Record executable/version/capability observations so a later adapter change
   cannot silently reinterpret a pending operation.
5. Add extension descriptors/RPC only after the bundled adapter contract passes
   conformance tests; other VCS implementations remain plugins.

Exit: a commit is reproducibly tied to one workspace checkpoint, concurrent
collaborative edits remain uncommitted until a later reconcile, and no
credential-shaped value reaches metadata, process inspection, logs, or audit.

### L4 — Run configurations and foreground execution

1. Persist revisioned run configurations, ordered steps, variables, policies,
   and immutable run manifests.
2. Build the executor as orchestration over existing session/connection/query
   paths, with admission guards, cancellation tokens, timeouts, and bounded
   logs/artifacts.
3. Implement `none`, `per_script`, and `all_scripts` policies plus `stop` and
   valid `continue` behavior for PostgreSQL and SQL Server.
4. Add safe core/tool pre-tasks and explicit target/profile/schema resolution.
5. Add create/validate/run/cancel/status/log/rerun routes, room events, SDK,
   OpenAPI, capabilities, query history, and audit.

Exit: an ordered multi-script run is deterministic from its manifest, cancel
does not wedge the server, and restart never auto-replays an uncertain write.

### L5 — Durable scheduler and recovery

1. Add schedules, occurrences, scheduler leases, misfire/concurrency policies,
   retention, and clock abstraction.
2. Re-evaluate identity, authorization, secrets, profiles, quotas, and extension
   generations for every occurrence.
3. Implement startup recovery and operator inspection for blocked, rejected,
   expired-lease, and outcome-unknown runs.
4. Add disable-on-revocation/unavailable-dependency behavior and explicit
   audited resume/rerun actions.

Exit: deterministic clock and process-kill tests prove at-most-one occurrence
enqueue, honest unknown outcomes, bounded catch-up, and no authority retention.

### L6 — Recipe-based import/export

1. Define canonical row/field streaming frames, recipe validation, artifact
   ownership, stage-and-commit, and formatter conformance fixtures.
2. Wrap existing formats without regression; add HTML, Markdown, and XLSX.
3. Activate Phase I `import_format`/`export_format` contributions through the
   supervised extension host with negotiated limits and cancellation.
4. Support interactive invocation first, then allow a validated recipe as a run
   step without creating a second scheduler.
5. Add SDK streaming consumers and exact OpenAPI/operation coverage.

Exit: large transfers remain bounded, plugin failure cannot publish partial
output, and interactive and scheduled recipes share one execution contract.

### L7 — Graduation and release closure

1. Run the deployment, conflict, security, determinism, compatibility, and
   performance matrices below.
2. Add current metadata fixtures and older-version compatibility coverage for
   every Phase L migration boundary.
3. Verify backup/restore round-trips definitions and history while excluding
   credentials, ephemeral artifacts, checkout bytes, live leases, and active
   runs.
4. Publish a Phase L graduation matrix and update README/build-list status only
   when all selected v1 surfaces pass.

## Test and graduation matrix

### Deployment

- Personal in-process, personal daemon, SSH-proxy daemon, network-hosted team,
  and container.
- Virtual-only workspace in every mode.
- Projection/Git enabled, disabled, missing, read-only, and root moved.
- Client disconnect/reconnect and daemon restart at every reconcile/run state.

### Collaboration and conflict

- Concurrent Loro edits plus rename/move/delete.
- Stale workspace revision and idempotent mutation retry.
- Projection-only, workspace-only, both-changed, rename/delete, case-fold, and
  symlink-race conflicts.
- Edits arriving between materialization and Git commit.
- Git index, branch, detached-head, merge, rebase-in-progress, and dirty-tree
  states are reported without destructive cleanup.

### Execution

- Both drivers across all transaction/error policies.
- Dialect batch boundaries, comments/strings around variables, missing/invalid
  variables, secret redaction, and target/schema changes.
- Cancel/timeout/disconnect/process kill before admission, between scripts,
  during a query, during commit, and while recording completion.
- DST changes, clock rollback/forward, misfires, lease takeover, duplicate
  scheduler processes, revocation, and quota exhaustion.
- Reruns point to prior manifests and never mutate prior records.

### Extensions and transfers

- Malformed/oversized frames, crash, hang, cancellation, version change, and
  unavailable formatter.
- CSV parity plus HTML/Markdown escaping and spreadsheet formula-injection,
  type, sheet, date, and size cases.
- Slow consumer backpressure and disconnect cleanup.
- Artifact expiry, authorization, digest, partial-write, and backup exclusion.

### Security

- Path traversal, absolute paths, Unicode/case aliases, symlink swaps, special
  files, hard-link escape, repository ownership, and unsafe Git config.
- HTTP and SSH Git credentials, malicious remote URLs, prompt suppression, and
  hostile stderr/stdout redaction.
- Cross-tenant ids, room membership changes, schedule-owner revocation,
  secret-handle substitution, and plugin permission narrowing.
- Audit payload scanning for SQL, bind values, variables, URLs with userinfo,
  secret bytes, and credential-helper material.

### Budgets

Set numeric ceilings during L0 and enforce them in tests for tree nodes,
checkpoint bytes/retention, reconcile scan entries, diff bytes, concurrent Git
processes, queued/running jobs, scripts/statements per run, log/artifact bytes,
formatter frames, schedule catch-up, and shutdown drain time. Graduation must
publish measured local and SSH-remote latency/throughput evidence rather than
leaving these as qualitative goals.

## Change-set discipline

Each slice should land as narrow, green changes:

1. pure protocol and operation vocabulary;
2. metadata migration, repository API, and compatibility fixture;
3. server-internal state machine or adapter plus unit tests;
4. authorization/audit/resource wiring;
5. HTTP/WebSocket routes;
6. SDK and OpenAPI parity;
7. cross-process and two-driver integration tests; and
8. documentation/ADR graduation.

Do not combine a new durable state machine, its public routes, Git process
execution, and scheduling in one change. Every change keeps `cargo fmt`,
`cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace` green.
