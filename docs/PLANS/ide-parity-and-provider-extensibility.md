# SQL IDE and DBMS Feature Coverage

Status: **active-development feature inventory, audited 2026-09-01.** This is
the canonical checkbox list for choosing product features. It is not a release
roadmap and has no release date, version scope, beta boundary, or claim of
readiness. A checked feature may still need hardening, accessibility,
performance, documentation, and cross-platform work.

Legend: `[x]` usable feature slice exists · `[~]` partial or server-only · `[ ]`
missing. Choose new feature work from `[~]` and `[ ]`; use Phase M for desktop
architecture and validation work.

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
- [ ] Snippets and templates
- [ ] SQL variables
- [ ] Multi-cursor editing
- [ ] Code folding
- [ ] Configurable formatting rules

### SQL intelligence

- [x] Keyword, table, and column completion
- [x] Context-sensitive completion
- [x] Alias-aware completion
- [~] CTE and temporary-object completion
- [~] Foreign-key JOIN completion
- [x] Syntax diagnostics
- [x] Semantic diagnostics
- [x] Quick fixes
- [x] Go to definition
- [x] Find usages
- [x] Rename refactoring
- [x] Statement selection
- [x] Catalog-aware binding
- [ ] Hover types and object metadata
- [ ] Multi-hop JOIN suggestions
- [ ] Star expansion
- [ ] Unsafe UPDATE/DELETE inspection UI
- [ ] Cartesian JOIN inspection UI

### Execution and safety

- [x] Result grid
- [x] Execution timing and outcome
- [ ] Multiple result sets
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
- [ ] Query progress UI

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
- [ ] Copy as CSV, JSON, SQL, or Markdown
- [x] JSON and text large viewers
- [ ] Image and blob viewers
- [ ] Foreign-key picker
- [x] Aggregate selected cells
- [ ] Saved grid layouts

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
- [ ] Dry-run transfer UI
- [ ] Error quarantine
- [ ] Resumable transfer UI
- [ ] Cross-engine type-mapping editor
- [ ] Parquet support

### Backup, restore, and maintenance

- [~] Sift state backup and restore
- [~] Metadata migration lifecycle
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

### Engine-specific depth

- [~] PostgreSQL schema introspection
- [~] PostgreSQL plans
- [~] PostgreSQL process control
- [~] PostgreSQL bulk import
- [~] PostgreSQL notifications
- [~] SQL Server schema introspection
- [~] SQL Server plans
- [~] SQL Server process control
- [~] SQL Server bulk import
- [ ] PostgreSQL extensions and partition management UI
- [ ] PostgreSQL replication and statistics UI
- [ ] PostgreSQL settings browser
- [ ] SQL Server Query Store
- [ ] SQL Server Agent
- [ ] SQL Server server-settings browser

### Platform and operations

- [ ] Prometheus metrics endpoint
- [ ] OpenTelemetry trace export
- [~] Cross-platform desktop packaging
- [ ] Signed artifact and installer validation matrix
