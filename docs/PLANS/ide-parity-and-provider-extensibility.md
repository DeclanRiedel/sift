# SQL IDE and DBMS Feature Coverage

Product-level coverage checklist for PostgreSQL and SQL Server.

Legend: `[x]` usable in desktop · `[~]` server/API exists, desktop incomplete · `[ ]` not implemented

## SQL IDE

### Connections and workspace

- [x] Connection profiles
- [x] Multiple saved connections
- [x] Connect and disconnect
- [~] Ad-hoc connection editor
- [~] Connection testing and health
- [~] SSL/TLS configuration
- [~] SSH remote connections
- [~] Read-only connections
- [~] Secret-store integration
- [~] Provider capability discovery
- [ ] Connection folders, tags, and favorites
- [ ] Environment labels and colors
- [ ] Production write lock
- [ ] Startup SQL and session variables
- [x] Persistent editor workspace
- [~] Multiple windows
- [~] Virtual workspaces
- [~] Filesystem projection
- [~] Git integration

### Explorer and navigation

- [~] Database and schema tree
- [~] Tables, views, and columns
- [~] Indexes and constraints
- [~] Functions, procedures, and types
- [~] Lazy metadata loading
- [~] Metadata refresh and invalidation
- [~] Schema search
- [~] Data search
- [~] Object DDL view
- [~] Dependency and dependent graph
- [~] Foreign-key navigation
- [ ] Global fuzzy object search UI
- [ ] Recent and favorite objects
- [ ] Object filters and saved explorer views
- [ ] Breadcrumb and peek navigation

### SQL editor

- [x] SQL editor tabs
- [x] Syntax highlighting
- [x] Execute document, statement, and selection
- [~] Query cancellation
- [~] Streaming and paged results
- [~] Parameterized execution
- [~] SQL formatting
- [~] Query history
- [~] Saved queries
- [x] Find and replace UI
- [ ] Line numbers and editor gutter
- [ ] Split editor
- [ ] Scratch SQL files
- [ ] Snippets and templates
- [ ] SQL variables
- [ ] Multi-cursor editing
- [ ] Code folding
- [ ] Configurable formatting rules

### SQL intelligence

- [~] Keyword, table, and column completion
- [~] Context-sensitive completion
- [~] Alias-aware completion
- [~] CTE and temporary-object completion
- [~] Foreign-key JOIN completion
- [~] Syntax diagnostics
- [~] Semantic diagnostics
- [~] Quick fixes
- [~] Go to definition
- [~] Find usages
- [~] Rename refactoring
- [~] Statement selection
- [~] Catalog-aware binding
- [ ] Hover types and object metadata
- [ ] Multi-hop JOIN suggestions
- [ ] Star expansion
- [ ] Unsafe UPDATE/DELETE inspection UI
- [ ] Cartesian JOIN inspection UI

### Execution and safety

- [x] Result grid
- [x] Execution timing and outcome
- [~] Multiple result sets
- [~] Explicit transactions
- [~] Commit and rollback
- [~] Savepoints
- [~] Query timeout
- [~] Explain plans
- [~] Explain Analyze safety
- [~] Available-operation gating
- [~] Audited operations
- [ ] Production confirmation policy UI
- [ ] Affected-row preview
- [ ] Plan-node cost highlighting
- [ ] Plan comparison
- [ ] Query progress UI

### Results and data editing

- [x] Virtualized result grid
- [x] NULL and binary value rendering
- [~] Server-side paging
- [~] Result sorting and filtering
- [~] Result export
- [~] Inline row insert, update, and delete
- [~] Staged edit preview
- [~] Conflict detection
- [~] Parameterized DML generation
- [~] Table and query-result comparison
- [ ] Result search UI
- [ ] Copy as CSV, JSON, SQL, or Markdown
- [ ] JSON, text, image, and blob viewers
- [ ] Foreign-key picker
- [ ] Aggregate selected cells
- [ ] Saved grid layouts

### Schema and migration

- [~] Object DDL generation
- [~] Catalog graph
- [~] Schema snapshots
- [~] Schema diff
- [~] Migration preview and apply
- [~] Risk classification
- [~] Dependency ordering
- [~] Diagram projection
- [~] Diagram mutation preview
- [ ] General object designer UI
- [ ] Rollback script generation
- [ ] Live database versus migration-folder diff
- [ ] Drift notifications
- [ ] Diagram export

### Collaboration and assistance

- [~] Shared rooms
- [~] Collaborative SQL documents
- [~] Presence and selections
- [~] Follow mode
- [~] Shared room connections
- [~] Shared result references
- [~] Workspace history and checkpoints
- [~] Extension system
- [~] Governed MCP tools
- [ ] Shared-query browser UI
- [ ] Reviewable AI SQL generation
- [ ] AI error and plan explanation

## DBMS Workbench

### Sessions, locks, and monitoring

- [~] Process and session listing
- [~] Cancel query
- [~] Terminate session
- [~] Transaction listing
- [~] Query duration and state
- [~] PostgreSQL activity metadata
- [~] SQL Server request metadata
- [ ] Lock manager UI
- [ ] Blocking-chain visualization
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

- [~] CSV import
- [~] CSV, TSV, JSON, and JSONL export
- [~] Background transfer execution
- [~] Cross-connection transfer recipes
- [~] Column mapping
- [~] Bounded streaming
- [~] Transfer scheduling
- [ ] Import schema and type inference UI
- [ ] Dry-run transfer UI
- [ ] Error quarantine
- [ ] Resumable transfer UI
- [ ] Cross-engine type-mapping editor
- [ ] Parquet support

### Backup, restore, and maintenance

- [~] Sift state backup and restore
- [~] Metadata migration lifecycle
- [~] Scheduled runs
- [~] Durable task history
- [~] Task cancellation and recovery
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
