# Catalog Graph, Schema Diff, and Migration Safety

Status: **implemented and graduated on 2026-08-10 (ADR-033).** This is the normative
Phase K contract for catalog identity, dependency discovery, durable schema
snapshots, normalized diffs, and migration preview/apply. It extends the
existing progressive `SchemaSnapshot` API; it does not replace it or change
the locked `Driver` trait signature.

## Goals and boundaries

The server owns one normalized, revisioned view of database structure that can
power semantic binding, diagrams, schema diff, migration ordering, usages, and
future workspace DDL sources. Clients render that truth; they do not infer
dependencies or generate executable migration SQL.

The graph covers catalogs, schemas, tables, views, materialized views,
foreign/partitioned tables, routines and overloads, triggers, types,
sequences, columns, indexes, and constraints. Dependencies include ownership,
containment, foreign-key references, view/routine/type references, trigger
targets, sequence/default ownership, and index/constraint column membership.
Permissions, unsupported engine metadata, timeouts, and size limits may make a
graph partial, but never silently complete.

Database contents, credentials, raw connection specifications, and secret
bytes are outside the graph and durable snapshot. Runtime rows remain in query
and comparison paths. CRDT state is never used for catalogs or migrations.

## Identity and revision model

`CatalogObjectId` is an opaque, bounded protocol string issued by Sift. It is
derived from the connection's provider/dialect identity, the database identity
reported at connection time, object kind, canonical qualified path, and
routine signature or subordinate object name. It is deterministic within one
database identity and naming state, but clients must not parse it.

Every node also carries an optional `native_id`. PostgreSQL OIDs and SQL Server
`object_id`/`column_id` values improve rename correlation inside one live
database, but are engine hints, not portable identity: they may be recycled,
change after restore, and never form authorization keys. A rename is automatic
only when the same live database revision proves native-id continuity and the
kind/signature remains compatible. Cross-database or durable-snapshot rename
matches are suggestions requiring an explicit request mapping.

`CatalogRevision` is a server-issued monotonic `u64` scoped to a canonical
connection-spec/database cache entry. It advances only when normalized graph
content or coverage changes. Each revision includes a deterministic
`content_digest`, an `invalidation_epoch`, capture time, provider/dialect and
pack versions, and a `CatalogCoverage` summary. Re-fetching identical content
after TTL expiry does not advance the revision. Wraparound evicts the entry and
requires a fresh graph.

Canonical graphs are cached without user-specific filtering, but are never
returned directly. Every consumer receives a visibility-filtered projection
keyed by tenant, principal, connection-profile policy revision, allowed schema
set, and canonical catalog revision. Nodes outside policy are removed; edges
to them become `inaccessible` boundary edges with no leaked name or metadata.
Semantic caches include the resulting visible revision/digest.

## Portable graph contract

The pure-serde protocol model is conceptually:

```text
CatalogGraph {
  revision, content_digest, captured_at, provider, database_identity,
  coverage, nodes, edges
}
CatalogNode {
  id, native_id?, kind, path, parent_id?, ordinal?, definition_digest?,
  properties, completeness
}
CatalogEdge {
  from, to?, kind, referenced_path?, column_pairs, certainty
}
CatalogCoverage {
  state: complete | partial | stale,
  requested_kinds, covered_schemas, omitted_schemas,
  truncation?, failures
}
```

Portable node properties are typed for columns, constraints, indexes,
triggers, routines, and types. Engine-only attributes live in a bounded
`extra` map. Raw definitions are opt-in and separately bounded; ordinary graph
reads carry normalized definition digests and summaries so a large routine
body cannot dominate the graph. Unknown enum values from an external provider
fail validation at the provider boundary rather than becoming executable
metadata.

Edges have a normalized kind and certainty (`catalog_proven`, `parsed`, or
`unresolved`). An unresolved reference carries a bounded qualified path but no
invented node. Duplicate ids, dangling proven edges, containment cycles,
invalid column ordinals, over-limit strings/maps, and inconsistent parentage
reject the provider result atomically.

## Introspection and invalidation

Add `SchemaDepth::Graph { options }` to the existing `Driver::schema` request
and advertise `driver.schema.graph@1`. This is additive to the protocol and
Driver RPC; the Rust trait signature remains unchanged under ADR-017. Bundled
drivers perform bounded bulk catalog queries, not one request per object.
External providers must declare the capability and pass hostile graph
validation. Providers without it remain usable for shallow/deep schema but
report graph-dependent operations as unsupported.

Introspection is staged:

1. fetch identity, namespaces, object headers, and containment;
2. fetch columns, constraints, indexes, triggers, routines, and types in
   bounded batches;
3. fetch engine-proven dependencies;
4. optionally parse definitions for references the catalog cannot prove;
5. normalize, validate, filter, digest, and publish the revision atomically.

A deadline or permission error publishes a validated partial graph only when
the response says exactly which stage/schema/kind is incomplete. Catastrophic
provider failure leaves the previous graph available as `stale`; it does not
replace it with an empty graph. Initial failure with no prior graph is an
error.

Existing PostgreSQL DDL notification, SQL Server `modify_date` polling, and
the TTL ceiling advance a per-spec invalidation epoch and evict shallow, deep,
search, graph, semantic-binding, and diagram projections together. Every
Sift-applied DDL invalidates synchronously after the last attempted statement,
including partial failure. In-flight builds publish only if their starting
epoch still matches. Concurrent misses coalesce. Entry count, node/edge count,
definition bytes, build concurrency, and retained bytes are hard-bounded and
tenant-accounted where the cache is attributable.

## Durable schema snapshots

A `CatalogSnapshotId` is a server-issued UUID stored in Sift metadata under a
tenant and connection-profile namespace. A snapshot contains the normalized,
visibility-filtered graph, capture provenance, source revision/digest,
description, creator, timestamps, coverage, and format version. It contains no
connection secrets or raw spec. Snapshot creation requires a complete graph by
default; callers may explicitly accept partial coverage, which is permanently
recorded and propagated into every diff.

Snapshots are immutable. Create/list/get/delete use optimistic metadata
revisions and tenant limits for count and retained bytes. The state-backup
archive includes them as ordinary non-secret metadata. A future Phase L DDL
source may implement the same `CatalogSource` interface without changing diff
semantics.

## Normalized diff

Diff compares any two authorized `CatalogSource`s: live revision or durable
snapshot. The result is immutable and bounded:

```text
SchemaDiff {
  from, to, coverage, changes, rename_suggestions, warnings
}
SchemaChange {
  id, kind, object_before?, object_after?, field_changes,
  dependencies, risk, reversibility
}
```

Changes include create/drop/rename/move/alter for supported nodes plus typed
column, constraint, index, trigger, routine, and type changes. Ordering is a
stable topological order over explicit prerequisites. Strongly connected
components are emitted as named groups with an engine-specific strategy or an
unsupported warning; arbitrary order is forbidden.

Comparison normalizes irrelevant engine noise (catalog ordering, generated
constraint names when proven equivalent, formatting-only definition changes)
while retaining semantic differences. Partial/inaccessible coverage suppresses
definitive drops and emits `unknown` changes/warnings. Rename heuristics never
silently replace drop+create; the caller supplies accepted `RenameMapping`s and
the server revalidates them for one-to-one kind/type compatibility.

Every change has a risk: `safe`, `locking`, `data_rewrite`, `data_loss`,
`privilege`, or `unknown`. Risk is conservative and deny-wins. Drops, narrowing
or incompatible type changes, non-null additions without a safe population
strategy, destructive constraint changes, and unproven partial-state actions
require explicit acknowledgement. Reversibility describes whether generated
rollback is exact, lossy, or unavailable; it is never inferred from having a
reverse SQL string.

## Migration preview and apply

Preview accepts a diff plus selected change ids, accepted rename mappings,
engine-aware options, and the expected live catalog revision. It returns a
short-lived opaque `MigrationPlanId`, ordered transactional groups, parameter-
free DDL statements, warnings, required acknowledgements, estimated lock/data
effects when knowable, rollback guidance, and a digest over every executable
field. SQL is generated only by the bundled engine renderer or the admitted
dialect/provider contribution and is validated against selected changes.

PostgreSQL groups transactional DDL where supported and isolates known
non-transactional statements. SQL Server groups only statements proven safe
for the active engine/version and reports implicit-commit or online-operation
limits. Preview never claims whole-plan atomicity when an engine cannot provide
it.

Apply requires editor-or-higher authorization intersected with profile policy,
an unexpired plan, exact plan digest, exact connection/profile/policy and live
catalog revision, and acknowledgements for every destructive/unknown risk.
The server takes a per-connection migration mutex, rechecks preconditions,
executes one bounded statement at a time through the normal isolated driver
path, and records group/statement outcomes. A stale revision fails before the
first statement. Cancellation stops before the next safe boundary; it cannot
promise rollback across non-transactional groups.

The apply response is a durable `MigrationRun`: start/end time, actor/scope,
plan digest, redacted statement fingerprints, group outcomes, terminal state
(`applied`, `rolled_back`, `partial`, `canceled`, `failed`), and resulting
catalog revision when refresh succeeds. SQL text and database error fragments
are not written to audit. Partial runs are never reported as success and
always force cache invalidation and a fresh graph before another apply.

## Public surfaces and operations

Connection-scoped routes provide graph get/refresh, live snapshot creation,
diff, migration preview, apply, cancel, and run retrieval. Tenant-scoped routes
list/get/delete durable snapshots. Diagram and semantic APIs consume graph ids
and revisions rather than duplicating catalog DTOs.

Every route has a typed `Operation`/`OperationKind`, authorization and rate
classification, generated OpenAPI, and reference SDK method. Graph reads and
preview are non-destructive; snapshot deletion and migration apply are
destructive. Audit stores ids, revisions, counts, risk classes, fingerprints,
and outcomes only. User descriptions are bounded and sanitized.

Stable errors include catalog unavailable/stale revision, partial catalog not
accepted, diff limit exceeded, migration plan expired/tampered, destructive
acknowledgement required, dependency cycle unsupported, and migration partial
failure. Existing rate, tenant-resource, timeout, cancellation, policy
revision, and unsupported-provider codes remain authoritative where applicable.

## Graduation evidence

ADR-033 graduates only with:

- PostgreSQL and SQL Server fixtures covering every node/edge/change family,
  overloads, quoted/UTF-8 identifiers, cycles, partial permissions, and large
  schemas;
- deterministic graph/diff digests and ordering under randomized catalog row
  order;
- create/apply/re-introspect round trips plus explicit destructive, stale-plan,
  cancellation, transactional, non-transactional, and partial-failure matrices;
- cross-tenant/policy filtering and redaction tests proving hidden names and
  SQL never enter results, caches, audit, traces, or errors;
- hostile external-provider validation and invalidation-race tests; and
- published latency/retained-memory budgets with public Operation/OpenAPI/SDK
  parity.
