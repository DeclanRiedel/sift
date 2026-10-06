//! Immutable bounded AI excerpts. Observes pages without advancing a cursor.
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sift_metadata::{ConnectionProfileId, PrincipalId, TenantId};
use sift_protocol::{ColumnMetadata, ConnectionId, CursorId, Page, Row, SessionId};
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};

const TTL: Duration = Duration::from_secs(600);
const MAX_RESULTS: usize = 128;
const MAX_PER_SESSION: usize = 8;
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_RESULT_BYTES: usize = 512 * 1024;
const MAX_ROWS: usize = 256;
const MAX_SETS: usize = 8;
const MAX_SQL_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub(crate) struct AiResultProvenance {
    pub actor: PrincipalId,
    pub tenant: TenantId,
    pub profile: ConnectionProfileId,
    pub session: SessionId,
    pub connection: ConnectionId,
    pub cursor: CursorId,
    pub sql: String,
    pub sql_truncated: bool,
    pub retained_until: chrono::DateTime<chrono::Utc>,
}
#[derive(Clone)]
pub(crate) struct AiResultSet {
    pub columns: Vec<ColumnMetadata>,
    pub schema_digest: String,
    pub rows: Vec<Row>,
    pub rows_seen: usize,
    pub truncated: bool,
}
struct Entry {
    provenance: AiResultProvenance,
    sets: Vec<Option<AiResultSet>>,
    current_set: Option<usize>,
    sealed: bool,
    interrupted: bool,
    bytes: usize,
    created: Instant,
}
#[derive(Clone, Default)]
pub(crate) struct AiResultRegistry {
    entries: Arc<Mutex<HashMap<Uuid, Entry>>>,
}
impl AiResultRegistry {
    pub fn start(&self, mut provenance: AiResultProvenance) -> Uuid {
        let id = Uuid::new_v4();
        provenance.retained_until =
            chrono::Utc::now() + chrono::Duration::seconds(TTL.as_secs() as i64);
        if provenance.sql.len() > MAX_SQL_BYTES {
            let mut end = MAX_SQL_BYTES;
            while !provenance.sql.is_char_boundary(end) {
                end -= 1;
            }
            provenance.sql.truncate(end);
            provenance.sql_truncated = true;
        }
        let mut entries = self.entries.lock().unwrap();
        reap(&mut entries);
        while entries
            .values()
            .filter(|entry| entry.provenance.session == provenance.session)
            .count()
            >= MAX_PER_SESSION
        {
            evict_oldest(&mut entries, Some(provenance.session), None);
        }
        while entries.len() >= MAX_RESULTS {
            evict_oldest(&mut entries, None, None);
        }
        entries.insert(
            id,
            Entry {
                bytes: provenance.sql.len(),
                provenance,
                sets: Vec::new(),
                current_set: None,
                sealed: false,
                interrupted: false,
                created: Instant::now(),
            },
        );
        enforce_bytes(&mut entries, id);
        id
    }
    pub fn observe(&self, id: Uuid, page: &Page) {
        let mut entries = self.entries.lock().unwrap();
        let Some(entry) = entries.get_mut(&id) else {
            return;
        };
        if entry.created.elapsed() >= TTL || entry.sealed {
            return;
        }
        match page {
            Page::NextResult { columns } => append_header(entry, columns),
            Page::Rows { rows } => append_rows(entry, rows),
            Page::Done { .. } => entry.sealed = true,
            Page::Error { .. } => {
                entry.sealed = true;
                entry.interrupted = true;
            }
        }
        enforce_bytes(&mut entries, id);
    }
    pub fn record_http(
        &self,
        provenance: AiResultProvenance,
        response: &sift_protocol::ExecuteResponse,
    ) -> Uuid {
        let id = self.start(provenance);
        let mut entries = self.entries.lock().unwrap();
        if let Some(entry) = entries.get_mut(&id) {
            append_header(entry, &response.columns);
            append_rows(entry, &response.rows);
            if let Some(Some(set)) = entry.sets.first_mut() {
                set.truncated |= response.has_more;
            }
            entry.sealed = true;
        }
        enforce_bytes(&mut entries, id);
        id
    }
    // Called when a pump closes without a terminal page as well as on normal
    // completion. Partial rows are immutable, but always visibly interrupted.
    pub fn finish(&self, id: Uuid) {
        if let Some(entry) = self.entries.lock().unwrap().get_mut(&id) {
            if !entry.sealed {
                entry.interrupted = true;
            }
            entry.sealed = true;
        }
    }
    pub fn get(
        &self,
        actor: PrincipalId,
        id: Uuid,
        result_set: u32,
        digest: &str,
    ) -> ApiResult<(AiResultProvenance, AiResultSet, bool)> {
        let mut entries = self.entries.lock().unwrap();
        reap(&mut entries);
        let unavailable = || {
            ApiError::BadRequest("AI result excerpt expired, was evicted, or is not retained. Run a query yourself to obtain a new result.".into())
        };
        let entry = entries
            .get(&id)
            .filter(|entry| entry.provenance.actor == actor)
            .ok_or_else(unavailable)?;
        if !entry.sealed {
            return Err(ApiError::BadRequest(
                "Wait for the result to finish before attaching rows".into(),
            ));
        }
        let set = entry
            .sets
            .get(result_set as usize)
            .and_then(Option::as_ref)
            .ok_or_else(unavailable)?;
        if set.schema_digest != digest {
            return Err(ApiError::BadRequest(
                "Result schema changed; select rows from the current result again".into(),
            ));
        }
        Ok((entry.provenance.clone(), set.clone(), entry.interrupted))
    }
    pub fn reference(
        &self,
        session: SessionId,
        connection: ConnectionId,
        cursor: CursorId,
    ) -> Option<Uuid> {
        let mut entries = self.entries.lock().unwrap();
        reap(&mut entries);
        entries
            .iter()
            .filter(|(_, entry)| {
                entry.provenance.session == session
                    && entry.provenance.connection == connection
                    && entry.provenance.cursor == cursor
            })
            .max_by_key(|(_, entry)| entry.created)
            .map(|(id, _)| *id)
    }
    pub fn remove(&self, id: Uuid) {
        self.entries.lock().unwrap().remove(&id);
    }
    pub fn close_session(&self, session: SessionId) {
        self.entries
            .lock()
            .unwrap()
            .retain(|_, entry| entry.provenance.session != session);
    }
    pub fn close_connection(&self, session: SessionId, connection: ConnectionId) {
        self.entries.lock().unwrap().retain(|_, entry| {
            entry.provenance.session != session || entry.provenance.connection != connection
        });
    }
}
fn append_header(entry: &mut Entry, columns: &[ColumnMetadata]) {
    entry.current_set = None;
    if entry.sets.len() >= MAX_SETS {
        return;
    }
    let index = entry.sets.len();
    entry.sets.push(None);
    if columns.len() > 512 {
        return;
    }
    let Some(size) = bounded_json_size(&columns, MAX_RESULT_BYTES.saturating_sub(entry.bytes))
    else {
        return;
    };
    let cost = size.saturating_add(
        columns
            .len()
            .saturating_mul(std::mem::size_of::<ColumnMetadata>()),
    );
    if entry.bytes.saturating_add(cost) > MAX_RESULT_BYTES {
        return;
    }
    entry.current_set = Some(index);
    entry.bytes += cost;
    entry.sets[index] = Some(AiResultSet {
        schema_digest: crate::comparison::schema_digest(columns),
        columns: columns.to_vec(),
        rows: Vec::new(),
        rows_seen: 0,
        truncated: false,
    });
}
fn append_rows(entry: &mut Entry, rows: &[Row]) {
    let Some(set) = entry
        .current_set
        .and_then(|index| entry.sets.get_mut(index))
        .and_then(Option::as_mut)
    else {
        return;
    };
    set.rows_seen = set.rows_seen.saturating_add(rows.len());
    for row in rows {
        if set.truncated {
            break;
        }
        if set.rows.len() >= MAX_ROWS || row.len() != set.columns.len() {
            set.truncated = true;
            break;
        }
        let remaining = MAX_RESULT_BYTES.saturating_sub(entry.bytes);
        let mut json_cost = 0usize;
        for value in &row.values {
            if let sift_protocol::Value::Json(value) = value {
                let Some(cost) =
                    json_structural_cost(value, remaining.saturating_sub(json_cost), 0)
                else {
                    set.truncated = true;
                    break;
                };
                json_cost = json_cost.saturating_add(cost);
            }
        }
        if set.truncated {
            break;
        }
        let Some(size) = bounded_json_size(row, remaining) else {
            set.truncated = true;
            break;
        };
        let cost = size
            .saturating_add(json_cost)
            .saturating_add(
                row.len()
                    .saturating_mul(std::mem::size_of::<sift_protocol::Value>()),
            )
            .saturating_add(std::mem::size_of::<Row>());
        if cost > remaining {
            set.truncated = true;
            break;
        }
        set.rows.push(row.clone());
        entry.bytes += cost;
    }
}
// Serialized size covers string payloads; charge JSON containers separately.
// Inspect depth before serde/clone so a native deeply nested value cannot poison
// the registry mutex or turn a compact JSON body into uncharged retained heap.
fn json_structural_cost(value: &serde_json::Value, limit: usize, depth: usize) -> Option<usize> {
    if depth > 64 {
        return None;
    }
    let base = std::mem::size_of::<serde_json::Value>();
    let mut cost = base;
    match value {
        serde_json::Value::Array(values) => {
            if values.len().saturating_mul(base).saturating_add(cost) > limit {
                return None;
            }
            for value in values {
                cost = cost.checked_add(json_structural_cost(
                    value,
                    limit.saturating_sub(cost),
                    depth + 1,
                )?)?;
            }
        }
        serde_json::Value::Object(values) => {
            // Conservative BTreeMap node/key overhead in addition to child values.
            cost = cost.checked_add(values.len().checked_mul(64)?)?;
            if cost > limit {
                return None;
            }
            for value in values.values() {
                cost = cost.checked_add(json_structural_cost(
                    value,
                    limit.saturating_sub(cost),
                    depth + 1,
                )?)?;
            }
        }
        _ => {}
    }
    (cost <= limit).then_some(cost)
}

fn reap(entries: &mut HashMap<Uuid, Entry>) {
    entries.retain(|_, entry| entry.created.elapsed() < TTL);
}
fn evict_oldest(
    entries: &mut HashMap<Uuid, Entry>,
    session: Option<SessionId>,
    preserve: Option<Uuid>,
) {
    let oldest = entries
        .iter()
        .filter(|(id, entry)| {
            Some(**id) != preserve
                && session.map_or(true, |session| entry.provenance.session == session)
        })
        .min_by_key(|(_, entry)| entry.created)
        .map(|(id, _)| *id);
    if let Some(id) = oldest {
        entries.remove(&id);
    }
}
fn enforce_bytes(entries: &mut HashMap<Uuid, Entry>, preserve: Uuid) {
    while entries.values().map(|entry| entry.bytes).sum::<usize>() > MAX_BYTES {
        let before = entries.len();
        evict_oldest(entries, None, Some(preserve));
        if entries.len() == before {
            entries.remove(&preserve);
            break;
        }
    }
}
// Counts encoding without allocating a potentially huge native value.
pub(crate) fn bounded_json_size(value: &impl serde::Serialize, limit: usize) -> Option<usize> {
    struct Counter {
        count: usize,
        limit: usize,
    }
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.count) {
                return Err(io::Error::other("AI excerpt byte limit"));
            }
            self.count += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { count: 0, limit };
    serde_json::to_writer(&mut counter, value).ok()?;
    Some(counter.count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::{Nullability, PrimitiveType, TypeRef, Value};
    fn provenance(session: u64) -> AiResultProvenance {
        AiResultProvenance {
            actor: PrincipalId(1),
            tenant: TenantId(1),
            profile: ConnectionProfileId(1),
            session: SessionId(session),
            connection: ConnectionId(1),
            cursor: CursorId(1),
            sql: "SELECT $1".into(),
            sql_truncated: false,
            retained_until: chrono::Utc::now(),
        }
    }
    fn columns(name: String) -> Vec<ColumnMetadata> {
        vec![ColumnMetadata {
            name,
            type_ref: TypeRef::Primitive(PrimitiveType::Text),
            nullable: Nullability::Nullable,
            auto_increment: false,
            primary_key: false,
            facets: Default::default(),
        }]
    }
    #[test]
    fn exact_executions_survive_cursor_reuse_and_keep_result_set_ordinals() {
        let registry = AiResultRegistry::default();
        let id = registry.start(provenance(1));
        registry.observe(
            id,
            &Page::NextResult {
                columns: columns("x".repeat(MAX_RESULT_BYTES + 1)),
            },
        );
        let cols = columns("safe".into());
        let digest = crate::comparison::schema_digest(&cols);
        registry.observe(
            id,
            &Page::NextResult {
                columns: cols.clone(),
            },
        );
        registry.observe(
            id,
            &Page::Rows {
                rows: (0..MAX_ROWS + 10)
                    .map(|index| Row::new(vec![Value::Text(index.to_string())]))
                    .collect(),
            },
        );
        assert!(registry.get(PrincipalId(1), id, 1, &digest).is_err());
        registry.observe(
            id,
            &Page::Done {
                affected_rows: None,
                warnings: vec![],
            },
        );
        assert!(registry.get(PrincipalId(1), id, 0, &digest).is_err());
        assert!(registry.get(PrincipalId(2), id, 1, &digest).is_err());
        assert!(registry.get(PrincipalId(1), id, 1, "wrong").is_err());
        let (source, set, interrupted) = registry.get(PrincipalId(1), id, 1, &digest).unwrap();
        assert_eq!(source.sql, "SELECT $1");
        assert_eq!(set.rows.len(), MAX_ROWS);
        assert_eq!(set.rows_seen, MAX_ROWS + 10);
        assert!(set.truncated && !interrupted);
        let later = registry.start(provenance(1));
        assert_ne!(id, later);
        registry.observe(later, &Page::NextResult { columns: cols });
        registry.observe(
            later,
            &Page::Rows {
                rows: vec![Row::new(vec![Value::Text("later".into())])],
            },
        );
        registry.finish(later);
        registry.observe(
            id,
            &Page::Rows {
                rows: vec![Row::new(vec![Value::Text(
                    "cannot mutate completed source".into(),
                )])],
            },
        );
        assert_eq!(
            serde_json::to_value(registry.get(PrincipalId(1), id, 1, &digest).unwrap().1.rows)
                .unwrap(),
            serde_json::to_value(set.rows).unwrap()
        );
        assert_eq!(
            registry.reference(SessionId(1), ConnectionId(1), CursorId(1)),
            Some(later)
        );
    }
    #[test]
    fn json_container_heap_and_depth_are_bounded_before_retention() {
        let registry = AiResultRegistry::default();
        let cols = columns("json".into());
        let digest = crate::comparison::schema_digest(&cols);
        for value in [
            serde_json::Value::Array(vec![serde_json::Value::Null; 20_000]),
            (0..70).fold(serde_json::Value::Null, |value, _| {
                serde_json::Value::Array(vec![value])
            }),
        ] {
            let id = registry.start(provenance(1));
            registry.observe(
                id,
                &Page::NextResult {
                    columns: cols.clone(),
                },
            );
            registry.observe(
                id,
                &Page::Rows {
                    rows: vec![Row::new(vec![Value::Json(value)])],
                },
            );
            registry.finish(id);
            let (_, set, _) = registry.get(PrincipalId(1), id, 0, &digest).unwrap();
            assert!(set.rows.is_empty());
            assert!(set.truncated);
        }
        assert!(json_structural_cost(&serde_json::json!({"a":[1,2,3]}), 4096, 0).is_some());
    }

    #[test]
    fn bounds_expiry_and_owner_cleanup_are_explicit() {
        let registry = AiResultRegistry::default();
        let cols = columns("bounded".into());
        let digest = crate::comparison::schema_digest(&cols);
        let mut ids = Vec::new();
        for _ in 0..MAX_PER_SESSION + 1 {
            let id = registry.start(provenance(1));
            ids.push(id);
            registry.observe(
                id,
                &Page::NextResult {
                    columns: cols.clone(),
                },
            );
            registry.observe(
                id,
                &Page::Rows {
                    rows: vec![Row::new(vec![Value::Text(
                        "x".repeat(MAX_RESULT_BYTES + 1),
                    )])],
                },
            );
            registry.finish(id);
        }
        assert!(registry.get(PrincipalId(1), ids[0], 0, &digest).is_err());
        let (_, set, interrupted) = registry.get(PrincipalId(1), ids[1], 0, &digest).unwrap();
        assert!(set.rows.is_empty() && set.truncated && interrupted);
        assert!(registry
            .entries
            .lock()
            .unwrap()
            .values()
            .all(|entry| entry.bytes <= MAX_RESULT_BYTES));
        let other = registry.start(provenance(2));
        registry.observe(other, &Page::NextResult { columns: cols });
        registry.finish(other);
        registry.close_session(SessionId(1));
        assert!(registry.get(PrincipalId(1), other, 0, &digest).is_ok());
        registry
            .entries
            .lock()
            .unwrap()
            .get_mut(&other)
            .unwrap()
            .created = Instant::now() - TTL;
        assert!(registry.get(PrincipalId(1), other, 0, &digest).is_err());
    }
}
