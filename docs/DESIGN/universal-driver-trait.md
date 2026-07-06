# Universal Driver Trait — Design Note

> **Status:** proposal, not yet an ADR. Feeds **ADR-017** (driver trait shape,
> Phase A) and **ADR-022** (driver extensibility, Phase I). Written against the
> code as of `docs/PLANS/server-build-list-v2.md`.
>
> Companion to `docs/DECISIONS.md` and `crates/driver-api/src/lib.rs`.

## Context

The current `Driver` trait is engine-neutral at the *registry* level
(`HashMap<Engine, Arc<dyn Driver>>`, `registry.rs:14`) but **SQL/relational-
shaped at the verb level**:

- `schema()` returns `CatalogTree → SchemaTree → ObjectInfo` carrying
  columns/indexes/constraints/triggers (`protocol/src/schema.rs:205-236`) — a
  relational catalog forced on every engine.
- `execute(ExecuteRequest)` carries `sql: String` (`protocol/src/result.rs:76`)
  — assumes a SQL dialect. Mongo pipelines and Redis command arrays would have
  to abuse the SQL slot.
- `begin`/`commit`/`rollback` are core verbs (`driver-api/src/lib.rs:157-161`)
  — assumes OLTP transactions. ClickHouse and Redis don't have them.
- `Engine` and `EngineConnectionSpec` are closed enums (`protocol/src/engine.rs:7`,
  `connection.rs:37`) — adding engines means editing both.
- Escape hatches (`Value::Engine` `value.rs:40`, `ObjectKind::Other`, `IndexKind::Other`
  `schema.rs:107,136`) make non-relational engines *possible* but second-class:
  they'd render through a relational UI onto a non-relational store.

This is fine for the 8 relational engines (MySQL, MariaDB, SQLite, Oracle,
CockroachDB, ClickHouse). It fights the document/KV/graph engines in the vision
(MongoDB, Redis, Neo4j).

## Goal

One trait surface that gives **every** engine class first-class treatment —
relational, columnar, document, key-value, graph, and whatever comes next —
without re-litigating the trait shape each time, and without forcing non-
relational engines through a SQL funnel.

## Non-goals

- Shipping engines beyond PG + MSSQL (that's Phase I+ work). This note only
  locks the *shape* they'd slot into.
- Replacing the `PgExt`/`MssqlExt` pattern for genuinely engine-only ops.
- Changing driver isolation (ADR-013 candidate). Capability discovery is
  orthogonal to `tokio::spawn` + `catch_unwind` + cancel.

## Design principles

1. **Capability discovery, not assumption.** The server asks what an engine can
   do; it never assumes.
2. **Mandatory minimum, optional everything else.** Only "connect" and "run
   something" are universal. Catalog shape, transactions, notifications, bulk
   IO — all optional.
3. **Neutral introspection, flavor-tagged payloads.** One catalog model with
   per-flavor detail, not N parallel models.
4. **Additive-only protocol changes.** New engines = new enum variants + new
   capability flags, never rewrites. Existing clients keep working.
5. **Engine-only ops stay in ext traits.** Capability traits are for *cross-
   cutting* concepts (notifications, bulk IO); genuinely unique ops (PG
   advisory locks) stay in `PgExt`.

## Proposed architecture

### 1. Capability discovery as the spine

Replace the implicit "every Driver implements every verb" contract with an
explicit capability set. `Driver` shrinks to the universal minimum; everything
else moves to optional capability traits.

```rust
#[async_trait]
pub trait Driver: Send + Sync {
    fn engine(&self) -> Engine;
    fn capabilities(&self) -> Capabilities;          // cheap, sync, cached

    async fn open(&self, spec: &ConnectionSpec) -> Result<ConnHandle, DriverError>;
    async fn close(&self, c: ConnHandle) -> Result<(), DriverError>;
    async fn ping(&self, c: ConnHandle) -> Result<ServerInfo, DriverError>;

    // The one universal execution verb.
    async fn execute(&self, c: ConnHandle, req: ExecuteRequest)
        -> Result<ResultSetStream, DriverError>;

    // Cancel stays universal — every engine can abort an in-flight op.
    async fn cancel(&self, c: ConnHandle, cursor: CursorId) -> Result<(), DriverError>;
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema)]
pub struct Capabilities {
    pub catalog_flavor: CatalogFlavor,        // Relational | Document | KeyValue | Graph | Generic
    pub query_dialects: QueryDialectSet,      // Sql | Pipeline | Commands | Cypher | ...
    pub transactions: bool,
    pub savepoints: bool,
    pub notifications: bool,
    pub bulk_import: bool,
    pub bulk_export: bool,
    pub server_side_cursors: bool,
    pub advisory_locks: bool,
    pub streaming_ingest: bool,
    // additive — unknown flags ignored by older clients
}

#[serde(rename_all = "snake_case")]
pub enum CatalogFlavor {
    Relational,   // PG, MSSQL, MySQL, SQLite, Oracle, CockroachDB, ClickHouse
    Document,     // MongoDB
    KeyValue,     // Redis
    Graph,        // Neo4j (future)
    Generic,      // last-resort tree of named nodes
}
```

`Capabilities` is **the contract the server reads first**. Every optional
concern below is gated on a flag; absent flag → `Code::UnsupportedForEngine`
with no driver call.

### 2. Optional concerns become capability traits

Each optional behavior becomes its own trait. The driver implements only the
ones its engine supports; `Driver` gains typed downcasts mirroring today's
`as_pg()`/`as_mssql()` (`driver-api/src/lib.rs:181-189`):

```rust
#[async_trait]
pub trait Transactional: Send + Sync {
    async fn begin(&self, c: ConnHandle, mode: TxMode) -> Result<TxHandle, DriverError>;
    async fn commit(&self, t: TxHandle) -> Result<(), DriverError>;
    async fn rollback(&self, t: TxHandle) -> Result<(), DriverError>;
}

#[async_trait]
pub trait Catalog: Send + Sync {
    async fn introspect(&self, c: ConnHandle, scope: Scope)
        -> Result<CatalogSnapshot, DriverError>;
}

#[async_trait]
pub trait NotificationSource: Send + Sync {
    async fn subscribe(&self, c: ConnHandle, channels: Vec<String>)
        -> Result<NotificationStream, DriverError>;
    async fn unsubscribe(&self, c: ConnHandle, channels: Vec<String>) -> Result<(), DriverError>;
}

#[async_trait]
pub trait BulkIO: Send + Sync {
    async fn import(&self, c: ConnHandle, op: ImportOp) -> Result<ImportResult, DriverError>;
    async fn export(&self, c: ConnHandle, op: ExportOp) -> Result<ExportStream, DriverError>;
}

// On Driver — default None, same pattern as today's as_pg()/as_mssql():
fn as_transactional(&self) -> Option<&dyn Transactional>       { None }
fn as_catalog(&self)       -> Option<&dyn Catalog>             { None }
fn as_notification(&self)  -> Option<&dyn NotificationSource>  { None }
fn as_bulk(&self)          -> Option<&dyn BulkIO>              { None }
fn as_pg(&self)            -> Option<&dyn PgExt>               { None }  // stays
fn as_mssql(&self)         -> Option<&dyn MssqlExt>            { None }  // stays
fn as_mongo(&self)         -> Option<&dyn MongoExt>            { None }  // new
```

Wrong-engine → `None` → server translates to `Code::UnsupportedForEngine`.
Same failure semantics as today's `ext_missing()` helper (`session.rs:725`),
just more traits.

**Why split, not one fat trait:** a fat trait forces every driver to stub
methods it can't implement — landmines of `unimplemented!()`. Splitting lets
the type system express "this engine has no concept of transactions" cleanly.
It also matches how the server already dispatches (the `Engine::Postgres => …
Engine::SqlServer => …` arms at `session.rs:440-520`): those arms become
capability checks instead of engine matches.

### 3. Neutral catalog model

Replace `SchemaSnapshot` (relational-only, `schema.rs:184-203`) with a flavor-
tagged tree. One model, per-flavor payloads — and `RelationalDetail` *is*
today's `ObjectInfo` payload, so PG/MSSQL don't lose anything:

```rust
pub struct CatalogSnapshot {
    pub flavor: CatalogFlavor,
    pub root: Vec<CatalogNode>,
    pub fetched_at: DateTime<Utc>,
    pub scope: Scope,
    pub incomplete: bool,
}

pub struct CatalogNode {
    pub id: String,            // stable path id for lazy deep-fetch
    pub name: String,
    pub kind: NodeKind,         // Database | Schema | Collection | Label | Prefix | Table | View | ...
    pub children: Vec<CatalogNode>,
    pub detail: NodeDetail,     // flavor-specific; Absent for shallow nodes
}

#[serde(tag = "flavor", rename_all = "snake_case")]
pub enum NodeDetail {
    Absent,
    Relational(RelationalDetail),   // columns, indexes, constraints, triggers (today's ObjectInfo)
    Document(DocumentDetail),       // validator / JSON schema, indexes, shard key
    KeyValue(KeyValueDetail),       // key pattern, sample types, TTL
    Graph(GraphDetail),             // property keys, indexes, constraints
    Generic(serde_json::Value),     // engine-native escape hatch
}
```

- **Relational engines** populate `RelationalDetail` — byte-equivalent to today.
- **MongoDB** returns `Database → Collection` nodes with `DocumentDetail`; no
  fake schema layer forced.
- **Redis** returns synthetic nodes for known key-prefix patterns (or a flat
  `Generic` tree when scan is disabled); no fake tables.
- **Neo4j** returns node-label / relationship-type / property-key nodes.

### 4. Neutral query model

`ExecuteRequest.sql: String` (`result.rs:76-77`) becomes a tagged union so
non-SQL engines stop abusing the SQL slot:

```rust
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Query {
    Sql { text: String, parameters: Vec<Value> },         // PG, MSSQL, MySQL, SQLite, Oracle, ...
    Pipeline { body: serde_json::Value },                 // Mongo aggregation / runCommand
    Commands { ops: Vec<String> },                        // Redis command arrays
    Cypher { text: String },                              // Neo4j
    Native { dialect: String, body: serde_json::Value },  // escape for the next thing
}
```

`ExecuteRequest` carries `Query` + execution options (timeout, fetch policy).
The HTTP/WS execute paths become engine-agnostic; the client picks a `Query`
variant per engine.

### 5. Result model: tabular stays, non-tabular added

`ResultSetStream`/`Page` is already nearly universal — most engines' results
flatten to rows. Add one `Page` variant for the cases that don't:

```rust
pub enum Page {
    NextResult { columns: Vec<ColumnMetadata> },
    Rows(Vec<Row>),
    Done { affected_rows: Option<u64> },
    NonTabular(serde_json::Value),   // Redis status replies, Mongo command acks, etc.
    Error(DriverError),
}
```

Redis `SET k v` → `Page::NonTabular("OK")` + `Page::Done`. Mongo `aggregate`
→ standard `Rows`. No engine forced through a grid.

### 6. Transactions: capability, not core

`begin`/`commit`/`rollback` move out of `Driver` into `Transactional`.
Engines without OLTP tx (ClickHouse, Redis-for-most-commands) don't implement
it; `capabilities().transactions == false`; the routes return
`UnsupportedForEngine`. PG/MSSQL/Mongo(4.0+)/MariaDB implement it.

### 7. Engine-specific ext traits remain

Cross-cutting concept → capability trait. Engine-only op → ext trait. Drawn
per-op, not per-engine:

- **PG `LISTEN`** migrates to `NotificationSource` (Redis Pub/Sub and Mongo
  change streams share the concept). **PG `advisory_lock`** stays in `PgExt`
  (no cross-engine analogue).
- **PG `COPY`** migrates to `BulkIO` (MSSQL `BULK INSERT` shares the concept).
- **MSSQL `USE <db>`/MARS** stay in `MssqlExt`.
- **Mongo** index/validator creation → `MongoExt`.
- **Redis** admin commands (`CLONE`/`CONFIG`/`DEBUG`) → `RedisExt`.

## Engine coverage matrix

| Engine | Catalog flavor | Query dialect | Tx | Notifications | Bulk | Notes |
|---|---|---|---|---|---|---|
| PostgreSQL | Relational | Sql | yes | yes (LISTEN) | yes (COPY) | ext: advisory locks |
| SQL Server | Relational | Sql | yes | — | yes (BULK INSERT) | ext: USE, MARS |
| MySQL / MariaDB | Relational | Sql | yes | — | partial | — |
| SQLite | Relational | Sql | yes (within-db) | — | — | single catalog |
| Oracle | Relational | Sql | yes | — | yes | ext likely |
| CockroachDB | Relational | Sql | yes | — | — | CRDB-as-PG wire |
| ClickHouse | Relational | Sql | no | — | yes | weak indexes via `Other` |
| MongoDB | Document | Pipeline | yes (4.0+) | yes (change streams) | yes | ext: validators |
| Redis | KeyValue | Commands | partial (MULTI) | yes (Pub/Sub) | partial | no real catalog |
| Neo4j (future) | Graph | Cypher | yes | — | yes | ext likely |

Every cell maps to a `Capabilities` flag the server reads before routing.

## Protocol impact (ADR-003, ADR-016 candidate)

Mostly additive; two breaking renames gated on a version bump:

- **`Engine` enum grows variants** (`Mongo`, `Redis`, `Neo4j`, ...). Closed enum
  + serde tags → each addition is a protocol minor bump. Clients MUST treat
  unknown variants as "engine not supported by this client," never as a parse
  error. This becomes a hardening rule of ADR-016.
- **`EngineConnectionSpec` grows variants** analogously — or replace with
  `engine_options: serde_json::Value` validated per-engine by the driver.
  *Recommendation: typed variants for v1 engines, escape hatch for experimental
  ones.*
- **`SchemaSnapshot` → `CatalogSnapshot`** is a **breaking rename** → v2. Old
  name aliased for one deprecation window (ADR-016 rule).
- **`ExecuteRequest.sql: String` → `ExecuteRequest.query: Query`** is a
  **breaking shape change** → v2.
- **`Page::NonTabular`** is additive (clients ignore unknown enum variants
  once hardened; else v2).

**Recommendation: bundle the breaking changes into `PROTOCOL_VERSION = 2` as a
single coordinated bump**, shipped with the first non-relational driver. Until
then v1 stays and PG/MSSQL keep working untouched.

## Migration path — PG and MSSQL do not regress

1. **No behavior change in driver crates.** `PgDriver`/`MssqlDriver` gain
   `capabilities()` impls returning the right flags. Existing method bodies
   move from `Driver::schema`/`begin`/etc. to `Catalog::introspect`/
   `Transactional::begin` with identical bodies. `as_catalog()`/
   `as_transactional()` return `Some(self)`.
2. **Server dispatch rewrites from match-on-engine to check-capability-then-
   call.** Today's `session.rs:440-520` arms become
   `if let Some(t) = driver.as_transactional() { t.savepoint(...) } else { Err(unsupported) }`.
   The `ext_missing` helper at `session.rs:725` already encodes this pattern.
3. **`PgExt`/`MssqlExt` stay during migration**; promote LISTEN/COPY to
   capability traits only when a second engine wants the same concept.
4. **Live tests stay green** — the reorg is mechanical; behavior preserved.

## Driver isolation (ADR-013 candidate) preserved

Capability discovery changes nothing about isolation:

- Every capability method is still `&self` + boxed future → `Arc<dyn Trait>`
  clones cheaply across tasks.
- The server still wraps every call in `tokio::spawn` + `catch_unwind` +
  cancel token (build-list Phase A/B). Capability traits inherit the same
  isolation contract.
- A wedged Mongo driver cannot freeze the server for the same reason a wedged
  PG driver cannot: the spawn boundary lives in the server, not the driver.

## Open questions

1. **Split capability traits vs one fat trait with `UnsupportedForEngine`
   everywhere.** Split is cleaner type-system-wise; fat is less boilerplate.
   Decide in ADR-017 before locking. *This note recommends split.*
2. **`EngineConnectionSpec`: typed variants or `serde_json::Value`?** Typed is
   safer; `Value` is friction-free for experimental engines. *Recommendation:
   hybrid.*
3. **Multi-model engines** (PG with JSONB/foreign tables, Mongo with Atlas
   SQL). Probably per-`CatalogNode` flavor, not per-engine — PG already mixes
   kinds today.
4. **Graph as first-class flavor or `Generic`?** Neo4j is the only likely
   candidate; promoting now is cheap, deferring is expensive.
5. **Does `Capabilities` need a wire form for the client?** Yes — expose via
   `GET /v1/connections/:id/capabilities` so the UI can render engine-
   appropriate affordances (hide the SQL editor for Redis; show a command
   composer instead).

## Relationship to existing ADRs and the build-list

- **Feeds ADR-017 (Phase A, unwritten):** the trait shape lock should be this
  shape, not the current one. Locking the current shape and reworking it later
  is a wasted protocol bump.
- **Feeds ADR-022 (Phase I, unwritten):** the RPC driver protocol carries
  capability traits as RPC interfaces; third-party drivers advertise
  capabilities on handshake.
- **Replaces** the implicit "every engine implements every verb" assumption in
  `Driver` (`driver-api/src/lib.rs:135-190`) and the engine match arms in
  `session.rs:440-520`.
- **Does not touch** ADR-001/002/003/004/005/006/007/008/009/010 — server-is-
  product, pure-serde protocol, rooms, secrets, audit, UI deferral all stand.

## Recommendation

Land this as **ADR-017** before locking the trait. The cost is one coordinated
`PROTOCOL_VERSION = 2` bump (unavoidable whenever non-relational engines
arrive) plus a mechanical reorg of PG/MSSQL method bodies. The payoff: adding
engine N+1 — relational or not — is *always* "implement the relevant capability
traits + register," never "revisit the trait."
