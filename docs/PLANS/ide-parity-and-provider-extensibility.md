# SQL IDE and DBMS Feature Coverage

Status: **active-development feature inventory, reconciled 2026-09-06.** This is
the canonical checkbox list for choosing product features. It is not a release
roadmap and has no release date, version scope, beta boundary, or claim of
readiness. A checked feature may still need hardening, accessibility,
performance, documentation, and cross-platform work.

Legend: `[x]` usable feature slice exists · `[~]` partial or server-only · `[ ]`
missing. Choose new feature work from `[~]` and `[ ]`; use the [desktop
plan](gpui-desktop.md) for desktop architecture and validation work.

For the bounded PostgreSQL/SQL Server support milestone and next-provider
sequence, use the [driver graduation checklist](postgres-sqlserver-graduation.md).
Core Driver contract remains locked; scoped PostgreSQL/SQL Server graduation
is recorded in ADR-055 and the linked acceptance evidence.
The [PostgreSQL feature map](postgres-feature-map.md) traces current PostgreSQL
paths from driver to API, SDK and desktop, including backend-only boundaries.

## SQL IDE

### Connections and workspace

- [x] Connection profiles
- [x] Multiple saved connections
- [x] Connect and disconnect
- [x] Ad-hoc connection editor
- [x] Connection testing and health
- [x] SSL/TLS configuration
- [x] SSH remote connections
- [x] Read-only connections
- [x] Secret-store integration
- [x] Provider capability discovery
- [x] Connection folders, tags, and favorites
- [x] Environment labels and colors
- [x] Production mutation confirmation
- [x] Startup SQL and session variables
- [x] Persistent editor workspace
- [x] Multiple windows
- [x] Virtual workspaces
- [x] Filesystem projection
- [x] Git integration

Connection organization is stored in profile tags and is searchable and
editable. Provider-specific session variables and startup SQL are applied at
the physical driver boundary. SSH profiles own bootstrap, NixOS runtime
negotiation, short-lived access renewal, and forwarding. New desktop windows
have independent workspace/runtime supervision; only the primary window owns
the single presentation-state writer, avoiding cross-window overwrite races.

### Explorer and navigation

- [x] Database and schema tree
- [x] Tables, views, and columns
- [x] Indexes and constraints
- [x] Functions, procedures, and types
- [x] Lazy metadata loading
- [x] Metadata refresh and invalidation
- [x] Schema search
- [x] Data search
- [x] Object DDL view
- [x] Dependency and dependent graph
- [x] Foreign-key navigation
- [x] Global fuzzy object search UI
- [x] Recent and favorite objects
- [x] Object filters and saved explorer views
- [x] Breadcrumb navigation
- [x] Peek definition
- [x] Active-connection Objects table

The Connections explorer persists identifier-only recent/favorite shortcuts
and named object-type views. Database-backed tabs expose clickable
connection/catalog/schema/object breadcrumbs. `Shift+P` peeks canonical DDL
without creating a tab. The active-connection Objects tab provides a compact
catalog-style overview with provider metadata for estimated rows, modification
time, and comments when available, plus open, create-table, design, confirmed
delete, import, and export workflows.

### SQL editor

- [x] SQL editor tabs
- [x] Syntax highlighting
- [x] Execute document, statement, and selection
- [x] Query cancellation
- [x] Streaming and paged results
- [x] Parameterized execution
- [x] SQL formatting
- [x] Query history
- [x] Saved queries
- [x] Find and replace UI
- [x] Line numbers and editor gutter
- [x] Split editor panes
- [x] Scratch SQL query tabs
- [x] Snippets and templates
- [x] SQL variables
- [x] SQLite native named/anonymous/numbered binding across execution,
      Benchmark/Profile, templates and definitions, with shared slot mapping,
      native metadata checks and Vim alias/fresh-value review (ADR-138).
- [x] Multi-cursor editing
- [x] Code folding
- [x] Configurable formatting rules

Ctrl+Alt+Up/Down adds Vim cursors on adjacent lines (maximum 128). Insert edits
and deletions apply across the group; Escape closes secondary cursors.
Completion is suppressed while multiple cursors are active.

### SQL intelligence

- [x] Keyword, table, and column completion
- [x] Context-sensitive completion
- [x] Alias-aware completion
- [x] CTE and temporary-object completion
- [x] Automatic Vim-insert completion with manual Ctrl+Space fallback
- [x] Live query-tab connection/database binding for SQL intelligence
- [x] Foreign-key JOIN completion
- [x] Syntax diagnostics
- [x] Semantic diagnostics
- [x] Idle-debounced diagnostics with stale markers hidden while typing
- [x] Quick fixes
- [x] Go to definition
- [x] Find usages
- [x] Rename refactoring
- [x] Statement selection
- [x] Catalog-aware binding
- [x] Hover types and object metadata
- [x] Multi-hop JOIN suggestions
- [x] Star expansion
- [x] Unsafe UPDATE/DELETE inspection UI
- [x] Cartesian JOIN inspection UI

Safety inspections appear through revision-bound editor diagnostics. Mutation
breadth and Cartesian joins are independent findings, including joins nested
in FROM relations. These warnings do not rewrite or block SQL.

JOIN table slots offer catalog-proven FK snippets, including composite keys,
reverse traversal and paths of up to three hops. Source aliases are preserved;
generated aliases avoid collisions. Nested/parenthesized statements and
CROSS/NATURAL JOIN slots retain ordinary completion. Missing or slow graph
metadata falls back to ordinary candidates while the bounded fetch warms cache.
Outer joins offer direct paths only; self-referencing foreign keys are supported.

### Execution and safety

- [x] Result grid
- [x] Execution timing and outcome
- [x] Multiple result sets
- [x] Explicit transactions
- [x] Commit and rollback
- [x] Savepoints
- [x] Query timeout
- [x] Explain plans
- [x] Explain Analyze safety
- [x] Available-operation gating
- [x] Audited operations
- [x] Production confirmation policy UI
- [x] Affected-row preview
- [x] Plan-node cost display
- [x] Plan comparison
- [x] Query progress UI

### Results and data editing

- [x] Virtualized result grid
- [x] NULL and binary value rendering
- [x] Server-side paging
- [x] Result sorting and filtering
- [x] Result export
- [x] Inline row insert, update, and delete
- [x] Staged edit preview
- [x] Conflict detection
- [x] Parameterized DML generation
- [x] Table and query-result comparison
- [x] Result search UI
- [x] Copy as CSV, JSON, SQL, or Markdown
- [x] JSON and text large viewers
- [x] Image and blob viewers
- [x] Foreign-key picker
- [x] Aggregate selected cells
- [x] Saved grid layouts

Value inspector previews PNG/JPEG/GIF/WebP first frames with bounded background
decoding. Binary values have paged hex/ASCII inspection (`j/k`, 4 KiB per page)
and page copying. Image limits: 16 MiB input, 4096×4096 pixels, 64 MiB decoder
allocation; unsupported or oversized images retain hex access.

### Schema and migration

- [x] Object DDL generation
- [x] Catalog graph
- [x] Schema snapshots
- [x] Schema diff
- [x] Migration preview and apply
- [x] Risk classification
- [x] Dependency ordering
- [x] Diagram projection
- [x] Diagram mutation preview
- [x] General object designer UI
- [x] Rollback script generation
- [x] Live database versus migration-folder diff
- [x] Drift notifications
- [x] Diagram export

Objects → Design (`d`) combines structured table design with canonical editable
replacement SQL for PostgreSQL/SQL Server views, functions, procedures and
triggers, plus sequence/type templates. Drafts retain source database binding;
loading/unsupported definitions remain read-only. SQLite views and materialized
view replacement require separate drop/recreate workflows and remain excluded.

### Collaboration and assistance

- [x] Shared rooms
- [x] Collaborative SQL documents
- [x] Presence and selections
- [x] Follow mode
- [x] Shared room connections
- [x] Shared result references
- [x] Workspace history and checkpoints
- [x] Personal and team server vaults
- [~] Extension system
- [~] Governed MCP tools
- [x] Private benchmark measurement inspection/comparison in AI and standalone MCP
      (bounded tenant/owner snapshots, no profile attestation or agent execution; ADR-108)
- [x] SDK-backed CLI tool list/call, managed query, confirmed Benchmark/Profile and
      private saved-run list/get/compare workflows (ADR-108)
- [x] Declarative extension contribution renderer — instance-scoped commands,
      scalar forms, bounded read-only result tables/details, and source-bound
      read panels in the trusted desktop dialog.
- [x] Context-target extension action binding and rendering — derive authorized
      tenant, room, profile, connection, and document scopes on the server.
- [x] Shared-query browser UI
- [x] Reviewable AI SQL generation (private Codex chat stages complete SQL drafts;
      public rooms use explicit shared database grants; typed row and migration
      drafts use human review, production confirmation and durable apply receipts;
      Linux Codex/Claude Code/OpenCode adapters and per-chat provider/model
      choice and live isolated tool roundtrips are verified for all three;
      governed own/same-room history, owned saved plans, and revision-bound native
      DDL reads and exact source-resolved attachment previews are implemented;
      desktop exact preview/chip controls, selected row/history/plan actions and
      shared-room cell selection are implemented with frozen result provenance;
      bounded diagnostics, inclusion choices and source-matched connection/transaction
      hints and bounded history, cancellation, quotas and run deadlines are
      implemented; reviewed encrypted external source registrations, independent
      room grants, audited HTTP/SDK management and selected remote read receipts
      are implemented; explicit source selection, operator review UI, credential
      refresh and quota-bound fixed provider inventory/read tools are implemented;
      reviewed local SQL/row/schema intent adapters retain encrypted provenance and
      use existing human review paths without remote writes; exact lost-startup
      cancellation preserves prompts and prevents late orphan turns; workspace,
      strict Clippy and live isolated Codex/Claude/OpenCode checks pass. Linux
      Read/Propose development and real PostgreSQL/SQL Server/SQLite backend acceptance
      are complete, including typed row/simple-table migration apply, stale proposals,
      revocation, cancellation, replay and desktop terminal supervision;
      Execute/unattended runs and Windows/macOS isolation are outside this scope)
- [x] Resizable independent AI panel with provider model/reasoning pickers and
      native Markdown transcripts, including tables, inline formatting and code
      copy controls using the SQL editor's syntax colors (ADR-109). Wrapped multiline
      prompts, compact retry feedback, narrow footer labels, full before/after SQL
      review, and audited revision-checked thread rename/delete controls are implemented.
- [x] AI chat lifecycle backend (tenant/admin retention, resumable content-key
      rotation, and encrypted full/tenant recovery with authenticated blob inventory)
- [x] AI error and plan explanation (all three adapters offer governed syntax
      diagnostics, estimated explain, scoped history/error reads, and bounded
      owned saved/analyzed-plan reads without execution; exact plan attachment
      previews/publication and desktop selection/review actions are implemented;
      bounded revision-bound automatic diagnostics and last-sent inclusion
      preferences are implemented; combined workspace UX/adapter tests pass,
      including verified live isolated tool roundtrips for all three providers)

“Find Shared Query…” (`<leader> f c`, or `&` in the command palette) searches
accessible room documents by title, tenant, and room. Results show room context
and open the live collaborative document, reusing existing tabs.

## DBMS Workbench

### Sessions, locks, and monitoring

- [x] Process and session listing
- [x] Cancel query
- [x] Terminate session
- [x] Transaction listing
- [x] Query duration and state
- [x] PostgreSQL activity metadata
- [x] SQL Server request metadata
- [x] Lock manager UI (waiting resource, mode, and wait age plus a bounded
      held-lock inventory in the process monitor)
- [x] Blocking-chain visualization
- [x] Deadlock inspection (live Cycles view, bounded SQL Server
      `system_health` history and operator-scoped PostgreSQL JSON log history)
- [x] Long-running-query alerts (bounded API subscriptions)
- [x] Idle-in-transaction alerts (bounded API subscriptions)
- [x] Server dashboard (audited per-connection Overview with bounded process
      counts, sampled-at and truncation state, capability/permission states,
      and a Vim command path through `<leader> d s`)
- [x] Query-performance history (scoped hourly API summaries)
- [~] Query performance workbench: profiling, repeatable benchmarks, saved runs
  and comparisons — [implementation checklist](query-performance.md)
- [x] Controlled paired benchmarks: frozen A/B workloads, alternating or seeded
      randomized round order, separate latency populations, shared cancellation/
      budget and complete attempt ordering. Vim review/population navigation,
      confirmed CLI, API/SDK and private artifact lineage (ADR-144); equivalence,
      controlled cache/load and instrumented A/B populations remain open.
- [x] Live Benchmark/Profile stages and attempt counters in Vim, with bounded
      polling/backpressure, current-policy reads, background source tracking and
      stale/reconnect refusal (ADR-131); final sample lists remain virtualized.
- [x] Private saved benchmark notes/tags: encrypted revision-checked edits,
      immutable measurements and Vim drafts preserved across conflicts
      (ADR-126). Optional workspace/Git source observations now freeze at
      benchmark review and save only on opt-in, outside measurement artifacts
      and AI/MCP projections (ADR-134). Owner-private age/count retention now has
      reviewed activation, automatic enforcement and durable payload cleanup,
      disabled by default (ADR-135).
- [x] Optional private Profile attachments on saved benchmarks: frozen workload
      matching, separate opt-in, encrypted bounded payloads and execution-free
      Vim Plan reopening (ADR-136). Profile clocks remain separate from benchmark
      samples; raw plans stay out of portable artifacts and AI/MCP measurement views.
- [x] Private saved Profile plan comparison in Vim and explicit CLI output:
      bounded normalized tree changes, heuristic/path correspondence, visible
      ambiguity/alignment limits and separate clocks, without execution (ADR-137).
- [x] Benchmark/Profile selected session-setting observations on dedicated
      connections, with bounded supervised capture, limited-scope comparison
      warnings and display (ADR-128); complete environment equivalence remains open.

Historical deadlock design: read at most 50 recent `xml_deadlock_report` events
from SQL Server's `system_health` ring buffer, with an explicit permission error
when the connection cannot read it. Project only timestamp, victim session,
participating sessions, and bounded wait-resource details; do not return raw
graph XML or statement text. This is retained ring-buffer history, not a durable
archive. PostgreSQL exposes current waits and a deadlock count through catalog
views. Event history now supports operator-registered JSON log files pinned to
managed tenant/profile, host/port and database, with upstream statistics rights
and schema-policy checks. Reads examine the last 4 MiB and project at most 50
redacted summaries; unsupported wait descriptions mark participant coverage
incomplete. Operators own logging, transport and rotation. The history read is
a distinct audited operation; neither provider claims a durable archive.

### Security and administration

- [~] Principals and authentication (hosted GitHub OAuth and closed admission;
      local OS-proven GitHub device owner linking and verification — ADR-110)
- [~] Tenants and memberships (desktop role/target-bound invitations, one-use
      acceptance, immediate permission refresh, and tenant-fenced revocation — ADR-110)
- [~] Role-based authorization
- [~] Connection policies
- [~] Resource and rate limits
- [~] Audit log
- [~] API tokens and signing keys
- [~] Approval workflows
- [~] Database users and roles editor (bounded role catalog with Vim desktop
      inspection and source-bound reviewed `CREATE ROLE NOLOGIN` desktop form;
      PostgreSQL 16+ explicit membership grant/update/revoke now binds grantor,
      all ADMIN/INHERIT/SET options and native identities in Vim review (ADR-140).
      PostgreSQL 16+ public create/rename/attribute-patch/drop actions now use
      native authority and identity/dependency-bound Vim review (ADR-143).
      Reviewed public workload defaults now support global/connected-database
      inspection and typed set/reset with native login semantics (ADR-145).
      Credentials, arbitrary/secret-bearing GUCs and effective privileges remain)
- [~] Grants and privilege matrix (bounded explicit schema ACL catalog with
      Vim desktop inspection and reviewed schema `USAGE`/`CREATE` grant/revoke
      forms; object and inherited privileges plus desktop matrix remain)
- [~] Database and schema ownership editor (bounded owner catalog with Vim
      desktop inspection and reviewed PostgreSQL database/schema owner-change
      forms plus SQL Server current-database/schema transfer forms, native
      permission-loss review and stale catalog identity checks (ADR-121);
      external/implicit server-owner families remain)
- [x] PostgreSQL row-level security editor: bounded catalog, rename, typed
      create/alter/drop definitions and roles, command/mode selection, table
      enable/disable/force controls, source-bound SQL review and native row/write
      enforcement acceptance (ADR-120). Unsupported expression syntax is refused;
      command/mode replacement uses separately reviewed drop/create actions.
- [~] SQL Server login and permission editor (bounded login, database principal,
      role membership, schema owner, and explicit schema permission inspection
      in a Vim Monitor view; reviewed role creation, membership add/remove and
      schema SELECT grant/revoke forms bind exact source and request identity;
      database/schema ownership transfers disclose native permission loss and
      use fresh identity-bound review (ADR-121); role/membership/schema-SELECT
      reviews bind native SIDs and actor/database identity, and refuse ambiguous
      grantors (ADR-122); SQL database-user create/remap/orphan repair,
      default-schema and drop forms now bind native identity and authority
      (ADR-132). Login credentials, other identity-provider/contained-user families,
      external-owner families and effective permissions remain)

### Import, export, and transfer

- [x] CSV import
- [x] CSV, TSV, JSON, and JSONL export
- [x] Background transfer execution
- [x] Cross-connection transfer recipes
- [~] Column mapping (CSV v2 ordered selection, drop, reorder, copy and rename
      through Vim review controls or recipe JSON; projected samples, DDL, native
      rows and saved recipes share the selection. XLSX/Parquet import v2 now share
      selection through Vim recipe JSON, with typed Parquet root projection and
      bounded XLSX conversion. Expressions, direct non-CSV review grids and wider
      sources remain pending; ADR-130/133/139)
- [x] Bounded streaming
- [x] Transfer scheduling
- [x] Import schema and type inference UI
- [x] Dry-run transfer UI
- [x] Error quarantine (CSV API reports, authorized artifact downloads, and
      desktop inspection from the current result or retained workspace history)
- [x] Durable CSV resume API (target-side atomic checkpoints and source validation)
- [x] Resumable CSV import UI (target-side checkpoint table and stable run UUID)
- [x] Cross-engine CSV type-mapping editor (Postgres, SQL Server, SQLite)
- [x] Parquet support

Transfer preview, CSV quarantine reports with retained desktop history,
and durable CSV resume are implemented. The desktop saves the checkpoint table
and run UUID in the CSV recipe; retrying with that recipe and the same source file
resumes from the target-side checkpoint. CSV preview accepts per-column SQL
types for the selected engine and can carry them into a transfer recipe.
Quarantine report fields and provider limits are documented in backend operations.

Parquet has bounded typed API export and transactional flat-type import; see
[backend operations](../BACKEND-OPERATIONS.md) for supported types and limits.

### Backup, restore, and maintenance

- [x] Sift state backup and restore
- [x] Metadata migration lifecycle
- [x] Sift backup scheduling and retention
- [x] Sift backup remote/object-store destinations
- [~] Tenant-selective Sift restore and disaster-recovery orchestration — offline same-installation/same-ID tenant recovery delivered; cross-instance identity migration and external DR orchestration remain.
- [x] Scheduled runs
- [x] Durable task history
- [x] Task cancellation and recovery
- [x] PostgreSQL dump and restore
- [x] SQL Server backup and restore (copy-only backup/new-name restore API)
- [x] SQLite same-root connected-main restore: reviewed atomic native copy,
      explicit overwrite/backup acknowledgment and Vim controls (ADR-125).
- [x] Restore preview and target validation (PostgreSQL/SQL Server backend workflows)
- [x] VACUUM and ANALYZE actions (explicit-target PostgreSQL API)
- [x] REINDEX actions (explicit-target PostgreSQL API)
- [~] Table and index maintenance (PostgreSQL API and Vim Monitor preview/apply
      for explicit table/index targets; SQLite reviewed main-table ANALYZE and
      main-index REINDEX now have native transactional writes, bounded review/
      cancellation and Vim controls (ADR-146); SQL Server single-index offline
      REBUILD, REORGANIZE and FULLSCAN statistics now have reviewed API/SDK/Vim
      controls with native acceptance (ADR-147). Advanced partition/storage and
      online/resumable families remain ([scope](sqlserver-index-maintenance.md)))
- [x] Integrity checks (scoped native API checks; PostgreSQL heap check in Vim
      Monitor; no repair)

The checked Sift recovery features are implemented operator CLI workflows
(ADRs 038/039/058), not a complete recovery UI. Backup policies are offline,
timer-driven and support encrypted conditional HTTPS PUT destinations. PostgreSQL
has a separate bounded custom-archive CLI with explicit transactional apply;
cluster/PITR recovery and destination compatibility preview remain separate.

### Engine-specific depth

The backend daily-driver scope below passed ADR-055 acceptance. Partial markers
retain broader UI/administration depth; they no longer imply missing native
provider implementations. The evidence matrix names tested versions and explicit
exclusions. Broader engine administration remains separate work.
Engine-specific follow-up checklists: [PostgreSQL](postgres-provider-gaps.md),
[SQL Server](sqlserver-provider-gaps.md), and [SQLite](sqlite-provider-gaps.md).

- [x] PostgreSQL scoped shallow/deep/graph introspection and native object DDL
- [~] PostgreSQL plans
- [~] PostgreSQL process control
- [~] PostgreSQL bulk import
- [x] PostgreSQL notifications (single-channel desktop listener with bounded
      history, explicit stop/restart, and Vim navigation)
- [x] SQL Server scoped shallow/deep/graph introspection and native object DDL
- [~] SQL Server plans
- [x] SQL Server process control (bounded audited session list and termination;
      Vim Monitor selection and explicit confirmation, capability reasons,
      connection and response generation guards)
- [x] SQL Server bulk import (Vim CSV preview, target-column mapping, dry-run
      gate, durable recipe retry, and reviewed identity Generate/Preserve and
      computed Omit for CSV/XLSX/typed Parquet. Reviewed modes add owned atomic
      writes, active progress, cancellation and unknown COMMIT receipts; ordinary
      targets and supported scalar conversions only, ADR-142)
- [~] PostgreSQL extensions and partition management UI (bounded catalog views,
      Vim preview/apply for extension install/drop, version-pinned extension
      update, partition detach and typed attachment with bounded dependency impact;
      broader cross-object management remains)
- [x] PostgreSQL replication and statistics inspection UI (bounded, audited
      read-only snapshots of senders, receiver, slots, database and accessible
      table counters; explicit replication privilege errors, Vim Monitor views)
- [x] PostgreSQL settings browser (read-only, bounded paging, role-visible
      `pg_settings`, sensitive-value redaction, audited API and Vim desktop view)
- [x] SQL Server Query Store (read-only, bounded database-state and plan inspection)
- [x] SQL Server Agent jobs browser (read-only, bounded owned-job reads for
      non-sysadmins, bounded local whole-job history through native Agent-role
      authority, audited API and Vim desktop view; native User/Reader/Operator
      role, revocation, truncation and editor-state acceptance pass (ADR-129))
- [x] SQL Server server-settings browser (read-only, bounded `sys.configurations`
      snapshot, configured/effective values, permission-aware audited API and
      Vim desktop view; native configured/effective comparison and explicit
      permission-denial/recovery acceptance pass (ADR-129))

Both SQL Server inspection views preserve editor state through private connections.
Typed session variables and supported literal SET startup are retained; arbitrary
startup batches are refused before a new inspection connection opens (ADR-129).
- [x] SQLite provider design ([scope and acceptance](sqlite-provider.md))
- [x] SQLite provider implementation (protocol 2; scoped Linux file support)
- [x] SQLite scoped provider graduation (ADR-056; explicit platform/DBA exclusions)
- [x] SQLite managed savepoints, estimated plans and atomic CSV import
- [x] SQLite seeded desktop demos and read-only Sift metadata inspection command
- [x] SQLite manifest completion/validation, provider artwork, and desktop metadata inspection menu
- [x] SQLite native migration backend for proven table/column rename mappings,
      transactional inverse and real-file preservation/refusal acceptance
      (ADRs 118/123); broader designer and dependency-bearing shapes remain partial.
- [x] SQLite existing-column designer rename and typed table-rename previews
      derived from live proof, with source-bound review/apply (ADR-124).
- [x] Editor diagnostics in Problems, usable copy controls, and a diagnostic-only footer

SQLite is implemented with explicit server roots, bounded workers, dynamic
values, native DDL, keyed edits and a partial IDE navigation catalog. Scoped
snapshots/diff, simple-table create and table/column rename migrations, desktop
native CSV bulk targets, and create/backup/restore/integrity/VACUUM workflows are
implemented, including existing-column rename controls. Full dependency
graphs/general migrations and broader designer controls,
per-node actual plans, larger streaming native transfers, cross-root recovery/
delete and Windows file acceptance remain outstanding. See [SQLite support](../SQLITE.md)
and the [current gap checklist](sqlite-provider-gaps.md).

### Platform and operations

- [x] Prometheus metrics endpoint
- [x] OpenTelemetry trace export
- [~] Cross-platform desktop packaging
- [ ] Signed artifact and installer validation matrix

Observability currently covers HTTP request metrics and opt-in bounded
OTLP/HTTP request-span export, not SQL/result payloads or full driver traces.
