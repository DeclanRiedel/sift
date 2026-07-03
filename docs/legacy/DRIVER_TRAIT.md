# sift — Driver Trait Shape (Spec)

> Status: design for TODO.md item 0.1 and PHASE0.md step 4. **Output is this
> written spec, not code.** Code starts at PHASE0.md step 7 (Postgres) and
> step 15 (SQL Server). The trait API below is **mutable until step 16
> lands** (SQL Server end-to-end); after that it is locked and changes
> require justification + a protocol bump.
>
> Design target is SQL Server (the harder engine). Postgres collapses to the
> degenerate case. JDBC's lowest-common-denominator failure mode — where only
> the intersection of every engine's capabilities is expressible — is the
> explicit anti-pattern this shape avoids.

## Goals

1. One core trait every supported engine can implement, holding **only** the
   verbs every engine has.
2. Engine-specific capability exposed through per-engine **extension traits**,
   never silently no-op'd and never raising a "not supported" exception at
   runtime — capability is advertised up front via downcast.
3. No engine-specific types in the core trait's signatures.
4. The protocol crate holds **union types**: serde models that cover both
   engines' shapes, where engine-specific facets are optional, tagged, and
   carried verbatim (not flattened).
5. Object-safe core trait so the server can hold a `HashMap<Engine,
   Arc<dyn Driver>>` registry.
6. Driver isolation (ADR-013 candidate): a wedged driver cannot freeze the
   server — every method must be safe inside `tokio::spawn`, and cancel must
   be callable from a different task than execute.

## Non-goals

- A trait generic over connection type (`Driver<Conn>`). This breaks
  object-safety and the registry pattern; rejected.
- A `DatabaseMetaData`-style god-object. Replaced by a typed `SchemaSnapshot`
  returned from `schema()`.
- A `ResultSet` trait drivers implement. Replaced by protocol-owned
  `Page` / `Row` types that drivers convert into.
- Flattening all column types into a single lowest-common-denominator
  enumeration. Replaced by `TypeRef::Primitive` (IDE-native render vocab) and
  `TypeRef::Engine` (escape hatch carrying the engine-native name verbatim).

---

## Crate layout

```
       protocol (pure serde, no I/O — ADR-004)
       Connection / Catalog / Schema / Object / ColumnMetadata
       TypeRef / Row / Page / ResultSetStream / Error codes
            ▲                        ▲
            │ imported               │
   ┌────────┴───────┐         ┌──────┴────────┐
   │  driver-api    │<─impl───│  server       │
   │  Driver trait  │         │  registry:    │
   │  Engine enum   │         │  HashMap<Eng, │
   │  ConnHandle    │         │   Arc<dyn D>> │
   │  ext helpers   │         └───────────────┘
   └──────┬─────────┘
          │ impl
   ┌──────┴──────────┐  ┌───────────────────┐
   │ driver-postgres │  │ driver-sqlserver  │
   │  PgDriver (fat) │  │  MssqlDriver(fat) │
   │  PgExt trait    │  │  MssqlExt trait   │
   └─────────────────┘  └───────────────────┘
```

The `protocol` crate is imported by both `driver-api` (for return types) and
`server` (for operation dispatch). `driver-api` defines the trait, the
`Engine` enum, and the `ConnHandle` newtype. Each driver crate implements
`Driver` on a fat struct that owns its pool and connection state, and also
defines an extension trait for engine-specific operations.

---

## The core trait

Lives in `driver-api`. Eight verbs: `open`, `ping`, `schema`, `begin`,
`commit`, `rollback`, `execute`, `cancel`, `close`. Every method takes
`&self`, returns a boxed future via `async_trait`, and returns a concrete
protocol-crate type. Object-safe.

```rust
#[async_trait]
pub trait Driver: Send + Sync {
    /// Which engine this driver serves. Used by the server's registry
    /// and by clients to pick the right ext-trait downcast.
    fn engine(&self) -> Engine;

    /// Open a new connection from this driver's pool. Returns an opaque
    /// handle; the driver internally maps the handle id to its typed
    /// connection.
    async fn open(&self, spec: &ConnectionSpec)
        -> Result<ConnHandle, DriverError>;

    /// Cheap liveness check + server-reported metadata (server version,
    /// current database, authenticated user). Used by session-open health
    /// check and by reconnect logic.
    async fn ping(&self, c: ConnHandle)
        -> Result<ServerInfo, DriverError>;

    /// Introspect the catalog tree at the requested depth. Server caches
    /// the result per session; RefreshSchema op invalidates the cache.
    async fn schema(&self, c: ConnHandle, scope: SchemaScope)
        -> Result<SchemaSnapshot, DriverError>;

    /// Begin a transaction. The returned handle holds the connection it
    /// was opened on; commit/rollback consume it and return the connection
    /// to the pool.
    async fn begin(&self, c: ConnHandle, mode: TxMode)
        -> Result<TxHandle, DriverError>;

    async fn commit(&self, t: TxHandle)
        -> Result<(), DriverError>;

    async fn rollback(&self, t: TxHandle)
        -> Result<(), DriverError>;

    /// Execute SQL. Returns a stream handle, NOT rows. Rows arrive via
    /// `ResultSetStream::rows`. This is the design point that accommodates
    /// SQL Server's multi-result batches (and PG's simple-query protocol,
    /// which produces the same shape).
    async fn execute(&self, c: ConnHandle, req: ExecuteRequest)
        -> Result<ResultSetStream, DriverError>;

    /// Cancel an in-flight query on the given cursor. Maps to
    /// `pg_cancel_backend` (Postgres) or TDS attention (SQL Server).
    /// MUST be safe to call from a different task than execute() ran on;
    /// execute() registers its cancel token in a shared map keyed by
    /// CursorId at start, deregisters at end.
    async fn cancel(&self, c: ConnHandle, cursor: CursorId)
        -> Result<(), DriverError>;

    /// Close the underlying DB connection cleanly. Returns the handle to
    /// the pool for bookkeeping.
    async fn close(&self, c: ConnHandle)
        -> Result<(), DriverError>;

    /// Downcast to the Postgres extension trait. None if this driver is
    /// not Postgres. Default `None` so a driver doesn't implement both.
    fn as_pg(&self) -> Option<&dyn PgExt> { None }

    /// Downcast to the SQL Server extension trait. None if this driver is
    /// not SQL Server.
    fn as_mssql(&self) -> Option<&dyn MssqlExt> { None }
}
```

### Why these eight verbs and no more

Every relational engine we would plausibly add (Postgres, SQL Server, MySQL,
SQLite) has all nine (counting commit + rollback separately). Anything beyond
this set risks becoming engine-shaped: catalog switching is asymmetric
(Postgres can't, SQL Server can), LISTEN/NOTIFY is Postgres-only, MARS is
SQL Server-only, savepoints have divergent naming, COPY/bulk paths are
engine-specific. All of those live on extension traits.

### Object-safety checklist

- All methods `&self`, return boxed futures via `async_trait`.
- No method generics.
- All return types are concrete (protocol-crate types).
- `ConnHandle` is a concrete newtype, not an associated type.
- → `Box<dyn Driver>` and `Arc<dyn Driver>` both work. The server holds a
  `HashMap<Engine, Arc<dyn Driver>>` populated at startup by the driver
  registry/factory.

---

## ConnHandle

`ConnHandle` is a concrete newtype, not an associated type, so that `dyn
Driver` is object-safe. Internally it is an `Arc` over a small inner struct
carrying the connection id and a weak back-reference to the driver.

```rust
pub struct ConnHandle(Arc<ConnHandleInner>);

struct ConnHandleInner {
    /// Driver-issued id; unique within a driver instance for the lifetime
    /// of the connection. The driver maps this to its typed connection in
    /// a DashMap.
    id: u64,
    /// Which engine this connection belongs to. Redundant with the driver
    /// it came from but cheap to carry and avoids a lookup on the hot path.
    engine: Engine,
    /// Weak backref to the owning driver, so cancel/close can be issued
    /// from a task that received the ConnHandle by value without reholding
    /// the registry.
    driver: Weak<dyn Driver>,
}
```

The driver owns a `DashMap<u64, TypedConnection>` mapping ids to its real
connection (a `deadpool_postgres::Object` for Postgres, a `tiberius`
client for SQL Server). Cheap clone, pass across tasks freely. Slight Arc
allocation overhead per `open()`, acceptable.

The server holds `ConnHandle`s in the session's connection map; it never
reaches inside them.

---

## Fat per-engine structs

Each driver crate implements `Driver` on a fat struct that owns its pool,
tuning knobs, and engine-specific connection state. The trait surface is
thin; the struct carries everything else.

```rust
// driver-postgres
pub struct PgDriver {
    /// Main pool for queries / DDL / transactions.
    main_pool: deadpool_postgres::Pool,
    /// Dedicated pool for LISTEN/NOTIFY. LISTEN is connection-stateful —
    /// it must run on a connection that has issued LISTEN — so it cannot
    /// share the main pool without surprising other queries.
    listen_pool: deadpool_postgres::Pool,
    /// Per-connection cancel tokens, looked up by ConnId. execute() inserts
    /// at start, removes at end; cancel() reads and triggers.
    cancel_tokens: DashMap<ConnId, tokio_postgres::CancelToken>,
    /// Engine-specific config overrides from figment.
    cfg: PgConfigOverrides,
}

// driver-sqlserver
pub struct MssqlDriver {
    /// tiberius ships no pool; we wrap it in deadpool::managed.
    pool: MssqlPool,
    /// Whether new connections default to MARS on. Per-call override via
    /// MssqlExt::set_mars.
    mars_default: bool,
    /// Engine-specific config overrides from figment.
    cfg: MssqlConfigOverrides,
}
```

`PgDriver::cancel()` looks up `ConnId → CancelToken` and fires it from a
*different* task than execute ran on — driver isolation per ADR-013.
`MssqlDriver::cancel()` sends TDS attention on the same socket via a
separate lock; the socket itself is shared through the wrapped pool object.
Both fit the same trait method because the trait exposes only "cancel this
cursor," not the mechanism.

---

## Per-engine extension traits

Live in each driver crate, alongside the fat struct. The server downcasts
via `as_pg()` / `as_mssql()` when an engine-specific Operation arrives.
Wrong engine → `Code::UnsupportedForEngine`, never silent no-op.

```rust
// driver-postgres::ext
#[async_trait]
pub trait PgExt {
    /// LISTEN on the given channels. Must be called on a connection from
    /// the listen_pool; the returned stream yields notifications until
    /// unlisten or close.
    async fn listen(&self, c: ConnHandle, channels: Vec<String>)
        -> Result<NotificationStream, DriverError>;

    async fn unlisten(&self, c: ConnHandle, channels: Vec<String>)
        -> Result<(), DriverError>;

    /// COPY protocol for bulk in/out. Distinct from generic execute()
    /// because the wire format and streaming shape don't fit ResultSetStream.
    async fn copy(&self, c: ConnHandle, op: CopyOp)
        -> Result<CopyResult, DriverError>;

    /// Advisory locks. PG-only concept; no SQL Server analogue.
    async fn advisory_lock(&self, c: ConnHandle, key: AdvisoryKey)
        -> Result<(), DriverError>;

    /// Savepoint. PG naming: anonymous, rollback-to-name. Distinct enough
    /// from SQL Server's named savepoints to live on the ext trait rather
    /// than the core trait.
    async fn savepoint(&self, t: &TxHandle, name: &str)
        -> Result<PgSavepoint, DriverError>;

    async fn rollback_to(&self, sp: PgSavepoint)
        -> Result<(), DriverError>;
}

// driver-sqlserver::ext
#[async_trait]
pub trait MssqlExt {
    /// Switch the connection's current database without reconnecting.
    /// SQL Server supports `USE`; Postgres does not (database is part of
    /// connection identity). A "switch database" Operation routes here for
    /// SQL Server and fails with UnsupportedForEngine for Postgres, where
    /// the server's connection manager must open a new connection instead.
    async fn use_database(&self, c: ConnHandle, db: &str)
        -> Result<(), DriverError>;

    /// BULK INSERT path. Distinct from generic execute() for the same
    /// reason as PG COPY.
    async fn bulk_insert(&self, c: ConnHandle, op: BulkOp)
        -> Result<BulkResult, DriverError>;

    /// Toggle MARS (Multiple Active Result Sets) on the connection.
    async fn set_mars(&self, c: ConnHandle, enabled: bool)
        -> Result<(), DriverError>;

    /// Savepoint. SQL Server naming: `SAVE TRANSACTION n` / `ROLLBACK
    /// TRANSACTION n`. Different shape from PG savepoints, hence ext trait.
    async fn savepoint(&self, t: &TxHandle, name: &str)
        -> Result<MssqlSavepoint, DriverError>;

    async fn rollback_to(&self, sp: MssqlSavepoint)
        -> Result<(), DriverError>;
}
```

The server's operation dispatcher pattern for an engine-specific op:

```rust
Operation::PgListen { channels } => {
    let driver = registry.get(session.connection.engine);
    let pg = driver.as_pg().ok_or(Code::UnsupportedForEngine)?;
    pg.listen(handle, channels).await
}
```

`as_pg()` / `as_mssql()` are **server-internal**. They are not part of the
public protocol surface; only `Operation` and the protocol crate types are
public.

---

## Union protocol types

Live in the `protocol` crate (ADR-004: pure serde, no I/O). These models
are the **union** of both engines' shapes: a superset where engine-specific
facets are optional, tagged, and carried verbatim rather than flattened.

### ColumnMetadata

```rust
pub struct ColumnMetadata {
    pub name: String,
    pub type_ref: TypeRef,
    pub nullable: Nullability,
    pub auto_increment: bool,
    pub primary_key: bool,
    /// Engine-specific facets. Both fields are Option; the client renders
    /// whichever is Some, keyed by the connection's engine.
    pub facets: EngineColumnFacets,
}

pub struct EngineColumnFacets {
    pub postgres: Option<PgColumnFacets>,    // identity col, array dims, OID
    pub sql_server: Option<MssqlColumnFacets>, // TDS type info, collation
}
```

### TypeRef — the type-system escape hatch

```rust
pub enum TypeRef {
    /// A "well-known" primitive the IDE renders natively across engines.
    /// This is the IDE's own type vocabulary; it does not try to capture
    /// every engine's native types.
    Primitive(PrimitiveType),
    /// Engine { engine, name, category } is the escape hatch for types
    /// that don't map cleanly across engines: varchar(max), tsvector,
    /// sql_variant, xml, hstore, jsonb, citext, etc. The client renders
    /// the native name verbatim with the engine as a hint, and uses
    /// `category` to pick a renderer (numeric, text, binary, composite,
    /// enum, temporal).
    Engine { engine: Engine, name: String, category: TypeCategory },
}

pub enum PrimitiveType {
    Int16, Int32, Int64,
    Float32, Float64,
    Decimal,
    Bool,
    Text, Blob,
    Date, Time, Timestamp, TimestampTz,
    Interval,
    Uuid,
    Json, Jsonb,
}

pub enum TypeCategory {
    Numeric, Text, Binary, Temporal, Boolean,
    Uuid, Json, Composite, Enum, Array, Range,
    Geometric, BitString, NetworkAddress, Xml, Other,
}
```

This is the mechanism that avoids the JDBC lowest-common-denominator trap
on types: rather than trying to flatten every native type into a single
enumeration and losing information, the trait carries the engine-native
name verbatim alongside a best-effort primitive the IDE can use for
rendering. Clients never need to interpret `TypeRef::Engine` to render a
cell — they fall back to text. Clients that want richer rendering (e.g. an
array editor) can do so when `category` matches.

### ObjectKind

```rust
pub enum ObjectKind {
    Table,
    View,
    MaterializedView,          // Postgres-only
    TableValuedFunction,
    ScalarFunction,
    Procedure,
    Synonym,                   // SQL Server
    Sequence,                  // both, but PG-heavy
    Trigger,
    Type,
    Extension,                 // Postgres
}
```

### Error model

Driver errors are translated at the trait boundary into a driver-agnostic
`DriverError` carrying a stable `Code` (ADR-004 candidate, see PHASE0.md
step 3). Raw driver error strings never cross the wire.

```rust
pub struct DriverError {
    pub code: Code,
    pub message: String,
    pub engine: Option<Engine>,
    pub engine_sqlstate: Option<String>,  // PG SQLSTATE / tiberius error code
}

pub enum Code {
    ConnectionFailed,
    AuthFailed,
    QueryTimedOut,
    QueryCanceled,
    SyntaxError,
    UndefinedObject,
    DuplicateObject,
    InvalidParameterValue,
    UnsupportedForEngine,
    DriverInternal,
    // ... grows as implementation surfaces real cases
}
```

---

## Schema introspection — SchemaScope

`schema()` is scoped by `SchemaScope` so the server can do one shallow pass
at session-open (catalog/db/schema/object names only) and a deep pass per
tree-expand (columns, types, indexes for one object). This matches Zed
lesson §2.2 (progressive post-paint indexing) and avoids a full-catalog
round-trip on connect.

```rust
pub struct SchemaScope {
    pub depth: SchemaDepth,
    pub filter: Option<SchemaFilter>,
}

pub enum SchemaDepth {
    /// Names only: catalogs → databases → schemas → object names + kinds.
    /// Used at session-open.
    Shallow,
    /// One object fully described: columns with ColumnMetadata, indexes,
    /// triggers, constraints, dependencies. Used on tree-expand.
    Deep { object: ObjectPath },
}

pub struct SchemaFilter {
    pub catalogs: Option<Vec<String>>,
    pub schemas: Option<Vec<String>>,
    pub kinds: Option<Vec<ObjectKind>>,
    pub name_pattern: Option<glob::Pattern>,
}

pub struct SchemaSnapshot {
    pub trees: Vec<CatalogTree>,
    pub fetched_at: DateTime<Utc>,
    pub scope: SchemaScope,        // echo back what was requested
    pub incomplete: bool,          // true if filter truncated or round-trip timed out
}
```

The server caches `SchemaSnapshot` per session. The RefreshSchema operation
invalidates the cache and re-issues the current scope. Catalog invalidation
signals (LISTEN/NOTIFY for Postgres, polling for SQL Server) are a Tier 2
backend item; the trait doesn't model them — the server subscribes/polls
and triggers RefreshSchema when something changes.

---

## Result streaming — the multi-result-batch design point

This is the SQL Server design point that shapes the result model. SQL
Server returns multiple result sets from one batch (`SELECT 1; SELECT 2;`).
Postgres's simple query protocol produces the same shape. The trait models
it once; single-statement queries just emit one result set followed by
`Done`.

`execute()` returns a stream handle, **not** rows. Rows arrive through an
`mpsc::Receiver<Page>` owned by the server. The server is the sole consumer
of that receiver; multi-subscriber fan-out (Tier 3 multi-user) is a server
concern, not a driver concern — aligns with Zed lesson §3.5 (results are
server-authoritative; share a reference, not the data).

```rust
pub struct ResultSetStream {
    pub cursor_id: CursorId,
    pub columns: Vec<ColumnMetadata>,
    pub rows: mpsc::Receiver<Page>,
    pub warnings: Vec<DriverWarning>,
    pub affected_rows: Option<u64>,
    /// false = small result already materialized in the receiver's buffer;
    /// true = the driver holds a server-side cursor and pages on demand.
    pub server_side_cursor: bool,
}

pub enum Page {
    Rows(Vec<Row>),
    /// A new result set is starting within the same batch. Carries the
    /// new column layout. Emitted between result sets of a multi-statement
    /// batch; single-statement queries never emit this.
    NextResult { columns: Vec<ColumnMetadata> },
    /// End of the stream. affected_rows is the cumulative count for the
    /// whole batch; warnings collected along the way.
    Done { affected_rows: Option<u64>, warnings: Vec<DriverWarning> },
}
```

Cursor eviction policy is PHASE0.md Tier 0 item 8 (ADR-011 candidate). The
trait just exposes `CursorId` so the server can map a cursor to its
driver-internal state for paging and cancel.

---

## Transactions

```rust
pub struct TxMode {
    pub isolation: IsolationLevel,
    pub access: AccessMode,    // ReadWrite | ReadOnly
}

pub enum IsolationLevel {
    ReadUncommitted,
    ReadCommitted,
    RepeatableRead,
    Snapshot,
    Serializable,
}

pub struct TxHandle {
    pub tx_id: TxId,
    pub conn: ConnHandle,      // the connection the tx is bound to
    pub mode: TxMode,
}
```

Begin / commit / rollback are on the core trait (both engines have them in
identical shape). **Savepoints are not**: PG's anonymous savepoint-with-
rollback-to-name and SQL Server's `SAVE TRANSACTION n` / `ROLLBACK
TRANSACTION n` have divergent naming semantics and live on the
corresponding ext traits. The transactions panel (FEATURES.md Tier 2 #23)
dispatches savepoint ops through the appropriate downcast.

A transaction holds its connection for its lifetime. The server's connection
manager must account for this: a connection in an open tx is not available
to the pool until commit/rollback.

---

## Driver isolation (ADR-013 candidate)

Every method on the core trait must be safe to call from a `tokio::spawn`'d
task. Concretely:

- **No blocking calls** inside driver methods. tiberius and tokio-postgres
  are both async-native; the driver's job is to keep that property.
- **Cancel is callable from a different task than execute.** execute()
  registers its cancel token in a shared `DashMap<CursorId, CancelToken>`
  at start and removes it at end. cancel() looks up by CursorId and
  triggers — it does not need to coordinate with the execute task.
- **The server dispatcher spawns each execute in its own task** with a
  timeout and a cancel token, never runs queries inline in the handler.
  This is what makes "wedged driver cannot freeze the server" true.
- **Pool acquisition is non-blocking with a timeout.** deadpool-postgres
  and the tiberius wrapper both support this; if the pool is exhausted the
  caller gets `Code::ConnectionFailed` (or a more specific
  `PoolExhausted`), not a hang.

---

## Engine asymmetries handled by this design

| Asymmetry | Postgres | SQL Server | Where handled |
| --- | --- | --- | --- |
| Database switching | New connection required | `USE` in place | `MssqlExt::use_database`; PG routes to UnsupportedForEngine and server opens new conn |
| Async events | LISTEN/NOTIFY | None native (Query Notifications is heavy) | `PgExt::listen`/`unlisten`; no SQL Server analogue |
| Concurrent ops on one conn | One statement at a time | MARS | `MssqlExt::set_mars`; PG just opens another pooled conn |
| Bulk in/out | COPY protocol | BULK INSERT / bcp | `PgExt::copy`, `MssqlExt::bulk_insert` |
| Cancel mechanism | `pg_cancel_backend` from another conn | TDS attention on same socket | Both hidden behind `Driver::cancel` |
| Multi-result batches | Simple query protocol | Native | `Page::NextResult` in protocol crate |
| Savepoint naming | Anonymous + rollback-to-name | `SAVE TRANSACTION n` | Per-ext-trait `savepoint` |
| Advisory locks | Native | None | `PgExt::advisory_lock` |
| Schema object kinds | Materialized views, extensions, sequences | Synonyms, richer proc metadata | `ObjectKind` union enum |

---

## Anti-JDBC-LCD explicit list

This trait deliberately does **not** do the following, each of which is a
JDBC-style failure mode:

- **No `executeQuery` / `executeUpdate` / `executeLargeUpdate` split.**
  One `execute`, the stream tells you what happened (rows vs affected
  count vs both).
- **No `ResultSet` trait drivers implement.** Protocol owns `Page` / `Row`;
  drivers convert.
- **No `DatabaseMetaData` god-object.** Typed `SchemaSnapshot` returned
  from `schema()`.
- **No "try the method, catch `SQLFeatureNotSupportedException`".**
  Capability is advertised up front via `as_pg()` / `as_mssql()` returning
  `Option`; the server consults capability before dispatching.
- **No implicit connection transaction state.** Explicit `TxHandle`.
- **No flatten-all-types-to-LCD enumeration.** `TypeRef::Engine` carries
  the native name verbatim.
- **No engine-specific methods on the core trait that one engine throws
  `Unsupported` for.** Engine-specific ops live on ext traits.

---

## Two-impl validation gate

From the project decision log: **the trait is not "public" until both
Postgres and SQL Server pass through it.** A trait with one implementation
is a struct wearing a hat. Concretely:

- Step 7 (Postgres impl) is allowed to refactor the trait freely. The trait
  API in `driver-api` is in flux.
- Step 15 (SQL Server impl) is the stress test. SQL Server's catalog model,
  multi-result batches, full column metadata, MARS, attention semantics hit
  the trait where Postgres didn't. Refactor if a flaw surfaces — expected,
  and contained to the driver layer because the server substrate is already
  stable.
- After step 16 (SQL Server integration tests pass): trait API locked.
  Further changes require justification + a protocol version bump.
- **Public surface = `protocol` crate + `Operation` enum only.** The
  `Driver` trait and ext traits are server-internal and may evolve
  post-step-16 with less ceremony, though signature changes still gate a
  protocol bump if they propagate.

---

## Resulting ADR candidates

This spec implies follow-up decisions to graduate into `DECISIONS.md`:

- **ADR-011 (candidate): result streaming via server-side cursors.** Already
  flagged in `ZED_LESSONS.md` §6. The `ResultSetStream` + `Page` model
  above is the trait-side foundation; the cursor-eviction policy is the
  server-side follow-up.
- **ADR-013 (candidate): driver isolation.** Each driver method safe inside
  `tokio::spawn`; cancel callable cross-task; queries never run inline in
  handlers. Already flagged in `ZED_LESSONS.md` §6.
- **ADR-017 (candidate): driver trait shape as designed here.** Thin core
  + fat structs + ext traits + union types; two-impl validation gate
  before locking.

---

## Sequencing into PHASE0.md

This spec satisfies PHASE0.md step 4 (TODO.md item 0.1). Implementation
work that depends on it:

- TODO.md item 2 — protocol v1 union types (`Connection`, `Catalog`,
  `Schema`, `Object`, `ColumnMetadata`, `TypeRef`, error codes). The shapes
  above are the input.
- TODO.md item 12 — `driver-api` trait (this spec, ported to Rust).
- TODO.md item 15 — `driver-postgres` impl.
- TODO.md item 22 — SQL Server ext-trait surface (`MssqlExt`).
- TODO.md item 23 — `driver-sqlserver` impl.
- TODO.md item 25 — trait refactor if step 23 exposes a flaw.
