//! Transient, room-scoped query results.
//!
//! A shared result is an immutable sequence of protocol pages. Readers keep
//! their own `from_seq`, so one observer can never advance or stall another.
//! Entries are process-local by design and therefore all expire on restart.

use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use dashmap::DashMap;
use sift_protocol::{
    Page, RoomQueryResult, RoomQueryStatus, RoomResultId, RoomResultPage, RoomResultPages,
};

use crate::error::{ApiError, ApiResult};

const DEFAULT_MAX_RESULTS_PER_ROOM: usize = 32;
const DEFAULT_RESULT_TTL: Duration = Duration::from_secs(600);
const DEFAULT_RESULT_MEMORY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
pub struct RoomResultRegistry {
    inner: Arc<Inner>,
}

struct Inner {
    entries: DashMap<RoomResultId, Arc<Entry>>,
    spill_key: [u8; 32],
}

struct Entry {
    reference: RoomQueryResult,
    pages: Vec<StoredPage>,
    last_accessed: std::sync::Mutex<Instant>,
    _retention_guards: Vec<crate::resources::ResourceGuard>,
}

enum StoredPage {
    Memory(Page),
    Spill {
        path: tempfile::TempPath,
        nonce: [u8; 12],
    },
}

pub struct NewRoomResult {
    pub room_id: i64,
    pub actor_principal_id: i64,
    pub connection_profile_id: Option<i64>,
    pub pages: Vec<Page>,
    pub row_count: Option<i64>,
    pub error_message: Option<String>,
    pub retention_guards: Vec<crate::resources::ResourceGuard>,
}

impl Default for RoomResultRegistry {
    fn default() -> Self {
        let mut spill_key = [0u8; 32];
        getrandom::getrandom(&mut spill_key).expect("OS RNG is available");
        Self {
            inner: Arc::new(Inner {
                entries: DashMap::new(),
                spill_key,
            }),
        }
    }
}

impl RoomResultRegistry {
    pub fn insert(&self, result: NewRoomResult) -> RoomQueryResult {
        let NewRoomResult {
            room_id,
            actor_principal_id,
            connection_profile_id,
            pages,
            mut row_count,
            error_message,
            mut retention_guards,
        } = result;
        self.reap_expired();
        self.enforce_room_cap(room_id);
        let schema_digests = pages
            .iter()
            .filter_map(|page| match page {
                Page::NextResult { columns } => Some(crate::comparison::schema_digest(columns)),
                _ => None,
            })
            .collect();
        let mut memory_bytes = 0usize;
        let pages = pages
            .into_iter()
            .map(|page| {
                let encoded = serde_json::to_vec(&page)
                    .map_err(|error| ApiError::Internal(error.to_string()))?;
                if memory_bytes.saturating_add(encoded.len()) <= DEFAULT_RESULT_MEMORY_BYTES {
                    memory_bytes = memory_bytes.saturating_add(encoded.len());
                    Ok(StoredPage::Memory(page))
                } else {
                    self.spill_page(&encoded)
                }
            })
            .collect::<ApiResult<Vec<_>>>();
        let (pages, spill_error) = match pages {
            Ok(pages) => (pages, error_message),
            Err(error) => {
                retention_guards.clear();
                row_count = None;
                (Vec::new(), Some(error.to_string()))
            }
        };
        let page_count = pages.len() as u64;
        let now = chrono::Utc::now();
        let reference = RoomQueryResult {
            result_id: RoomResultId(uuid::Uuid::new_v4()),
            room_id,
            actor_principal_id,
            connection_profile_id,
            row_count,
            page_count,
            schema_digests,
            status: if spill_error.is_some() {
                RoomQueryStatus::Error
            } else {
                RoomQueryStatus::Ok
            },
            error_message: spill_error,
            created_at: now,
            finished_at: Some(now),
        };
        self.inner.entries.insert(
            reference.result_id,
            Arc::new(Entry {
                reference: reference.clone(),
                pages,
                last_accessed: std::sync::Mutex::new(Instant::now()),
                _retention_guards: retention_guards,
            }),
        );
        reference
    }

    pub fn list(&self, room_id: i64) -> Vec<RoomQueryResult> {
        self.reap_expired();
        let mut results: Vec<_> = self
            .inner
            .entries
            .iter()
            .filter(|entry| entry.reference.room_id == room_id)
            .map(|entry| entry.reference.clone())
            .collect();
        results.sort_by_key(|result| result.created_at);
        results
    }

    pub fn get(&self, room_id: i64, result_id: RoomResultId) -> ApiResult<RoomQueryResult> {
        self.reap_expired();
        self.with_entry(room_id, result_id, |entry| entry.reference.clone())
    }

    pub fn pages(
        &self,
        room_id: i64,
        result_id: RoomResultId,
        from_seq: u64,
        limit: usize,
    ) -> ApiResult<RoomResultPages> {
        self.reap_expired();
        self.with_entry(room_id, result_id, |entry| {
            let start = usize::try_from(from_seq).unwrap_or(usize::MAX);
            if start > entry.pages.len() {
                return Err(ApiError::BadRequest(format!(
                    "from_seq={from_seq} is beyond result page count {}",
                    entry.pages.len()
                )));
            }
            let end = start
                .saturating_add(limit.clamp(1, 256))
                .min(entry.pages.len());
            let pages = entry.pages[start..end]
                .iter()
                .enumerate()
                .map(|(offset, page)| {
                    Ok(RoomResultPage {
                        seq: (start + offset) as u64,
                        page: self.read_page(page)?,
                    })
                })
                .collect::<ApiResult<Vec<_>>>()?;
            Ok(RoomResultPages {
                result_id,
                pages,
                next_seq: end as u64,
                done: end == entry.pages.len(),
            })
        })?
    }

    /// Materialize one immutable result set for a bounded comparison. The
    /// underlying pages remain retained and independently readable.
    pub fn comparison_dataset(
        &self,
        room_id: i64,
        result_id: RoomResultId,
        result_set: u32,
        expected_schema_digest: &str,
    ) -> ApiResult<sift_core::comparison::ComparisonDataset> {
        self.reap_expired();
        self.with_entry(room_id, result_id, |entry| {
            if entry.reference.status != RoomQueryStatus::Ok {
                return Err(ApiError::BadRequest(format!(
                    "room result {result_id} did not complete successfully"
                )));
            }
            let mut current_result: Option<u32> = None;
            let mut columns = None;
            let mut rows = Vec::new();
            for page in &entry.pages {
                match self.read_page(page)? {
                    Page::NextResult {
                        columns: next_columns,
                    } => {
                        current_result = Some(current_result.map_or(0u32, |value| value + 1));
                        if current_result == Some(result_set) {
                            columns = Some(next_columns);
                        }
                    }
                    Page::Rows { rows: next_rows } if current_result == Some(result_set) => {
                        rows.extend(next_rows);
                    }
                    Page::Error { error } => return Err(ApiError::Driver(error)),
                    Page::Rows { .. } | Page::Done { .. } => {}
                }
            }
            let columns = columns.ok_or_else(|| {
                ApiError::BadRequest(format!(
                    "room result {result_id} has no result_set={result_set}"
                ))
            })?;
            if crate::comparison::schema_digest(&columns) != expected_schema_digest {
                return Err(ApiError::BadRequest(format!(
                    "stale room-result schema digest for result {result_id}"
                )));
            }
            Ok(sift_core::comparison::ComparisonDataset {
                columns,
                rows,
                immutable_order: true,
            })
        })?
    }

    pub fn remove_room(&self, room_id: i64) {
        self.inner
            .entries
            .retain(|_, entry| entry.reference.room_id != room_id);
    }

    fn with_entry<T>(
        &self,
        room_id: i64,
        result_id: RoomResultId,
        f: impl FnOnce(&Entry) -> T,
    ) -> ApiResult<T> {
        let entry = self.inner.entries.get(&result_id).ok_or_else(|| {
            ApiError::BadRequest(format!("room result {result_id} was not found or expired"))
        })?;
        if entry.reference.room_id != room_id {
            return Err(ApiError::BadRequest(format!(
                "room result {result_id} was not found or expired"
            )));
        }
        *entry.last_accessed.lock().unwrap() = Instant::now();
        Ok(f(&entry))
    }

    fn spill_page(&self, encoded: &[u8]) -> ApiResult<StoredPage> {
        let mut nonce = [0u8; 12];
        getrandom::getrandom(&mut nonce)
            .map_err(|error| ApiError::Internal(format!("room result nonce: {error}")))?;
        let cipher = ChaCha20Poly1305::new((&self.inner.spill_key).into());
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), encoded)
            .map_err(|_| ApiError::Internal("encrypting room result spill failed".into()))?;
        let mut file = tempfile::NamedTempFile::new()
            .map_err(|error| ApiError::Internal(format!("creating room result spill: {error}")))?;
        file.write_all(&ciphertext)
            .map_err(|error| ApiError::Internal(format!("writing room result spill: {error}")))?;
        Ok(StoredPage::Spill {
            path: file.into_temp_path(),
            nonce,
        })
    }

    fn read_page(&self, page: &StoredPage) -> ApiResult<Page> {
        match page {
            StoredPage::Memory(page) => Ok(page.clone()),
            StoredPage::Spill { path, nonce } => {
                let ciphertext = std::fs::read(path).map_err(|error| {
                    ApiError::Internal(format!("reading room result spill: {error}"))
                })?;
                let cipher = ChaCha20Poly1305::new((&self.inner.spill_key).into());
                let encoded = cipher
                    .decrypt(Nonce::from_slice(nonce), ciphertext.as_ref())
                    .map_err(|_| {
                        ApiError::Internal("decrypting room result spill failed".into())
                    })?;
                serde_json::from_slice(&encoded).map_err(|error| {
                    ApiError::Internal(format!("decoding room result page: {error}"))
                })
            }
        }
    }

    fn reap_expired(&self) {
        self.inner
            .entries
            .retain(|_, entry| entry.last_accessed.lock().unwrap().elapsed() < DEFAULT_RESULT_TTL);
    }

    fn enforce_room_cap(&self, room_id: i64) {
        let mut room_entries: Vec<_> = self
            .inner
            .entries
            .iter()
            .filter(|entry| entry.reference.room_id == room_id)
            .map(|entry| (entry.reference.created_at, *entry.key()))
            .collect();
        if room_entries.len() < DEFAULT_MAX_RESULTS_PER_ROOM {
            return;
        }
        room_entries.sort_by_key(|(created_at, _)| *created_at);
        let remove_count = room_entries.len() + 1 - DEFAULT_MAX_RESULTS_PER_ROOM;
        for (_, result_id) in room_entries.into_iter().take(remove_count) {
            self.inner.entries.remove(&result_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use sift_protocol::{Row, Value};

    use super::*;

    #[test]
    fn readers_page_independently() {
        let registry = RoomResultRegistry::default();
        let result = registry.insert(NewRoomResult {
            room_id: 7,
            actor_principal_id: 3,
            connection_profile_id: Some(2),
            pages: vec![
                Page::Rows {
                    rows: vec![Row::new(vec![Value::Int64(1)])],
                },
                Page::Done {
                    affected_rows: None,
                    warnings: vec![],
                },
            ],
            row_count: Some(1),
            error_message: None,
            retention_guards: Vec::new(),
        });
        let first = registry.pages(7, result.result_id, 0, 1).unwrap();
        let again = registry.pages(7, result.result_id, 0, 1).unwrap();
        assert_eq!(first.next_seq, 1);
        assert_eq!(again.next_seq, 1);
        assert!(!first.done);
    }

    #[test]
    fn cross_room_reads_are_denied() {
        let registry = RoomResultRegistry::default();
        let result = registry.insert(NewRoomResult {
            room_id: 7,
            actor_principal_id: 3,
            connection_profile_id: None,
            pages: vec![],
            row_count: Some(0),
            error_message: None,
            retention_guards: Vec::new(),
        });
        assert!(matches!(
            registry.get(8, result.result_id),
            Err(ApiError::BadRequest(_))
        ));
    }
}
