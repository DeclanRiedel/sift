# SQL Editor Productivity And Execution Completeness

Status: **active implementation plan.** This plan implements ADR-053 and the
corresponding unchecked SQL editor, intelligence, and execution inventory.
Checked milestones mean the scoped implementation and evidence gate exist;
they do not announce a release or support commitment.

## Product outcome

The SQL editor should feel immediate and database-aware in the same places a
dedicated database IDE does: pressing `Tab` accepts context-ranked table,
alias, field, function, and keyword completion; hover explains the exact symbol
under the caret; star expansion produces reviewable dialect-correct SQL;
templates remove repetitive typing; and every result returned by a batch or
stored procedure remains independently navigable.

Correctness is scoped to the active connection. A PostgreSQL editor uses the
PostgreSQL pack and that connection's current database/catalog projection. A
SQL Server editor uses the T-SQL pack and that connection's database projection.
No desktop fallback guesses a dialect or mixes catalogs from another
connection.

## Ownership

```text
GPUI editor/results
        │ typed HTTP and WebSocket contracts
        ▼
sift-server
  ├─ semantic document and filtered catalog orchestration
  ├─ execution coordinator, result store, spill, and progress
  └─ operation authorization and sanitized audit
        │                              │
        ▼                              ▼
sift-semantic                     Driver trait
  ├─ portable document/HIR         ├─ PostgreSQL driver
  ├─ PostgreSQL pack               └─ SQL Server driver
  └─ T-SQL pack
```

- `sift-protocol` owns serde/schemars DTOs only.
- `sift-semantic` owns document revisions, syntax artifacts, portable HIR,
  catalog binding indexes, request limits, and pack orchestration.
- Dialect packs own grammar, recovery, scopes, type rules, quoting, variables,
  star expansion, and completion enrichments.
- `sift-server` owns live catalog selection, authorization, deadlines,
  execution state, cursor/spill lifecycle, and all public operations.
- Drivers own physical protocol decoding and native progress observation.
- GPUI owns presentation, focus, Vim interaction, and stale-answer rejection;
  it does not parse SQL.

## Completion quality contract

Completion is driven by the active semantic document and active connection's
filtered catalog revision. Candidates combine:

1. aliases and projected fields visible at the cursor;
2. CTEs and temporary/document-created objects in scope;
3. fields belonging to relations already present in the statement;
4. tables, views, routines, types, and schemas from the selected database;
5. dialect functions and keywords; and
6. snippets whose dialect and trigger match.

Ranking uses context before fuzzy score. After `FROM`, relations outrank
columns. After `alias.`, only fields and members of that exact binding are
shown. In a projection or predicate, in-scope columns outrank global objects.
Recently selected candidates may break ties only within the same semantic
class and catalog revision. Hidden or unauthorized objects never enter the
candidate set.

Every candidate includes a replacement range, insertion text, display label,
kind, qualified detail, optional type/nullability, documentation, stable sort
text, and optional additional edits. `Tab` accepts the selected candidate in
Vim insert mode. `Ctrl-n`/`Ctrl-p` and arrows navigate the menu; `Escape`
dismisses it. Accepting a table may add an alias only when a configured,
deterministic alias rule is enabled. Accepting a field never invents a
qualifier when binding is ambiguous.

Warm completion must not perform database I/O. Catalog refresh/invalidation
advances the catalog revision shared by completion, diagnostics, hover, and
prepared edits.

## Semantic model

The portable bound model records statement scopes, relation bindings,
projection items, symbols, expressions, inferred types, nullability, source
ranges, stable catalog ids where available, and certainty:

```text
exact | inferred | ambiguous | catalog_incomplete | unresolved
```

The indexed catalog binding view contains qualified object identity, kind,
ordered columns, native and portable types, nullability, defaults, generated
state, comments, routine signatures, and completeness. It is immutable and
shared by `Arc`; folded-name and stable-id indexes are built once per catalog
revision.

The first implementation may retain `sqlparser-rs` as a valid-statement AST
bridge, but editor correctness requires a lossless, error-recovering syntax
artifact. Syntax representation stays private to packs so corpus evidence can
drive a later parser replacement without changing public contracts.

## Hover contract

Hover returns the exact source range and any available symbol, expression,
catalog object, type, nullability, definition location, comment, and binding
certainty. PostgreSQL inference covers casts, arrays, domains, enums,
composites, common operators, numeric precision/scale, and overload selection.
T-SQL inference covers precedence, string lengths, decimal arithmetic,
`CASE`, `ISNULL`, `COALESCE`, alias types, and known routine signatures.

Unknown and ambiguous types remain explicit. Procedure and dynamic-SQL result
shapes are not guessed. Hover runs after a short pointer delay and through a
Vim keyboard command; stale semantic or catalog revisions are discarded.

## Star expansion contract

Star expansion is a prepared, revision-bound text edit. Initial safe support:

- `alias.*` with one exact relation binding;
- unqualified `*` with exactly one relation;
- CTE/subquery projections whose ordered fields are known; and
- dialect-correct quoting with original alias spelling preserved.

Advanced support adds multiple relations, `JOIN ... USING`, natural joins,
temporary tables, table-valued functions, PostgreSQL composite records, and
T-SQL `inserted`/`deleted` pseudo tables. Expansion fails closed when catalog
coverage, ordering, scope, or dynamic shape is incomplete. Large edits receive
a preview and apply as one undo unit.

## Variables and snippets

Sift execution variables use `{{name}}`, `{{ident:name}}`, and
`{{list:name}}`. Values become bind parameters; identifiers pass through the
selected dialect's quoting routine; lists become bounded sequences of bind
parameters. Raw SQL variables are unavailable. Native `$1` and T-SQL `@name`
remain native syntax.

Resolution precedence is run prompt, run configuration, query tab, workspace,
connection profile, then tenant defaults. Secret values resolve from opaque
`SecretStore` handles and never enter resolved history, audit, logs, or error
text. History retains the template plus redacted variable descriptors.

Snippet tabstops use `$0`, `$1`, and `${1:default}`, keeping them distinct from
execution variables. Snippets are immutable built-ins or versioned personal,
workspace, and tenant records with dialect allowlists. Ordinary expansion is
local from a cached index. Catalog-generated templates are server-prepared
against the exact catalog revision.

## Execution event contract

Execution v2 publishes this ordered lifecycle:

```text
execution_started
  statement_started*
    result_set_started
      rows*
    result_set_completed
    command_completed*
  notice*
  progress*
execution_completed | error
```

Rows carry a result-set id. Result-set completion and execution completion are
different events. Command results retain affected rows, command tag, statement
ordinal when known, duration, and warnings. Stored-procedure result sets may
have unknown source-statement attribution; the protocol says so rather than
guessing.

PostgreSQL executes semantic statement units sequentially on the same physical
connection when typed extended-protocol results are available, stopping on the
first error by default and preserving explicit transaction state. Streaming
simple-query mode remains a bounded fallback for constructs that require a raw
batch. SQL Server streams native TDS metadata boundaries; `GO` is a semantic
client batch separator and is never sent as SQL.

## Result retention and UI

One execution owns ordered row-set and command items plus messages. Every row
set has independent columns, row count, duration, warnings, filter/sort state,
export state, page cursor, spill state, and terminal status. The server keeps a
bounded initial window and spills additional pages under existing session
quotas and TTLs. Paging addresses execution and result-set identity.

Desktop tabs show `Results 1`, `Results 2`, command summaries, and Messages.
Only the active result grid materializes display cells or participates in
layout. Inactive tabs keep summary state and server page references. Closing an
execution releases retained pages and spill files.

## Progress honesty

Portable progress reports queueing, connection acquisition, preparation,
waiting for first row, streaming, spilling, cancelling, elapsed time, current
statement, result-set count, rows, and bytes. It never synthesizes percentage
from elapsed time.

PostgreSQL and SQL Server native percentages are optional extension telemetry,
sampled on separate bounded monitor connections no faster than four times per
second. Monitor timeout, pool pressure, permission failure, or unsupported
command silently degrades to portable indeterminate progress and never changes
query outcome.

## Milestones and evidence gates

### 1. Contract and ADR

- [x] ADR-053 records dialect, catalog, execution, progress, variable, and
  snippet ownership.
- [x] This plan records all twelve implementation milestones and quality gates.

Gate: docs agree with ADR-017, ADR-032, ADR-033, protocol purity, operation
audit, secret storage, and Vim-only interaction rules.

### 2. Execution v2 wire contract

- [x] Add typed execution, statement, result-set, command, notice, progress,
  error, and completion events.
- [x] Add ids, summaries, compatibility projection, and OpenAPI components.
  SDK streaming integration follows the live server path in milestone 3.
- [x] Keep protocol free of runtime and driver dependencies.

Gate: serde fixtures prove event ordering, forward-additive compatibility,
bounded fields, and explicit first-result truncation in legacy projection.

### 3. Multi-result execution and tabs

- [x] Normalize PostgreSQL and SQL Server driver streams behind explicit v2
  execution events and add gated live-engine batch fixtures.
- [x] Retain each result set in a bounded inactive desktop slot while cursor
  backpressure and existing spill/resume remain the transport bound.
- [x] Replace desktop first-result truncation with ordered result tabs and
  command/message items.

Gate: both live drivers return two row sets plus a command; cancellation,
spill/resume, export, and tab closure retain correct ownership and bounds.

### 4. Portable progress

- [x] Publish portable queue/first-row/stream phases through v2 events with
  room for acquire/prepare/spill/cancel producers at their ownership points.
- [x] Add coalesced progress/status UI with elapsed time, statement ordinal,
  row and byte counters, and cancellation.

Gate: progress never blocks stream acknowledgement, exceeds frequency bounds,
or reports terminal success before execution completion.

### 5. Rich binding and contextual completion

- [x] Add typed indexed catalog binding view and bound semantic scopes.
- [x] Rank tables and fields from the active connection/database context.
- [x] Resolve aliases, CTEs, temporary objects, qualified fields, routines,
  functions, schemas, and keywords through one pipeline.
- [x] Preserve Vim `Tab` acceptance and completion navigation.

Gate: PostgreSQL and T-SQL corpora prove no cross-connection/catalog leakage,
alias-qualified fields, ambiguous-column handling, stale revision rejection,
and warm completion without I/O.

### 6. Hover

- [x] Add protocol, operation, route, SDK, server orchestration, pack logic,
  and GPUI presentation.

Gate: typed columns, objects, aliases, CTEs, functions, expressions,
nullability, comments, uncertainty, and stale responses pass both dialects.

### 7. Safe star expansion

- [ ] Add exact-binding `alias.*`, single-relation `*`, CTE, and subquery
  expansion with preview and one-undo application.

Gate: ordering, quoting, catalog revision, incomplete coverage, ambiguous
scope, UTF-8 ranges, and generated-edit validation fail safely.

### 8. SQL variables

- [ ] Add typed declarations, scope resolution, prompt UI, dialect compiler,
  bind generation, identifier quoting, source maps, and redacted history.

Gate: injection corpus, typed nulls, empty lists, limits, secret sentinels,
retry/cancel behavior, and both driver parameter encodings pass.

### 9. Snippets and templates

- [ ] Add versioned persistence, typed operations, audit, built-ins, cached
  completion index, tabstops, management UI, and catalog-generated templates.

Gate: scope authorization, dialect filtering, conflict revisions, sanitized
audit, local expansion latency, import bounds, and one-undo insertion pass.

### 10. Native progress

- [ ] Add optional PostgreSQL and SQL Server telemetry through extension
  traits and separate monitor connections.

Gate: supported maintenance command, unsupported query, permission failure,
monitor timeout, pool pressure, cancel, disconnect, and secret/log redaction
pass without changing execution outcome.

### 11. Advanced dialect depth

- [ ] Add multi-relation/`USING`/natural star projection, temp objects,
  table-valued functions, composite records, pseudo tables, overloads, and
  deeper dialect completion/type rules.

Gate: feature matrix records exact complete/partial/unsupported states for
both engines; no heuristic edit is presented as exact.

### 12. Hardening and performance graduation

- [ ] Add golden, mutation, fuzz, resource, live-engine, and UI performance
  corpora; update canonical feature inventory.

Gate: `cargo fmt`, strict workspace clippy, workspace tests, live-driver
fixtures, semantic latency budgets, result memory quotas, and GPUI frame
budgets pass with published evidence.

## Performance budgets

- Warm completion server p95 at most 30 ms; visible response at most 50 ms.
- Warm hover server p95 at most 30 ms.
- Incremental changed-statement semantic work p95 at most 5 ms.
- Full 8,000-line document semantic pass p95 at most 50 ms off async runtime.
- Star expansion p95 at most 50 ms for a 500-column visible catalog scope.
- Local snippet lookup and insertion preparation at most 2 ms.
- Progress publication at most 4 Hz per execution.
- Only active result grid may shape cells; inactive tab navigation remains
  inside the 8.3 ms 120 Hz frame budget.
- Execution/result memory and spill remain bounded by documented per-session
  quotas independent of total rows or result-set count.

## Completion corpus minimums

Both dialects require cases for empty input, partial keywords, quoted and
case-folded identifiers, schema/database qualification, aliases, joins,
ambiguous columns, nested subqueries, correlated references, CTE shadowing,
temporary objects, functions, procedures, DML targets, `RETURNING`/`OUTPUT`,
comments, strings, invalid tokens, incomplete statements, and catalog policy
filtering. Every fixture declares dialect id, connection/database identity,
catalog revision, cursor byte offset, expected replacement range, ordered top
candidates, and forbidden candidates.
