# SQLite provider gap checklist

Status: active Linux backlog. SQLite passed the scoped ADR-056 `ide_capable`
contract for server-owned local files. This list tracks product gaps without
weakening root authority, transaction guards, or explicit unsupported states.
Use [SQLite support](../SQLITE.md), [acceptance evidence](database-provider-acceptance.md),
and the [product inventory](ide-parity-and-provider-extensibility.md) for current
claims.

## Catalog, schema, and editing

- [ ] Complete dependency coverage beyond the partial navigation catalog;
      identify omitted edges rather than presenting a full graph.
- [ ] Add schema snapshots, diff, migration preview/apply, and designer changes
      only for DDL shapes the native model can round-trip. Reject lossy changes.
- [ ] Improve CHECK metadata/parser coverage while stored native SQL remains
      authoritative.
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
- [ ] Add a durable transfer-artifact history so quarantine reports can be
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
