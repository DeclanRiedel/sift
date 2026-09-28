# SQLite provider gap checklist

Status: active Linux backlog. SQLite passed the scoped ADR-056 `ide_capable`
contract for server-owned local files. This list tracks product gaps without
weakening root authority, transaction guards, or explicit unsupported states.
Use [SQLite support](../SQLITE.md), [acceptance evidence](database-provider-acceptance.md),
and the [product inventory](ide-parity-and-provider-extensibility.md) for current
claims.

## Catalog, schema, and editing

- [~] Complete dependency coverage beyond the partial navigation catalog.
      The graph now keeps SQLite foreign keys within their source schema,
      adds catalog-proven trigger targets, parsed trigger-body dependencies,
      parsed direct view reads, and FTS5 external-content dependencies. Missing
      or ambiguous targets stay unresolved. Parsed CHECK and partial-index
      predicates, generated columns, and expression indexes link to referenced
      columns when their stored SQL parses. Object-level gap markers identify
      omitted definitions. Unsupported expression syntax, virtual-table
      modules, table functions in views, and unparsed trigger/view SQL remain
      explicit coverage gaps; the graph is still partial.
- [ ] Add schema snapshots, diff, migration preview/apply, and designer changes
      only for DDL shapes the native model can round-trip. Reject lossy changes.
- [x] Improve CHECK metadata coverage while stored native SQL remains
      authoritative. Table and column clauses now scan valid SQLite table DDL
      without depending on whole-statement editor parsing; quoted names,
      comments, nested expressions, and native enforcement have real-file tests.
      Metadata deliberately omits virtual-table clauses and CREATE statements
      larger than 1 MiB; stored native SQL remains the source of truth.
- [ ] Define safe identity and write rules for any additional editable table
      shapes; virtual tables and nullable/partial/expression keys remain gated.

## Execution and workbench

- [ ] Add bounded native bulk/transfer targets with explicit affinity and
      decimal-conversion limits.
- [ ] Decide whether an actual-plan or runtime-profile workflow has useful
      SQLite evidence; estimated `EXPLAIN QUERY PLAN` must not invent costs.
- [ ] Add scoped maintenance and database creation workflows for configured
      server roots, with preview, backup expectations, and audit.
- [x] Open the authorized CSV quarantine artifact from an import result in the
      desktop, with rejected-row navigation and source-value details.
- [x] Configure and retry durable CSV import from the desktop using a target
      checkpoint table and stable run UUID.
- [x] Add a durable transfer-artifact history so quarantine reports can be
      reopened after the current import result is dismissed (within retention).
- [ ] Decide whether ATTACH, extensions, custom functions/collations, or
      virtual-table writes can meet Sift's file and authorization boundaries;
      keep them disabled until an accepted design exists.

## Acceptance

- [ ] Real-file Linux tests for every added catalog, migration, transfer, and
      maintenance operation, including read-only roots, concurrency, rollback,
      cancellation, and file-boundary refusal.
- [ ] Representative larger schema and result fixtures with measured resource
      limits; retain mixed SQLite storage-class behavior.
- [ ] `cargo fmt`, strict workspace Clippy, and workspace tests pass after each
      implementation slice; SQLite provider and HTTP tests pass for engine work.
