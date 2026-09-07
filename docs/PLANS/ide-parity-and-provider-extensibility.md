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
- [ ] Multi-cursor editing
- [x] Code folding
- [x] Configurable formatting rules

### SQL intelligence

- [x] Keyword, table, and column completion
- [x] Context-sensitive completion
- [x] Alias-aware completion
- [x] CTE and temporary-object completion
- [x] Automatic Vim-insert completion with manual Ctrl+Space fallback
- [x] Live query-tab connection/database binding for SQL intelligence
- [~] Foreign-key JOIN completion
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
- [ ] Multi-hop JOIN suggestions
- [x] Star expansion
- [x] Unsafe UPDATE/DELETE inspection UI
- [x] Cartesian JOIN inspection UI

Safety inspections appear through revision-bound editor diagnostics. Mutation
breadth and Cartesian joins are independent findings, including joins nested
in FROM relations. These warnings do not rewrite or block SQL.

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
- [ ] Image and blob viewers
- [x] Foreign-key picker
- [x] Aggregate selected cells
- [x] Saved grid layouts

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
- [~] General object designer UI
- [x] Rollback script generation
- [x] Live database versus migration-folder diff
- [x] Drift notifications
- [x] Diagram export

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
- [ ] Declarative extension contribution renderer
- [ ] Shared-query browser UI
- [ ] Reviewable AI SQL generation
- [ ] AI error and plan explanation

## DBMS Workbench

### Sessions, locks, and monitoring

- [x] Process and session listing
- [x] Cancel query
- [x] Terminate session
- [x] Transaction listing
- [x] Query duration and state
- [x] PostgreSQL activity metadata
- [x] SQL Server request metadata
- [ ] Lock manager UI
- [x] Blocking-chain visualization
- [ ] Deadlock inspection
- [ ] Long-running-query alerts
- [ ] Idle-in-transaction alerts
- [ ] Server dashboard
- [ ] Query-performance history

### Security and administration

- [~] Principals and authentication
- [~] Tenants and memberships
- [~] Role-based authorization
- [~] Connection policies
- [~] Resource and rate limits
- [~] Audit log
- [~] API tokens and signing keys
- [~] Approval workflows
- [ ] Database users and roles editor
- [ ] Grants and privilege matrix
- [ ] Database and schema ownership editor
- [ ] PostgreSQL row-level security editor
- [ ] SQL Server login and permission editor

### Import, export, and transfer

- [x] CSV import
- [x] CSV, TSV, JSON, and JSONL export
- [x] Background transfer execution
- [x] Cross-connection transfer recipes
- [~] Column mapping
- [x] Bounded streaming
- [x] Transfer scheduling
- [x] Import schema and type inference UI
- [x] Dry-run transfer UI
- [ ] Error quarantine
- [ ] Resumable transfer UI
- [ ] Cross-engine type-mapping editor
- [ ] Parquet support

Transfer preview is implemented. Quarantine report retrieval, durable resume
with checkpoints/source validation, and a dedicated type-mapping editor remain
open; existing preview and result plumbing do not complete those workflows.

### Backup, restore, and maintenance

- [x] Sift state backup and restore
- [x] Metadata migration lifecycle
- [ ] Sift backup scheduling and retention
- [ ] Sift backup remote/object-store destinations
- [ ] Tenant-selective Sift restore and disaster-recovery orchestration
- [x] Scheduled runs
- [x] Durable task history
- [x] Task cancellation and recovery
- [ ] PostgreSQL dump and restore
- [ ] SQL Server backup and restore
- [ ] Restore preview and target validation
- [ ] VACUUM and ANALYZE actions
- [ ] REINDEX actions
- [ ] Table and index maintenance
- [ ] Integrity checks

The checked Sift recovery features are implemented operator CLI workflows
(ADRs 038/039), not connected-database backups or a complete recovery UI.

### Engine-specific depth

The backend daily-driver scope below passed ADR-055 acceptance. Partial markers
retain broader UI/administration depth; they no longer imply missing native
provider implementations. The evidence matrix names tested versions and explicit
exclusions. Broader engine administration remains separate work.

- [x] PostgreSQL scoped shallow/deep/graph introspection and native object DDL
- [~] PostgreSQL plans
- [~] PostgreSQL process control
- [~] PostgreSQL bulk import
- [~] PostgreSQL notifications
- [x] SQL Server scoped shallow/deep/graph introspection and native object DDL
- [~] SQL Server plans
- [~] SQL Server process control
- [~] SQL Server bulk import
- [ ] PostgreSQL extensions and partition management UI
- [ ] PostgreSQL replication and statistics UI
- [ ] PostgreSQL settings browser
- [ ] SQL Server Query Store
- [ ] SQL Server Agent
- [ ] SQL Server server-settings browser
- [x] SQLite provider design ([scope and acceptance](sqlite-provider.md))
- [x] SQLite provider implementation (protocol 2; scoped Linux file support)
- [x] SQLite scoped provider graduation (ADR-056; explicit platform/DBA exclusions)
- [x] SQLite managed savepoints, estimated plans and atomic CSV import
- [x] SQLite seeded desktop demos and read-only Sift metadata inspection command
- [x] SQLite manifest completion/validation, provider artwork, and desktop metadata inspection menu
- [x] Editor diagnostics in Problems, usable copy controls, and a diagnostic-only footer

SQLite is implemented with explicit server roots, bounded workers, dynamic
values, native DDL, keyed edits and a partial IDE navigation catalog. Full
dependency graphs/migrations, actual plans, native bulk targets, maintenance and
Windows file access remain outside its scope. See [SQLite support](../SQLITE.md)
and the [overnight handoff](database-provider-overnight.md).

### Platform and operations

- [ ] Prometheus metrics endpoint
- [ ] OpenTelemetry trace export
- [~] Cross-platform desktop packaging
- [ ] Signed artifact and installer validation matrix
