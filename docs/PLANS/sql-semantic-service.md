# Shared Dialect-Aware SQL Semantic Service

Status: **accepted for implementation (ADR-032).** This document is the
normative Phase K contract for SQL parsing and semantic features. It does not
select a particular parser library or reopen the Phase I extension trust
model.

## Goals

Sift will have one server-owned semantic document and one parse per revision,
shared by statement selection, diagnostics, completion, formatting, usages,
refactoring, quick fixes, and governed AI context. PostgreSQL and SQL Server
are the first-party conformance dialects. Thin clients never embed a second
authoritative parser, and feature implementations do not maintain private SQL
text or token caches.

The service must:

- tolerate incomplete, temporarily invalid editor text without discarding the
  valid prefix or suffix;
- preserve exact UTF-8 byte locations so every client applies the same edits;
- select a dialect from the connection's declared `DialectId`, never from a
  guessed provider name or the legacy `Engine` enum;
- isolate CPU-heavy or faulty dialect work from the async server runtime;
- make every public feature reachable through typed Operations, HTTP,
  generated OpenAPI, and the reference SDK; and
- remain useful with no optional extension installed by bundling
  `sift/postgresql` and `sift/tsql` packs.

Plan capture consumes the statement identity and normalized semantic context
defined here, but its database execution and retention lifecycle are a
separate Phase K design slice. Catalog identity and schema diff remain
ADR-033.

## Ownership and crate boundary

Add a UI-free `sift-semantic` crate. It owns document state, normalized source
locations, statement indexing, cache keys, resource accounting, orchestration,
and the dialect-pack interface. `sift-protocol` contains only the serde and
schemars wire DTOs. `sift-server` owns authorization, routing, lifecycle,
timeouts, cancellation, audit, schema-cache views, and pack registration.

A dialect pack owns dialect grammar and dialect-specific behavior:

- lossless lexing and error-recovering parsing;
- statement and syntax-node classification;
- name binding and dialect-specific diagnostics;
- formatting rules, completion enrichments, quick fixes, and refactor rules;
  and
- rendering/quoting rules for edits it proposes.

Core owns the portable result contracts and validates every range, edit,
diagnostic, symbol, and size returned by a pack. A pack cannot own routes,
authentication, document identity, caches, schema access, audit records, or
apply an edit. It receives a bounded, visibility-filtered catalog view rather
than a driver or metadata-store handle. Pack failure cannot change connection
or document state.

The internal contract is capability-negotiated rather than a monolithic trait:

```text
DialectPackDescriptor {
  dialect_id, contract_version, pack_version,
  capabilities, limits
}

parse(source, parse_options, cancellation) -> ParsedArtifact + ParseSummary
format(parsed_artifact, range, options, cancellation) -> TextEdits
diagnose(parsed_artifact, catalog_view, cancellation) -> Diagnostics
complete(parsed_artifact, catalog_view, cursor, cancellation) -> Candidates
find_usages(parsed_artifact, catalog_view, target, cancellation) -> Usages
prepare_refactor(parsed_artifact, catalog_view, request, cancellation) -> WorkspaceEdit
quick_fix(parsed_artifact, diagnostic_id, cancellation) -> WorkspaceEdit
```

`ParsedArtifact` is pack-private and revision-bound. Core retains only its
handle plus a normalized `ParseSummary`: lossless token spans, top-level
statement spans/kinds, recovery regions, pack diagnostics, and portable symbol
facts. In-process built-ins may hold a Rust object; an out-of-process pack uses
an opaque handle scoped to its supervised process generation. Handles never
cross the public protocol and are invalid after a pack restart. Core reparses
from retained source when a handle is lost.

Phase I's `dialect_pack` manifest contribution remains only an identity until
this contract is implemented. Activation additionally requires
`sql.semantic@1`, an exact supported semantic contract range, declared
capabilities and hard limits, and the normal signed-package, grant,
supervision, and tenant-allowlist checks. External packs run out of process;
only bundled first-party packs may run in process. There is exactly one active
pack per `DialectId`; conflicting ownership or ambiguous priority fails
activation rather than producing nondeterministic parsing.

## Parsed-document identity and revisions

`SemanticDocumentId` is an opaque, server-issued UUID. It is scoped to the
authenticated tenant, principal, session, connection, and the connection's
current dialect. It is not a durable query/document id and cannot be used to
read CRDT, workspace, or query-history state.

Creating a semantic document supplies UTF-8 text and an optional source link:

```text
SemanticSource = scratch
               | room_document { room_id, document_id, version_fingerprint }
               | workspace_document { workspace_id, document_id, revision }
```

The workspace variant is reserved until ADR-034. A source link is provenance,
not permission: the route independently authorizes the linked resource. Room
text is materialized by the existing per-document Loro actor; semantic state
never stores CRDT operations or treats a semantic revision as a Loro version.

Each accepted full-text replacement advances a server-issued `u64`
`SemanticRevision`, starting at 1. Updates carry `base_revision`; a mismatch
returns `409 revision_conflict` with the current revision and does not parse or
change text. Identical retry requests against the immediately preceding base
revision are idempotent when their content digest matches. Revision overflow
closes the semantic document and requires recreation.

The tuple `(semantic_document_id, revision, dialect_id, pack_version)` names a
parsed document. Source bytes are immutable within that tuple. Catalog state
does not change parse identity: catalog-dependent results additionally carry a
`catalog_revision` and their cache key includes it. Formatting options and
feature settings are included through a stable settings fingerprint.

Semantic documents are process-local, non-durable editor accelerators. They
close explicitly, when their session/connection closes, when authorization or
the connection policy is revoked, or after 30 minutes without access. A server
restart or eviction returns `semantic_document_not_found`; clients recreate
from their authoritative text. Room and future workspace documents remain
durable through their own stores.

## Text, offsets, and recovery

All public ranges are half-open UTF-8 byte offsets `[start, end)` into the
exact source for the requested revision. Both endpoints must be at code-point
boundaries and satisfy `start <= end <= source.len()`. Invalid offsets return
`400 invalid_text_range`; they are never silently clamped. Line and UTF-16
coordinates are presentation concerns for clients. Responses echo the
revision, and edits include the expected source digest so they cannot be
applied to another revision accidentally.

The parser is lossless: comments, whitespace, quoted identifiers, delimiters,
and invalid tokens retain spans. Recovery creates explicit error/missing nodes,
continues at dialect-specific synchronization points, and must preserve a
monotonic, non-overlapping top-level statement index over all recoverable
input. A syntax error is a successful parse with diagnostics, not a failed
request. Catastrophic pack failure, timeout, cancellation, invalid UTF-8 at an
extension boundary, or a hard resource limit is a request error and never
publishes a partial revision.

Diagnostics use stable ids only within one document revision and contain:
`id`, `severity`, `code`, `message`, primary range, bounded related ranges,
source (`parser`, `binder`, `lint`, or a contribution id), and available
quick-fix ids. Parser diagnostics require no catalog. Binding and lint
diagnostics declare the `catalog_revision` used and may be marked `incomplete`
when introspection is partial. Missing/inaccessible catalog objects are not
reported as definitive errors when the catalog view is incomplete.

## Feature contracts

Every feature request contains `document_id` and `revision`. The service
rejects a stale revision; it never quietly answers from the latest text.

### Statement selection

`SelectStatementRequest` contains `cursor` and an optional non-empty
`selection`. A supplied selection wins after boundary validation and is
returned unchanged with every intersecting top-level statement id. With an
empty selection, selection is deterministic:

1. choose the top-level statement whose trimmed span contains the cursor;
2. on a semicolon or whitespace boundary, choose the following statement;
3. if none follows, choose the preceding statement; and
4. for an empty/comment-only document, return no statement.

The response includes the full statement range (including its delimiter when
present), executable range (outer trivia and delimiter excluded), normalized
statement kind, ordinal, whether recovery touched it, and a revision-scoped
`StatementId`. Selection does not claim that an error-recovered statement is
safe to execute; callers must make that choice explicitly.

### Completion

Completion contains cursor, optional trigger character/kind, and a limit. It
returns a validated replacement range, ranked candidates, context, revision,
catalog revision, and an `is_incomplete` flag. Candidates retain the existing
label/insert/kind/detail shape and add stable sort text, optional documentation,
and optional additional non-overlapping edits. The server clamps results to
200. Keywords, aliases, CTEs, temporary/document-created objects, visible
catalog objects, and pack enrichments all rank through one pipeline.

The existing `POST .../complete` route remains during protocol v1 as a
compatibility adapter: it creates a request-scoped semantic document and uses
the same parse/model path. `sift-completion`'s private token cache and context
scanner are removed after corpus parity; its dictionary/fuzzy ranker may move
into `sift-semantic`. No completion code may tokenize SQL independently after
migration.

### Formatting

Formatting accepts the whole document or a validated range plus a typed,
bounded `FormatOptions` object. It returns sorted, non-overlapping `TextEdit`s,
never mutates source, and is idempotent for a fixed pack version and settings.
Range formatting may expand only to enclosing syntax nodes/statements and
reports the actual formatted range. Error regions are preserved verbatim
unless a declared recovery-safe rule changes only surrounding trivia.

### Usages, refactoring, and quick fixes

Symbols use revision-scoped `SymbolId`s plus an optional stable catalog object
identity from ADR-033. `FindUsages` returns bounded, paged locations with a
kind (`definition`, `read`, `write`, `call`, `type_reference`) and completeness
metadata. Before ADR-034, cross-document usages are limited to live catalog
objects and the current semantic document; the response says so explicitly.

Refactors have a prepare-only semantic contract. `PrepareRefactorRequest` is a
tagged operation (initially `rename_symbol` and `qualify_name`) and returns a
`WorkspaceEdit` grouped by document, with expected revisions/digests, warnings,
and conflict policy. Quick fixes return the same shape. Semantic routes never
write CRDT or workspace text. A client applies room edits through the existing
document-update contract; future workspace apply belongs to ADR-034. All edits
must be sorted, non-overlapping, within authorized documents, and independently
validated by core.

## HTTP, OpenAPI, and Operations

The connection selects the dialect and catalog visibility, so the v1 routes
are connection-scoped:

| Method and path suffix under `/v1/sessions/{session}/connections/{connection}` | Operation kind | Result |
| --- | --- | --- |
| `POST /semantic-documents` | `OpenSemanticDocument` | `SemanticDocumentState` |
| `PUT /semantic-documents/{document}` | `UpdateSemanticDocument` | `SemanticDocumentState` |
| `DELETE /semantic-documents/{document}` | `CloseSemanticDocument` | empty |
| `POST /semantic-documents/{document}/statements/select` | `SelectStatement` | `StatementSelection` |
| `POST /semantic-documents/{document}/diagnostics` | `DiagnoseSql` | `DiagnosticsResponse` |
| `POST /semantic-documents/{document}/format` | `FormatSql` | `WorkspaceEdit` |
| `POST /semantic-documents/{document}/complete` | existing `Complete` | `CompletionResponseV2` |
| `POST /semantic-documents/{document}/usages` | `FindSqlUsages` | `UsagePage` |
| `POST /semantic-documents/{document}/refactors/prepare` | `PrepareSqlRefactor` | `WorkspaceEdit` |
| `POST /semantic-documents/{document}/quick-fixes/{fix}` | `SqlQuickFix` | `WorkspaceEdit` |

The common wire envelopes are fixed as follows (field types named here become
ordinary tagged serde DTOs, not free-form JSON):

```text
CreateSemanticDocumentRequest { text, source? }
UpdateSemanticDocumentRequest { base_revision, text }
SemanticFeatureRequest { revision, feature_fields... }

SemanticDocumentState {
  document_id, revision, source_digest, dialect_id, pack_version,
  parse_status, syntax_diagnostics
}
TextEdit { range, new_text }
DocumentEdit { document_id, expected_revision, source_digest, edits }
WorkspaceEdit { documents, warnings, is_complete }
```

Create returns `201`; an accepted update returns `200`; close returns `204`.
Feature calls are read/prepare operations and return `200` even when SQL has
syntax diagnostics. Update is atomic: the new revision is published only when
the lossless parse and normalized statement index pass core validation. Usage
pagination uses the repository's opaque keyset `Page` contract, not numeric
offsets. Requests reject unknown fields where the existing public protocol
does so, and additive response fields follow ADR-016 compatibility rules.

Document create/update responses contain revision, source digest, dialect id,
pack version, parse status, and syntax diagnostics so the first implementation
slice needs no redundant diagnostics call. Catalog diagnostics remain an
explicit request because they may await progressive schema hydration.

All request/response types live in `sift-protocol`, derive serde and schemars,
and are generated into OpenAPI from the live router. The SDK owns every stable
operation id. `Operation`, `OperationKind::ALL`, capability evaluation,
authorization, rate class, and destructive classification are extended
exhaustively. Semantic edit generation is non-destructive; a later apply
operation retains its own write/destructive classification.

New stable error codes are `SemanticDocumentNotFound`, `SemanticRevisionConflict`,
`InvalidTextRange`, `DialectUnavailable`, `SemanticLimitExceeded`, and
`SemanticTimedOut`. Cancellation uses the existing cancellation mapping and
never returns a successful truncated result. Unsupported pack capabilities use
`UnsupportedForEngine` and are exposed through capability discovery.

## Audit, privacy, and redaction

SQL text, selected text, identifier names, diagnostic messages containing
source fragments, completion prefixes, formatting output, symbol names, and
text edits never enter operation logs, durable audit, metrics labels, tracing
fields, or error logs. Semantic operations record only actor/scope ids,
document id, revision, dialect/pack ids, feature, source byte count, result
counts, latency bucket, completion/error code, and a normalized SQL
fingerprint when useful. Source digests are keyed fingerprints for correlation,
not raw SHA-256 values suitable for guessing short statements.

The current `Operation::Complete { request }` must be changed before migration:
the sanitized operation retains a fingerprint, cursor, and limit but no SQL.
Redaction tests cover every new Operation variant and both success/failure
paths. Pack stderr/stdout and structured logs pass through Phase I rate limits
and redaction; untrusted diagnostic text is response data, not a log field.

Catalog views are filtered by the central authorization evaluator and profile
schema policy before reaching the service or pack. Cache keys include tenant,
principal visibility, profile policy revision, and catalog revision; semantic
or catalog results are never shared across those boundaries.

## Caching, cancellation, and resource bounds

Parsing is CPU work. In-process packs run on a bounded blocking pool behind a
dedicated semaphore, never on an Axum/Tokio executor thread. External packs use
the Phase I supervisor with bounded frames, deadlines, cancellation, crash
recovery, and process resource limits. Queue admission happens before source
is cloned into work.

Defaults (operator-configurable only downward for hosted tenants) are:

| Resource | Limit |
| --- | ---: |
| source text per semantic document | 2 MiB |
| live semantic documents per session | 64 |
| retained semantic source + artifacts per tenant | 64 MiB |
| tokens / normalized syntax nodes per revision | 250,000 / 500,000 |
| diagnostics / quick fixes per response | 500 / 200 |
| text edits / serialized response | 10,000 / 4 MiB |
| completion candidates | 200 |
| concurrent semantic jobs per tenant | 4 |

The parsed cache is weighted by retained source plus a pack-reported artifact
weight. It retains at most the current and immediately previous revision per
live document and evicts globally by least-recent access within tenant scope.
Feature-result keys add catalog revision, settings fingerprint, pack version,
and feature arguments. Negative/error results are not cached except that an
unchanged successful parse includes its syntax diagnostics. Updating text,
pack activation/restart, policy revision, or catalog invalidation cancels and
invalidates affected derived work. Eviction never evicts durable CRDT text or
schema-cache truth.

Every job receives a cancellation token tied to request disconnect, explicit
client cancellation, document update/close, session/connection close,
authorization revocation, timeout, and server shutdown. Packs check at lexer
chunks and between statements/tree walks; conformance requires observable
cancellation within 10 ms or 4,096 tokens/nodes of CPU work. A stale job may
finish internally but its result is discarded unless document id, revision,
pack generation, policy revision, and catalog revision still match.

Interactive service-level objectives on release hardware, measured warm and
excluding schema refresh/database I/O, are:

| Operation and input | p95 budget | hard deadline |
| --- | ---: | ---: |
| cached statement selection | 5 ms | 50 ms |
| parse/update, 100 KiB | 50 ms | 500 ms |
| completion, 100 KiB + warm catalog | 75 ms | 500 ms |
| diagnostics, 100 KiB + warm catalog | 150 ms | 1 s |
| format, 100 KiB | 250 ms | 2 s |
| usages/refactor, one document + warm catalog | 500 ms | 3 s |

Requests exceeding a hard deadline fail with `SemanticTimedOut`; they do not
fall back to an independent heuristic parser. Cold catalog hydration returns
an explicitly incomplete semantic result or the existing progressive schema
state rather than hiding database I/O inside these budgets.

## Implementation sequence

1. Add the protocol identities, ranges, errors, Operations, dialect registry,
   and `sift-semantic` crate skeleton.
2. Land the first executable slice as one vertical feature: bounded parsed-
   document state, both bundled recovery parsers, the common statement index,
   create/update syntax diagnostics, statement selection, HTTP/OpenAPI/SDK,
   and audit sanitization. Prove revision conflicts, UTF-8 boundaries,
   cancellation, cache eviction, pack failure isolation, and redaction.
3. Migrate the existing completion endpoint and ranker onto the parsed
   document. Delete its tokenizer/context cache only after the old completion
   corpus passes through both compatibility and stateful routes.
4. Add formatting and catalog-backed binding diagnostics, then usages,
   refactors, and quick fixes. Integrate ADR-033 catalog identities when that
   decision graduates.
5. Add plan capture/retrieval against selected statement ids and normalized
   semantic context without merging database execution into the parser.

No later slice may add a second parser, token cache, range convention, or
document revision model.

## Two-engine graduation corpus

ADR-032 graduates only when a committed, reviewable corpus runs identically
through in-process APIs and public HTTP/SDK paths for `sift/postgresql` and
`sift/tsql`. Every fixture contains dialect id, source bytes, cursor/selections,
expected statement spans, normalized diagnostic codes/ranges, completion
contexts, and—when applicable—edits plus the expected post-edit source.

Both dialect sets must cover:

- empty, whitespace/comment-only, multi-statement, delimiter-boundary, and
  selected-range behavior;
- UTF-8 identifiers/comments, CRLF, byte-order marks, invalid byte offsets,
  quoted identifiers, escaped strings, nested comments where supported, and
  semicolons inside strings/comments;
- incomplete tokens, missing clauses/delimiters, unmatched parentheses,
  malformed middle statements followed by valid statements, and bounded
  error cascades;
- CTEs (including recursive), aliases, correlated/nested queries, joins,
  subqueries, DML with returning/output forms, DDL, transactions, routines,
  temporary objects, and dialect-specific batches;
- PostgreSQL dollar-quoted bodies, casts, operators, `RETURNING`, `ON
  CONFLICT`, arrays/JSON, and procedural-body recovery boundaries; and
- SQL Server brackets, `GO` batch separators, `TOP`, `OUTPUT`, `APPLY`, table
  variables, temp tables, `MERGE`, procedure bodies, and T-SQL variable scope.

The graduation matrix additionally includes golden formatting idempotence,
completion parity and richer scope cases, diagnostic/quick-fix validity,
rename collision and quoting cases, partial/inaccessible catalog behavior,
pack restart and malformed-output isolation, cancellation races, stale
revision/catalog rejection, cross-tenant cache isolation, worst-case nesting
and token floods, response caps, and the latency budgets above.

At least one fixture for each supported statement family must parse, select,
diagnose, complete, and format under each engine. Expected ranges are asserted
against raw bytes, not reconstructed line/column positions. Graduation also
requires fuzz/property tests proving that arbitrary UTF-8 input never panics,
all returned ranges are valid boundaries, edits do not overlap, resource use
remains bounded, and cancellation prevents stale publication.
