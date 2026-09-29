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
- [~] Add schema snapshots, diff, migration preview/apply, and designer changes
      only for DDL shapes the native model can round-trip. Durable snapshots and
      partial-coverage diff work; audited preview, test rollback, and apply now
      admit additive creation of simple ordinary `main` tables with native
      INTEGER/REAL/TEXT/BLOB/NUMERIC columns and optional NOT NULL. Stored DDL
      is verified before a canonical statement is retained. Lossy/virtual,
      indexed, constrained, generated, STRICT, WITHOUT ROWID, TEMP, drop,
      rename and alter shapes are refused. Designer and broader native DDL
      round trips remain open (ADR-070).
- [x] Improve CHECK metadata coverage while stored native SQL remains
      authoritative. Table and column clauses now scan valid SQLite table DDL
      without depending on whole-statement editor parsing; quoted names,
      comments, nested expressions, and native enforcement have real-file tests.
      Metadata deliberately omits virtual-table clauses and CREATE statements
      larger than 1 MiB; stored native SQL remains the source of truth.
- [ ] Define safe identity and write rules for any additional editable table
      shapes; virtual tables and nullable/partial/expression keys remain gated.

## Execution and workbench

- [~] Add bounded native bulk/transfer targets with explicit affinity and
      decimal-conversion limits. The audited HTTP/SDK native bulk target now
      previews typed rows against an authorized ordinary `main` table and
      applies a one-use confirmed request in bounded transactional batches
      (ADR-074). Decimal text is limited to 38 digits and scale 18; NUMERIC
      affinity and unsupported coercions are refused. CSV and transfer recipes
      still use their existing uploaded-file importer; desktop native target
      selection and streaming transfer-source integration remain open.
- [x] Provide a bounded runtime profile with useful SQLite evidence. The
      dedicated read-only execution reports Sift-observed full-consumption
      timing and completed rows alongside an estimated `EXPLAIN QUERY PLAN`.
      Native per-node actual rows, timing, and costs remain unavailable and
      are not inferred from the estimated plan.
- [~] Add scoped maintenance and database creation workflows for configured
      server roots, with preview, backup expectations, and audit. ADR-078 adds
      audited HTTP/SDK preview and one-use confirmed create-new database and
      online backup-to-new-file actions, constrained to the managed SQLite
      connection's writable root and tenant. The existing integrity check is
      read-only. The Vim Monitor now offers preview/confirmation for create
      and backup plus the read-only integrity action (ADR-081). VACUUM,
      restore/delete and arbitrary PRAGMA changes remain gated; creation needs
      no prior backup, while future
      mutating maintenance should use a verified backup first.
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
