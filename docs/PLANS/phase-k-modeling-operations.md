# Phase K Modeling Operations

Status: **implemented and graduated on 2026-08-10.** This document locks the Phase K
contracts for table/result comparison, diagram projection, and semantic
query-plan capture. Catalog truth and migration behavior are defined by
ADR-033 in `catalog-graph-schema-migrations.md`; semantic document identity is
defined by ADR-032 in `sql-semantic-service.md`.

## Shared rules

These features are server-owned, connection/tenant scoped, bounded,
cancellable, policy-filtered, audited, and exposed through typed protocol,
HTTP, OpenAPI, and the reference SDK. Clients may choose layout and rendering,
but cannot manufacture catalog identities, comparison truth, executable patch
SQL, or plan provenance.

Requests bind exact source revisions. A stale catalog, cursor/result reference,
semantic document revision, statement id, profile-policy revision, or retained
artifact fails explicitly; the server never substitutes latest state. Source
SQL, bind values, compared row values, predicates, generated patch values,
plan predicates, and raw plan text never enter audit, metrics labels, traces,
or ordinary logs.

## Table and result comparison

### Sources and identity

A comparison accepts exactly two `CompareSource`s:

- `table { connection, catalog_revision, object_id, optional_filter }`;
- `query_result { cursor_id, result_set, schema_digest }`; or
- `room_result { room_id, result_id, result_set, schema_digest }`.

Both sources must remain authorized for the caller. A table filter uses a
bounded semantic predicate contract and bind values; arbitrary interpolated
SQL is not accepted. Result sources are immutable retained pages. Cross-engine
comparison is supported after both sides normalize into protocol `Value`s;
engine-native values with no safe common comparison report `incomparable`.

Column mapping defaults to exact case-aware engine identity, then explicit
request mappings. It never silently pairs fuzzy names. The response records
missing, extra, incompatible, and mapped columns before row work starts.

### Key selection and duplicates

`CompareKey` is explicit columns, inferred primary key, inferred non-null
unique constraint, or row ordinal. Inference is allowed only for a table and
returns the chosen catalog constraint identity. Row ordinal is allowed only
for two immutable results with deterministic retained order; it is forbidden
for live tables. If no safe key exists the request must provide one or fail.

Keys are compared as typed tuples with explicit null ordering. Duplicate keys
are not overwritten: each side forms a bounded multiset group. Equal rows are
matched first by canonical row digest, then remaining rows pair in stable
lexicographic digest/order; surplus members become added/removed rows. The
response marks duplicate groups so a client cannot mistake an arbitrary pair
for identity.

### Value equality

Exact typed equality is the default. Optional per-column tolerances are typed
and bounded:

- numeric absolute and/or relative tolerance, with explicit NaN/infinity
  behavior;
- timestamp tolerance in microseconds after preserving instant-vs-local type;
- text Unicode normalization, case folding, or outer-whitespace handling; and
- binary digest comparison for large values.

`NULL` equals only `NULL`. Missing, conversion failure, truncation, and
unsupported types are distinct from unequal. Values are never compared through
display strings. Tolerance settings and type coercions are echoed in the
result and included in its digest.

### Execution, paging, and patch generation

The v1 server hard-admits each normalized source by row, byte, and column caps,
then compares that bounded working set in memory. It never accumulates an
unbounded side. Retained row-diff pages spill through authenticated encryption
once their small in-memory threshold is crossed. This deliberately avoids an
untrusted external-sort implementation while preserving a measurable hard
memory ceiling; a future streaming merge can optimize below that ceiling
without changing comparison semantics. Admission also caps duplicate-group
size, runtime, retained diff rows, and total retained bytes. A `ComparisonId`
owns immutable keyset-paged summaries and row diffs until TTL/eviction.
Cancellation is cooperative during source loading and comparison; truncation
is explicit and prevents patch generation.

Optional patch generation targets one live table only. It requires a proven
primary/non-null-unique identity, a complete comparison, exact target catalog
revision, and expected old values. INSERT/UPDATE/DELETE reuse ADR-023's
parameterized optimistic DML builder. The comparison API only prepares a
bounded patch; apply remains the existing audited edit apply path, preserving
conflict detection. Cross-engine, ordinal-key, duplicate-key, tolerant-equal,
partial, or incompatible comparisons cannot generate a patch.

Operations are `StartComparison`, `PageComparison`, `CancelComparison`, and
`PrepareComparisonPatch`. Start/prepare are heavy; page is interactive;
cancel is control. Row values and filters are sanitized from Operation replay.

## Diagram projection

A `CatalogDiagram` is a deterministic projection of one visible
`CatalogGraph` revision. The request selects schemas, object ids, dependency
edge kinds, neighborhood depth, and inclusion of columns/routines. The server
returns graph revision/digest, bounded nodes and edges, omitted counts, and
partial/inaccessible boundary markers.

Diagram nodes reference `CatalogObjectId` and contain only authorized catalog
facts. Foreign keys retain ordered source/target column pairs. Other dependency
edges retain kind and certainty. Stable ordering is by normalized qualified
identity, never catalog query arrival order.

Coordinates, routing, colors, collapsed state, viewport, and automatic layout
are client concerns and do not enter catalog state. A future workspace may
persist presentation preferences separately. Visual edits are declarative
mutation intents (`add_foreign_key`, `drop_foreign_key`, `rename_object`, and
supported column changes) translated into an ADR-033 schema diff/migration
preview. There is no diagram-specific DDL executor and no direct graph
mutation.

`ProjectCatalogDiagram` is a non-destructive Operation. Preparing a diagram
mutation uses the normal migration-preview Operation and safety rules.

## Semantic query-plan capture

### Provenance

Plan capture extends the existing engine-normalized `ExplainResponse`. A
request names an open semantic document, exact revision, selected
`StatementId`, connection catalog revision, analyze flag, and bind parameters.
The server re-runs statement selection, verifies the id/range/kind against the
same immutable semantic revision, extracts SQL internally, and routes through
the existing bounded explain implementation. Clients do not resend SQL.

A `PlanCapture` records an opaque id, actor/tenant/connection profile,
provider/dialect and server versions, semantic source digest, document
revision, statement id and normalized statement fingerprint, catalog revision,
analyze flag, capture time/duration, normalized `PlanNode`, warnings, and
redaction/completeness metadata. It never stores semantic source text or bind
values.

### Retention and privacy

Captures are durable tenant metadata with operator-bounded count, normalized
tree bytes, age, and per-document history. Default retention is the newest 50
captures per semantic source fingerprint and 30 days; operators may lower it.
Semantic document ids are process-local, so retrieval keys primarily by source
digest plus optional document id while it remains live.

Normalized plans are scrubbed before persistence: predicate strings, literal
values, parameter values, ad-hoc SQL fragments, temp object names, and
engine-specific fields not on an allowlist are removed or fingerprinted. The
raw PostgreSQL JSON or SQL Server XML may be returned in the immediate response
under the existing response byte cap, but is never persisted. Callers may opt
out of the immediate raw response. A plan that cannot be normalized without
retaining sensitive raw fields fails durable capture rather than storing them.

Analyze follows ADR-025: PostgreSQL non-read statements execute inside a
rollback boundary; unsupported SQL Server analyze remains explicit until its
multi-result capture is safely implemented. Analyze requires the same write
authorization the selected statement would require, even when rolled back.
Estimate-only capture is non-destructive. Timeout/cancel never persists a
successful capture.

Retrieval supports bounded keyset paging by source fingerprint and capture
time. Delete is revision-guarded and destructive. Captures are invalidated
only by retention/delete, not by later catalog or semantic revisions; stale
provenance remains visible and comparable. Optional server-side comparison of
two normalized captures reports operator/cardinality/cost changes but never
compares costs across engines.

Operations are `CaptureSemanticPlan`, `ListPlanCaptures`, `GetPlanCapture`,
`ComparePlanCaptures`, and `DeletePlanCapture`. All have HTTP/OpenAPI/SDK
coverage and audit only ids, revisions, counts, timings, and fingerprints.

## Public API outline

Connection-scoped endpoints start/page/cancel comparisons, project diagrams,
and capture plans. Tenant-scoped endpoints page/delete retained comparisons
and plan captures where retention is enabled. Exact paths follow the existing
`/v1/sessions/{session}/connections/{connection}` and `/v1/metadata/tenants`
families and use stable unique operation ids.

Protocol DTOs reject unknown request fields, validate every size/range before
database work, use opaque keyset continuation tokens, and represent partial,
truncated, incomparable, stale, and unsupported outcomes explicitly. Response
caps apply before serialization as well as at spill/persistence boundaries.

## Graduation evidence

- comparison matrices cover key inference, composite/null/duplicate keys,
  every protocol value family, tolerances, cross-engine coercion, paging,
  spill, cancellation, truncation, and patch refusal/apply conflicts;
- diagram fixtures prove deterministic projection, column-pair fidelity,
  policy filtering, partial boundaries, and mutation-to-preview equivalence;
- plan fixtures cover both engine formats, exact semantic provenance, stale
  revisions, analyze authorization/rollback, retention, raw-plan exclusion,
  redaction, and normalized plan comparison;
- hostile sizes, malformed provider values, tenant isolation, audit replay,
  OpenAPI drift, and SDK manifest parity are tested; and
- large-table/large-result/large-graph latency and retained-memory budgets are
  measured and published with the Phase K graduation matrix.
