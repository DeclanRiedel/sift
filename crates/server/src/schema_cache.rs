//! Per-spec schema cache with 60s TTL + engine-specific invalidators
//! (ADR-lite; Phase C schema cache item).
//!
//! Key = `(spec_hash, canonical_scope_json)`; two connections to the
//! same DB share cache entries. Invalidation paths:
//!
//! - **PG**: dedicated LISTEN connection on `sift_schema_change`. The
//!   user opts in by installing a DDL event trigger that
//!   `NOTIFY`s the channel. Falls back to TTL if the trigger isn't
//!   installed.
//! - **SQL Server**: a background poller runs `SELECT
//!   MAX(modify_date) FROM sys.objects` every 30s; invalidates the
//!   spec on change. TTL is the ceiling regardless.
//!
//! TTL default is 60s. Both invalidator strategies are best-effort —
//! the TTL guarantees a stale entry is refreshed within `ttl` even if
//! invalidation is skipped.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use sift_completion::Dictionary;
use sift_driver_api::{Driver, ResultSetStream};
use sift_protocol::{
    ConnectionSpec, Engine, ExecuteRequest, Page, SchemaScope, SchemaSnapshot, Value,
};
use tokio::task::JoinHandle;

/// Cache-wide configuration.
#[derive(Debug, Clone)]
pub struct SchemaCacheConfig {
    /// Hard TTL for a cached entry regardless of invalidation. A
    /// client's schema panel sees at most `ttl` staleness.
    pub ttl: Duration,
    /// Polling interval for the SQL Server invalidator. Ignored for
    /// PG connections (which use LISTEN/NOTIFY).
    pub mssql_poll_interval: Duration,
}

impl Default for SchemaCacheConfig {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(60),
            mssql_poll_interval: Duration::from_secs(30),
        }
    }
}

#[derive(Clone, Default)]
pub struct SchemaCache {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    config: std::sync::RwLock<SchemaCacheConfig>,
    entries: DashMap<CacheKey, CachedEntry>,
    /// `spec_hash → invalidator task`. Tasks are spawned lazily on
    /// first `insert` for a spec; kept alive for the process lifetime.
    invalidators: DashMap<String, InvalidatorHandle>,
    /// Metrics — atomic counters exposed for tests.
    hits: std::sync::atomic::AtomicU64,
    misses: std::sync::atomic::AtomicU64,
    invalidations: std::sync::atomic::AtomicU64,
}

#[derive(Hash, PartialEq, Eq, Clone, Debug)]
struct CacheKey {
    spec_hash: String,
    scope_key: String,
}

struct CachedEntry {
    schema: CachedSchema,
    inserted_at: Instant,
}

#[derive(Clone)]
pub struct CachedSchema {
    pub snapshot: Arc<SchemaSnapshot>,
    pub dictionary: Arc<Dictionary>,
}

impl CachedSchema {
    pub fn new_uncached(snapshot: SchemaSnapshot) -> Self {
        let dictionary = Arc::new(Dictionary::from_snapshot(&snapshot));
        Self {
            snapshot: Arc::new(snapshot),
            dictionary,
        }
    }
}

struct InvalidatorHandle {
    /// Kept so drop aborts the task on server shutdown. `_` because we
    /// don't need to poll it; abort on drop is sufficient.
    _task: JoinHandle<()>,
}

impl Drop for InvalidatorHandle {
    fn drop(&mut self) {
        self._task.abort();
    }
}

impl SchemaCache {
    pub fn new(config: SchemaCacheConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                config: std::sync::RwLock::new(config),
                ..Default::default()
            }),
        }
    }

    pub fn set_config(&self, config: SchemaCacheConfig) {
        *self.inner.config.write().unwrap() = config;
    }

    pub fn config(&self) -> SchemaCacheConfig {
        self.inner.config.read().unwrap().clone()
    }

    /// Look up a cached snapshot. Returns `None` on miss or expired
    /// (TTL-based). Expired entries are dropped as a side effect.
    pub fn get(&self, spec: &ConnectionSpec, scope: &SchemaScope) -> Option<SchemaSnapshot> {
        self.get_cached(spec, scope)
            .map(|cached| (*cached.snapshot).clone())
    }

    /// Look up a cached snapshot with its prebuilt completion dictionary.
    pub fn get_cached(&self, spec: &ConnectionSpec, scope: &SchemaScope) -> Option<CachedSchema> {
        let Ok(key) = self.key_for(spec, scope) else {
            self.inner
                .misses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return None;
        };
        let ttl = self.config().ttl;
        let mut expired = false;
        let snap = match self.inner.entries.get(&key) {
            Some(entry) if entry.inserted_at.elapsed() < ttl => Some(entry.schema.clone()),
            Some(_) => {
                expired = true;
                None
            }
            None => None,
        };
        let Some(snap) = snap else {
            if expired {
                drop(self.inner.entries.remove(&key));
            }
            self.inner
                .misses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return None;
        };
        {
            self.inner
                .hits
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Some(snap)
    }

    /// Insert a fresh snapshot. Spawns an invalidator task for the
    /// spec on the first insert.
    pub fn insert(
        &self,
        spec: &ConnectionSpec,
        scope: &SchemaScope,
        snapshot: SchemaSnapshot,
        driver: Arc<dyn Driver>,
    ) -> Option<CachedSchema> {
        let (key, spec_hash) = match self.key_for_with_hash(spec, scope) {
            Ok(pair) => pair,
            Err(_) => return None, // serialization failure — skip caching
        };
        let schema = CachedSchema::new_uncached(snapshot);
        self.inner.entries.insert(
            key,
            CachedEntry {
                schema: schema.clone(),
                inserted_at: Instant::now(),
            },
        );
        // Ensure invalidator is running for this spec.
        self.ensure_invalidator(spec, spec_hash, driver);
        Some(schema)
    }

    /// Invalidate every cached entry for a spec. Called by the
    /// invalidator tasks; also public so operators / tests can flush
    /// on demand.
    pub fn invalidate_spec(&self, spec: &ConnectionSpec) {
        let Ok(spec_hash) = self.spec_hash(spec) else {
            return;
        };
        self.invalidate_spec_by_hash(&spec_hash);
    }

    fn invalidate_spec_by_hash(&self, spec_hash: &str) {
        let victims: Vec<CacheKey> = self
            .inner
            .entries
            .iter()
            .filter(|e| e.key().spec_hash == spec_hash)
            .map(|e| e.key().clone())
            .collect();
        for k in victims {
            self.inner.entries.remove(&k);
        }
        self.inner
            .invalidations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn cache_hits(&self) -> u64 {
        self.inner.hits.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn cache_misses(&self) -> u64 {
        self.inner.misses.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn invalidation_count(&self) -> u64 {
        self.inner
            .invalidations
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn entry_count(&self) -> usize {
        self.inner.entries.len()
    }

    pub fn invalidator_count(&self) -> usize {
        self.inner.invalidators.len()
    }

    fn key_for(
        &self,
        spec: &ConnectionSpec,
        scope: &SchemaScope,
    ) -> Result<CacheKey, serde_json::Error> {
        Ok(self.key_for_with_hash(spec, scope)?.0)
    }

    fn key_for_with_hash(
        &self,
        spec: &ConnectionSpec,
        scope: &SchemaScope,
    ) -> Result<(CacheKey, String), serde_json::Error> {
        let spec_hash = self.spec_hash(spec)?;
        let scope_key = serde_json::to_string(scope)?;
        Ok((
            CacheKey {
                spec_hash: spec_hash.clone(),
                scope_key,
            },
            spec_hash,
        ))
    }

    fn spec_hash(&self, spec: &ConnectionSpec) -> Result<String, serde_json::Error> {
        use sha2::{Digest, Sha256};
        #[derive(serde::Serialize)]
        struct CacheIdentity<'a> {
            host: &'a str,
            port: Option<u16>,
            database: Option<&'a str>,
            user: &'a str,
            ssl_mode: Option<sift_protocol::SslMode>,
            engine_specific: Option<&'a sift_protocol::EngineConnectionSpec>,
        }
        let identity = CacheIdentity {
            host: &spec.host,
            port: spec.port,
            database: spec.database.as_deref(),
            user: &spec.user,
            ssl_mode: spec.ssl_mode,
            engine_specific: spec.engine_specific.as_ref(),
        };
        let json = serde_json::to_string(&identity)?;
        let hash = Sha256::digest(json.as_bytes());
        let mut out = String::with_capacity(64);
        for b in hash {
            use std::fmt::Write as _;
            let _ = write!(out, "{b:02x}");
        }
        Ok(out)
    }

    fn ensure_invalidator(
        &self,
        spec: &ConnectionSpec,
        spec_hash: String,
        driver: Arc<dyn Driver>,
    ) {
        if self.inner.invalidators.contains_key(&spec_hash) {
            return;
        }
        let engine = driver.engine();
        let spec_clone = spec.clone();
        let cache = self.clone();
        let poll = self.config().mssql_poll_interval;
        let task = match engine {
            Engine::Postgres => {
                tokio::spawn(pg_listen_task(spec_clone, driver, cache, spec_hash.clone()))
            }
            Engine::SqlServer => tokio::spawn(mssql_poll_task(
                spec_clone,
                driver,
                cache,
                spec_hash.clone(),
                poll,
            )),
        };
        // Race-safe insert: if another caller inserted a handle
        // meanwhile, abort the one we just spawned.
        match self.inner.invalidators.entry(spec_hash) {
            dashmap::mapref::entry::Entry::Occupied(_) => {
                task.abort();
            }
            dashmap::mapref::entry::Entry::Vacant(v) => {
                v.insert(InvalidatorHandle { _task: task });
            }
        }
    }
}

/// PG invalidator: opens a dedicated LISTEN connection and invalidates
/// the spec on every notification. Falls back silently if the user
/// hasn't installed the DDL trigger — the 60s TTL still bounds
/// staleness.
async fn pg_listen_task(
    spec: ConnectionSpec,
    driver: Arc<dyn Driver>,
    cache: SchemaCache,
    spec_hash: String,
) {
    // Open a dedicated conn.
    let handle = match driver.open(&spec).await {
        Ok(h) => h,
        Err(error) => {
            tracing::debug!(%error, "schema invalidator: PG open failed");
            return;
        }
    };
    let Some(pg) = driver.as_pg() else {
        tracing::debug!("schema invalidator: PG driver has no PgExt");
        let _ = driver.close(handle).await;
        return;
    };
    let stream = match pg
        .listen(handle.clone(), vec!["sift_schema_change".to_string()])
        .await
    {
        Ok(s) => s,
        Err(error) => {
            tracing::debug!(%error, "schema invalidator: LISTEN failed");
            let _ = driver.close(handle).await;
            return;
        }
    };
    let mut notifications = stream.notifications;
    while let Some(_notification) = notifications.recv().await {
        cache.invalidate_spec_by_hash(&spec_hash);
    }
    let _ = driver.close(handle).await;
}

/// SQL Server invalidator: polls `MAX(modify_date)` and invalidates on
/// change. Cheap in the steady state (a single row scan of
/// `sys.objects`).
async fn mssql_poll_task(
    spec: ConnectionSpec,
    driver: Arc<dyn Driver>,
    cache: SchemaCache,
    spec_hash: String,
    poll_interval: Duration,
) {
    let handle = match driver.open(&spec).await {
        Ok(h) => h,
        Err(error) => {
            tracing::debug!(%error, "schema invalidator: MSSQL open failed");
            return;
        }
    };
    let mut last: Option<String> = None;
    let mut ticker = tokio::time::interval(poll_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await; // consume the immediate first tick
    loop {
        ticker.tick().await;
        let req = ExecuteRequest {
            sql: "SELECT CONVERT(varchar(30), MAX(modify_date), 121) FROM sys.objects".into(),
            params: Vec::new(),
        };
        let value = match read_first_scalar(&*driver, handle.clone(), req).await {
            Ok(v) => v,
            Err(error) => {
                tracing::debug!(%error, "schema invalidator: MSSQL poll failed");
                continue;
            }
        };
        match (&last, &value) {
            (Some(a), Some(b)) if a == b => {}
            _ => {
                if last.is_some() {
                    cache.invalidate_spec_by_hash(&spec_hash);
                }
                last = value;
            }
        }
    }
}

/// Drain a driver stream and return the first scalar cell as a
/// canonical string. Used by the MSSQL poller.
async fn read_first_scalar(
    driver: &dyn Driver,
    handle: sift_driver_api::ConnHandle,
    req: ExecuteRequest,
) -> Result<Option<String>, sift_protocol::DriverError> {
    let ResultSetStream { mut rows, .. } = driver.execute(handle, req).await?;
    let mut out: Option<String> = None;
    while let Some(page) = rows.recv().await {
        match page {
            Page::Rows { rows } => {
                if out.is_none() {
                    if let Some(first) = rows.into_iter().next() {
                        if let Some(v) = first.values.into_iter().next() {
                            out = Some(value_to_string(v));
                        }
                    }
                }
            }
            Page::Done { .. } => break,
            Page::Error { error } => return Err(error),
            _ => {}
        }
    }
    Ok(out)
}

fn value_to_string(v: Value) -> String {
    match v {
        Value::Text(s) => s,
        Value::Null => "".into(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::{ConnectionSpec, SchemaDepth, SchemaScope, SchemaSnapshot, SslMode};

    fn spec() -> ConnectionSpec {
        ConnectionSpec {
            host: "h".into(),
            port: Some(5432),
            database: Some("db".into()),
            user: "u".into(),
            password: Some("p".into()),
            ssl_mode: Some(SslMode::Prefer),
            engine_specific: None,
        }
    }

    fn scope() -> SchemaScope {
        SchemaScope {
            depth: SchemaDepth::Shallow,
            filter: None,
        }
    }

    #[test]
    fn miss_on_empty_cache() {
        let cache = SchemaCache::new(SchemaCacheConfig::default());
        assert!(cache.get(&spec(), &scope()).is_none());
        assert_eq!(cache.cache_misses(), 1);
    }

    #[test]
    fn hit_after_insert_within_ttl() {
        let cache = SchemaCache::new(SchemaCacheConfig::default());
        let snap = SchemaSnapshot::empty(scope());
        // Insert without spawning invalidator (bypass ensure): use the
        // low-level entry insertion to keep this test hermetic.
        let (key, _) = cache.key_for_with_hash(&spec(), &scope()).unwrap();
        cache.inner.entries.insert(
            key,
            CachedEntry {
                schema: CachedSchema::new_uncached(snap.clone()),
                inserted_at: Instant::now(),
            },
        );
        let got = cache.get(&spec(), &scope()).unwrap();
        assert_eq!(cache.cache_hits(), 1);
        // Sanity — same scope round-trips.
        assert_eq!(
            serde_json::to_string(&got).unwrap(),
            serde_json::to_string(&snap).unwrap()
        );
    }

    #[test]
    fn cached_dictionary_is_reused_across_hits() {
        let cache = SchemaCache::new(SchemaCacheConfig::default());
        let (key, _) = cache.key_for_with_hash(&spec(), &scope()).unwrap();
        cache.inner.entries.insert(
            key,
            CachedEntry {
                schema: CachedSchema::new_uncached(SchemaSnapshot::empty(scope())),
                inserted_at: Instant::now(),
            },
        );

        let first = cache.get_cached(&spec(), &scope()).unwrap();
        let second = cache.get_cached(&spec(), &scope()).unwrap();

        assert!(Arc::ptr_eq(&first.dictionary, &second.dictionary));
        assert!(Arc::ptr_eq(&first.snapshot, &second.snapshot));
    }

    #[test]
    fn expired_entry_is_evicted_on_get() {
        let cache = SchemaCache::new(SchemaCacheConfig {
            ttl: Duration::from_millis(1),
            ..Default::default()
        });
        let (key, _) = cache.key_for_with_hash(&spec(), &scope()).unwrap();
        cache.inner.entries.insert(
            key.clone(),
            CachedEntry {
                schema: CachedSchema::new_uncached(SchemaSnapshot::empty(scope())),
                inserted_at: Instant::now() - Duration::from_millis(100),
            },
        );
        assert!(cache.get(&spec(), &scope()).is_none());
        assert!(!cache.inner.entries.contains_key(&key));
        assert_eq!(cache.cache_misses(), 1);
    }

    #[test]
    fn invalidate_spec_drops_all_entries_for_that_spec() {
        let cache = SchemaCache::new(SchemaCacheConfig::default());
        // Two entries for spec A, one for spec B.
        let (kaa, _) = cache.key_for_with_hash(&spec(), &scope()).unwrap();
        let deep = SchemaScope {
            depth: SchemaDepth::Deep {
                object: sift_protocol::ObjectPath {
                    catalog: None,
                    schema: Some("public".into()),
                    name: "users".into(),
                    kind: None,
                    routine_args: None,
                },
            },
            filter: None,
        };
        let (kab, _) = cache.key_for_with_hash(&spec(), &deep).unwrap();
        let mut spec_b = spec();
        spec_b.host = "other".into();
        let (kbb, _) = cache.key_for_with_hash(&spec_b, &scope()).unwrap();
        for k in [&kaa, &kab, &kbb] {
            cache.inner.entries.insert(
                k.clone(),
                CachedEntry {
                    schema: CachedSchema::new_uncached(SchemaSnapshot::empty(scope())),
                    inserted_at: Instant::now(),
                },
            );
        }

        cache.invalidate_spec(&spec());
        assert!(!cache.inner.entries.contains_key(&kaa));
        assert!(!cache.inner.entries.contains_key(&kab));
        assert!(cache.inner.entries.contains_key(&kbb));
        assert_eq!(cache.invalidation_count(), 1);
    }

    #[test]
    fn spec_hash_stable_across_calls() {
        let cache = SchemaCache::new(SchemaCacheConfig::default());
        let h1 = cache.spec_hash(&spec()).unwrap();
        let h2 = cache.spec_hash(&spec()).unwrap();
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn spec_hash_differs_across_hosts() {
        let cache = SchemaCache::new(SchemaCacheConfig::default());
        let mut b = spec();
        b.host = "other".into();
        assert_ne!(
            cache.spec_hash(&spec()).unwrap(),
            cache.spec_hash(&b).unwrap()
        );
    }

    #[test]
    fn spec_hash_ignores_password() {
        let cache = SchemaCache::new(SchemaCacheConfig::default());
        let mut without_password = spec();
        without_password.password = None;
        let mut rotated_password = spec();
        rotated_password.password = Some("rotated".into());
        assert_eq!(
            cache.spec_hash(&without_password).unwrap(),
            cache.spec_hash(&rotated_password).unwrap()
        );
    }
}
