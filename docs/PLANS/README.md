# Development plan index

Reconciled 2026-11-11. Start with [the current TODO](../TODO.md), then read the
linked contract and relevant [ADRs](../DECISIONS.md). “Done” is limited to the
implemented scope; it never upgrades a partial parent feature or deferred native
platform. Reference/evidence documents describe current boundaries rather than
opening a second backlog. Completed contracts remain because their safety and
wire/lifecycle rules still matter.

| Plan / contract | Current status | Remaining work |
| --- | --- | --- |
| [AI agent integration contract](ai-agent-integration.md) | Done (scoped contract) | [TODO](../TODO.md#ai-execution) |
| [Governed AI execution and unattended runs](ai-execution.md) | Partial / active | [TODO](../TODO.md#ai-execution) |
| [Backend data operations](backend-data-operations.md) | Done (scoped contract) | [TODO](../TODO.md#recovery-and-maintenance) |
| [Backend workbench batch](backend-workbench-batch.md) | Done (scoped contract) | [TODO](../TODO.md#recovery-and-maintenance) |
| [Independent benchmark suite case limits](benchmark-case-limits.md) | Done (scoped contract) | [TODO](../TODO.md#performance-workbench) |
| [Concurrent benchmark load](benchmark-load.md) | Done (scoped contract) | [TODO](../TODO.md#performance-workbench) |
| [Conditional median uncertainty for benchmark comparisons](benchmark-median-uncertainty.md) | Done (scoped contract) | [TODO](../TODO.md#performance-workbench) |
| [Private Profile attachments](benchmark-profile-attachments.md) | Done (scoped contract) | [TODO](../TODO.md#performance-workbench) |
| [Private benchmark retention](benchmark-retention.md) | Done (scoped contract) | [TODO](../TODO.md#performance-workbench) |
| [Private benchmark-run annotations](benchmark-run-annotations.md) | Done (scoped contract) | [TODO](../TODO.md#performance-workbench) |
| [Private benchmark source context](benchmark-source-context.md) | Done (scoped contract) | [TODO](../TODO.md#performance-workbench) |
| [Desktop benchmark suites](benchmark-suite-desktop.md) | Done (scoped contract) | [TODO](../TODO.md#performance-workbench) |
| [Explicit benchmark suite ordering](benchmark-suite-ordering.md) | Done (scoped contract) | [TODO](../TODO.md#performance-workbench) |
| [Serial parameter matrices and query suites](benchmark-suites.md) | Done (scoped contract) | [TODO](../TODO.md#performance-workbench) |
| [Bulk insert captured source and owned settlement](bulk-source-settlement.md) | Done (scoped contract) | [TODO](../TODO.md#ai-execution) |
| [Catalog Graph, Schema Diff, and Migration Safety](catalog-graph-schema-migrations.md) | Done (scoped contract) | [TODO](../TODO.md) |
| [Ordered CSV recipe column projection](csv-column-projection.md) | Done (scoped contract) | [TODO](../TODO.md#imports-and-transfers) |
| [Ordered CSV import review controls](csv-projection-editor.md) | Done (scoped contract) | [TODO](../TODO.md#imports-and-transfers) |
| [CSV captured source and owned settlement](csv-source-settlement.md) | Done (scoped contract) | [TODO](../TODO.md#imports-and-transfers) |
| [Database provider acceptance evidence](database-provider-acceptance.md) | Reference / scoped evidence | [TODO](../TODO.md#desktop-quality-and-architecture) |
| [Database provider overnight handoff](database-provider-overnight.md) | Done (scoped contract) | [TODO](../TODO.md) |
| [DDL — supported scope and remaining gaps](ddl-gaps.md) | Partial / active | [TODO](../TODO.md) |
| [Desktop API and workflow coverage](desktop-api-coverage.md) | Reference / scoped evidence | [TODO](../TODO.md#desktop-quality-and-architecture) |
| [Desktop bundle verification and acceptance gates](desktop-artifact-verification.md) | Done (scoped contract) | [TODO](../TODO.md#platform-distribution-and-native-acceptance) |
| [Desktop transaction control source binding](desktop-transaction-control-binding.md) | Done (scoped contract) | [TODO](../TODO.md#ai-execution) |
| [Export query terminal ownership](export-query-settlement-ownership.md) | Done (scoped contract) | [TODO](../TODO.md#ai-execution) |
| [Export source capture and independent native receipts](export-source-settlement.md) | Done (scoped contract) | [TODO](../TODO.md#ai-execution) |
| [Driver RPC v2 savepoints](extension-savepoints.md) | Done (scoped contract) | [TODO](../TODO.md#extensions-and-mcp) |
| [External-provider estimated plans](external-estimated-plans.md) | Done (scoped contract) | [TODO](../TODO.md#extensions-and-mcp) |
| [External-provider notifications](external-notifications.md) | Done (scoped contract) | [TODO](../TODO.md#extensions-and-mcp) |
| [GitHub owner onboarding and team admission](github-owner-onboarding.md) | Done (scoped contract) | [TODO](../TODO.md#security-and-administration) |
| [GPUI Desktop Client](gpui-desktop.md) | Partial / active | [TODO](../TODO.md#desktop-quality-and-architecture) |
| [Grid apply captured source and owned settlement](grid-source-settlement.md) | Done (scoped contract) | [TODO](../TODO.md#ai-execution) |
| [HTTP execution source attribution](http-execution-source-attribution.md) | Done (scoped contract) | [TODO](../TODO.md#ai-execution) |
| [HTTP query settlement ownership](http-query-settlement-ownership.md) | Done (scoped contract) | [TODO](../TODO.md#ai-execution) |
| [SQL IDE and DBMS Feature Coverage](ide-parity-and-provider-extensibility.md) | Coverage inventory | [TODO](../TODO.md#next-milestone) |
| [Keyboard-first desktop interaction language](keyboard-first-desktop.md) | Partial / active | [TODO](../TODO.md#desktop-quality-and-architecture) |
| [Native Linux Debian distribution](linux-debian-distribution.md) | Partial / active | [TODO](../TODO.md#platform-distribution-and-native-acceptance) |
| [Metadata migration lifecycle](metadata-migration-lifecycle.md) | Done (scoped contract) | [TODO](../TODO.md#recovery-and-maintenance) |
| [Migration apply source attribution and owned settlement](migration-source-settlement.md) | Done (scoped contract) | [TODO](../TODO.md#ai-execution) |
| [Bounded nested extension command forms](nested-extension-forms.md) | Done (scoped contract) | [TODO](../TODO.md#extensions-and-mcp) |
| [Direct XLSX/Parquet column mapping review](non-csv-mapping-review.md) | Done (scoped contract) | [TODO](../TODO.md#imports-and-transfers) |
| [Controlled serial A/B benchmark execution](paired-benchmark.md) | Done (scoped contract) | [TODO](../TODO.md#performance-workbench) |
| [Paired decoded-result verification](paired-result-verification.md) | Done (scoped contract) | [TODO](../TODO.md#performance-workbench) |
| [Desktop performance measurements](performance-measurements.md) | Reference / scoped evidence | [TODO](../TODO.md#performance-workbench) |
| [Live Benchmark and Profile progress](performance-progress.md) | Done (scoped contract) | [TODO](../TODO.md#performance-workbench) |
| [Observed performance session settings](performance-session-settings.md) | Done (scoped contract) | [TODO](../TODO.md#performance-workbench) |
| [Reviewed PostgreSQL default privileges](postgres-default-privileges.md) | Done (scoped contract) | [TODO](../TODO.md#postgresql) |
| [PostgreSQL feature map](postgres-feature-map.md) | Reference / scoped evidence | [TODO](../TODO.md#postgresql) |
| [Native PostgreSQL ACL provenance](postgres-grant-provenance.md) | Done (scoped contract) | [TODO](../TODO.md#postgresql) |
| [Reviewed PostgreSQL object privilege editing](postgres-object-privilege-editor.md) | Done (scoped contract) | [TODO](../TODO.md#postgresql) |
| [PostgreSQL native privilege matrix](postgres-privilege-matrix.md) | Done (scoped contract) | [TODO](../TODO.md#postgresql) |
| [PostgreSQL provider gap checklist](postgres-provider-gaps.md) | Partial / active | [TODO](../TODO.md#postgresql) |
| [Reviewed PostgreSQL role session defaults](postgres-role-defaults.md) | Done (scoped contract) | [TODO](../TODO.md#postgresql) |
| [PostgreSQL role lifecycle and public attributes](postgres-role-lifecycle.md) | Done (scoped contract) | [TODO](../TODO.md#postgresql) |
| [PostgreSQL role membership workbench](postgres-role-memberships.md) | Done (scoped contract) | [TODO](../TODO.md#postgresql) |
| [PostgreSQL and SQL Server graduation](postgres-sqlserver-graduation.md) | Done (scoped contract) | [TODO](../TODO.md#postgresql) |
| [Product refactors and improvements](product-improvements.md) | Partial / active | [TODO](../TODO.md#desktop-quality-and-architecture) |
| [Profile node inspection and linked observations](profile-node-navigation.md) | Done (scoped contract) | [TODO](../TODO.md#performance-workbench) |
| [Provider and operations completion](provider-and-operations-completion.md) | Partial / active | [TODO](../TODO.md) |
| [Native receipts for HTTP, room and WebSocket SQL](query-native-settlement.md) | Done (scoped contract) | [TODO](../TODO.md#ai-execution) |
| [Query performance workbench](query-performance.md) | Partial / active | [TODO](../TODO.md#performance-workbench) |
| [Recipe execution capture and terminal receipts](recipe-source-settlement.md) | Done (scoped contract) | [TODO](../TODO.md#ai-execution) |
| [Relational table viewer and ER diagram](relational-table-viewer-and-erd.md) | Partial / active | [TODO](../TODO.md#desktop-quality-and-architecture) |
| [Reproducible Sift instances](reproducible-instances.md) | Partial / active | [TODO](../TODO.md#reproducible-instances) |
| [Result filter builder redesign](result-filter-builder-ux.md) | Partial / active | [TODO](../TODO.md#desktop-quality-and-architecture) |
| [Saved Profile plan comparison](saved-plan-comparison.md) | Done (scoped contract) | [TODO](../TODO.md#performance-workbench) |
| [SQL editor interaction performance](sql-editor-interaction-performance.md) | Reference / scoped evidence | [TODO](../TODO.md#performance-workbench) |
| [SQL Editor Productivity And Execution Completeness](sql-editor-productivity-and-execution.md) | Done (scoped contract) | [TODO](../TODO.md#desktop-quality-and-architecture) |
| [Shared Dialect-Aware SQL Semantic Service](sql-semantic-service.md) | Done (scoped contract) | [TODO](../TODO.md#desktop-quality-and-architecture) |
| [SQLite native nullable column additions](sqlite-add-columns.md) | Done (scoped contract) | [TODO](../TODO.md#sqlite) |
| [SQLite native column removal](sqlite-drop-columns.md) | Done (scoped contract) | [TODO](../TODO.md#sqlite) |
| [SQLite named parameter mapping](sqlite-named-parameters.md) | Done (scoped contract) | [TODO](../TODO.md#sqlite) |
| [SQLite native table rename milestone](sqlite-native-renames.md) | Done (scoped contract) | [TODO](../TODO.md#sqlite) |
| [Scoped SQLite statistics and index maintenance](sqlite-object-maintenance.md) | Done (scoped contract) | [TODO](../TODO.md#sqlite) |
| [SQLite provider gap checklist](sqlite-provider-gaps.md) | Partial / active | [TODO](../TODO.md#sqlite) |
| [SQLite provider design](sqlite-provider.md) | Done (scoped contract) | [TODO](../TODO.md#sqlite) |
| [SQLite restore design](sqlite-restore.md) | Done (scoped contract) | [TODO](../TODO.md#sqlite) |
| [Reviewed SQL Server database users](sqlserver-database-users.md) | Done (scoped contract) | [TODO](../TODO.md#sql-server) |
| [SQL Server effective-permission matrix](sqlserver-effective-privileges.md) | Done (scoped contract) | [TODO](../TODO.md#sql-server) |
| [SQL Server reviewed generated-column imports](sqlserver-generated-import-columns.md) | Done (scoped contract) | [TODO](../TODO.md#sql-server) |
| [Reviewed SQL Server index maintenance](sqlserver-index-maintenance.md) | Done (scoped contract) | [TODO](../TODO.md#sql-server) |
| [SQL Server Agent and settings native boundaries](sqlserver-inspection-acceptance.md) | Done (scoped contract) | [TODO](../TODO.md#sql-server) |
| [SQL Server provider gap checklist](sqlserver-provider-gaps.md) | Partial / active | [TODO](../TODO.md#sql-server) |
| [Sift state backup and restore](state-backup-restore.md) | Done (scoped contract) | [TODO](../TODO.md#recovery-and-maintenance) |
| [Ordered XLSX and Parquet upload projection](tabular-upload-projection.md) | Done (scoped contract) | [TODO](../TODO.md#imports-and-transfers) |
| [Tailnet database connections](tailnet-connections.md) | Partial / active | [TODO](../TODO.md#desktop-quality-and-architecture) |
| [Transaction execution fencing](transaction-execution-fencing.md) | Done (scoped contract) | [TODO](../TODO.md#ai-execution) |
| [Exact decimal transfer expressions](transfer-decimal-expressions.md) | Done (scoped contract) | [TODO](../TODO.md#imports-and-transfers) |
| [Portable transfer text expressions](transfer-text-expressions.md) | Done (scoped contract) | [TODO](../TODO.md#imports-and-transfers) |
| [WebSocket execution source attribution](websocket-execution-source-attribution.md) | Done (scoped contract) | [TODO](../TODO.md#ai-execution) |
| [WebSocket query settlement](websocket-query-settlement.md) | Done (scoped contract) | [TODO](../TODO.md#ai-execution) |
| [Windows launcher process lifetime](windows-launcher-lifetime.md) | Done (scoped contract) | [TODO](../TODO.md#platform-distribution-and-native-acceptance) |
| [Native Windows distribution milestones](windows-native-distribution.md) | Partial / active | [TODO](../TODO.md#platform-distribution-and-native-acceptance) |
