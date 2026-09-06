# PostgreSQL and SQL Server graduation

Status: **planned, code-reviewed 2026-09-06; acceptance remains open.** This
checklist defines a bounded daily-driver support milestone. ADR-017 already
locks the core Driver contract; this plan does not reopen it or claim that
either engine passes the acceptance gate today. The review did not run tests.

The [canonical inventory](ide-parity-and-provider-extensibility.md) owns product
feature status; [DDL gaps](ddl-gaps.md) owns detailed DDL implementation notes.
Graduation means the declared scope is reliable and evidenced, with explicit
unsupported states. It does not require every database administration feature.

## Scope and design

- [ ] Publish a two-engine support matrix covering tested engine versions,
      authentication/TLS modes, values and parameters, schema objects, plans,
      process control, bulk import, PostgreSQL notifications, and permissions.
- [ ] Classify each gap below as supported work or an explicit scope exclusion.
      Any unsupported schema shape must fail explicitly rather than emit
      plausible DDL that silently loses properties.
- [ ] Design metadata/request changes before implementation. Update the ADR
      and bump the protocol for locked public-shape changes, including an
      addressable index object if added. Keep engine-only behavior outside the
      locked core Driver trait.

## DDL fidelity

- [x] Implement sequence definitions for both engines. Live acceptance remains
      open; PostgreSQL ownership and runtime counters are not exported.
- [ ] Generate standalone trigger DDL for both engines.
- [ ] Generate PostgreSQL composite/enum/domain and scoped SQL Server
      user-defined type DDL; document excluded type families.
- [ ] Preserve generated/computed expressions and generation/persistence
      properties for supported columns.
- [ ] Preserve non-default column collations.
- [ ] Preserve identity configuration: PostgreSQL mode/options and SQL Server
      seed/increment. Current formatter forces ALWAYS and IDENTITY(1,1).
- [ ] Preserve supported index properties and add standalone index addressing,
      or document an explicit exclusion. Current formatter reconstructs names,
      columns, uniqueness, and predicates; richer index definitions need proof.
- [ ] Preserve partition definitions or reject unsupported partitioned-table
      DDL. PostgreSQL PartitionedTable currently uses the plain table formatter.
- [ ] Exercise corrected shapes through schema snapshots/diff/migration paths
      where supported, not only the object DDL viewer.

## Values and execution boundaries

- [ ] Resolve or explicitly scope PostgreSQL Interval parameter rejection and
      text-only values/metadata in the simple-query batch fallback.
- [ ] Resolve or explicitly scope SQL Server Native parameter rejection and
      undecoded sql_variant/UDT results. Distinguish typed NULL requirements
      from unsupported types; do not promise lossless round trips for placeholders.
- [ ] Verify precision, NULLs, native values, batch result boundaries, affected
      rows, and parameterized writes across the declared type matrix.

## Acceptance evidence

- [ ] Run existing live-pg and live-mssql driver suites against configured
      engines; record versions, commands, results, and remaining limitations.
- [ ] Expand PostgreSQL DDL round trips for the supported fidelity fixes and
      execute the new non-default sequence fixtures.
- [ ] Add SQL Server DDL round trips, including batch handling and non-default
      identity/sequence configuration.
- [ ] Establish live coverage for supported plan capture, import, process
      control, and PostgreSQL notification workflows. Existing dedicated live
      plan coverage is PostgreSQL-only; reuse fixtures before adding tests.
- [ ] Verify transaction/savepoint behavior, timeout/cancel, close mid-stream,
      disconnect recovery, and restricted-user behavior through server paths.
- [ ] Record large-result/schema performance and bounded resource behavior for
      both engines; agree acceptance budgets before declaring success.
- [ ] Run cargo fmt, cargo clippy --workspace --all-targets -- -D warnings,
      and cargo test --workspace, plus the opt-in live suites. Keep CI manual;
      add no smoke scripts or workflows.
- [ ] Record the scoped graduation decision and evidence in docs/DECISIONS.md;
      reconcile partial engine rows in the canonical inventory individually.

## Accepted limits and deferred work

ADR-017 permits SQL Server abort-and-discard cancellation, no MARS, no savepoint
release, and CSV bulk import without native typed TDS bulk loading. These do not
independently block this milestone. Its original no-pooling note is historical:
the current SQL Server driver already implements a per-spec warm-idle pool;
validate that behavior rather than queueing pooling as missing functionality.

Foreign-table server/options metadata, PostgreSQL extension DDL, SQL Server
synonym DDL, PostgreSQL sequence ownership, and richer dynamic semantic
inference may remain explicit exclusions. Runtime sequence counters are outside
schema export. The optional AST-equivalence DDL check is not an acceptance gate.

Keep lock/deadlock tooling, alerts/dashboards, database security editors,
dump/backup/restore, maintenance, replication, Query Store, SQL Server Agent,
and engine settings browsers in the product backlog. They are not prerequisites
for calling the supported query/schema workflows graduated.

## Recommended next-provider order

1. Close this bounded correctness and live-acceptance milestone.
2. Implement the [designed SQLite query/schema/transaction slice](sqlite-provider.md)
   before expanding the PostgreSQL/SQL Server DBA workbench.
3. Continue shared IDE improvements according to product priority.

SQLite design must decide server-owned file paths and access policy (including
remote sessions), connection ownership, blocking-work isolation, cancellation,
locking/busy behavior, transactions, value typing, schema/DDL fidelity, dialect
intelligence, and capability gating. Sift's internal SQLite metadata store is
not a user-database provider and must remain separate.

The SQLite design selects native integration with the existing Driver trait.
External Driver RPC v1 need not expand for that slice. The
[four-task handoff](database-provider-overnight.md) makes implementation dependent
on the preceding correctness, live-evidence, and graduation tasks. This is
sequencing guidance, not an implemented SQLite feature or a graduation claim.
