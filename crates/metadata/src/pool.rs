//! Bounded, non-waiting admission for file-backed SQLite connections.

use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::{configure_connection, MetadataError, Result};

const MAX_CONNECTIONS: usize = 16;

#[derive(Default)]
struct PoolState {
    idle: Vec<Connection>,
    checked_out: usize,
}

pub(super) struct ConnectionPool {
    pub(super) path: PathBuf,
    state: Mutex<PoolState>,
}

impl ConnectionPool {
    pub(super) fn new(path: PathBuf) -> Self {
        Self {
            path,
            state: Mutex::new(PoolState::default()),
        }
    }

    pub(super) fn checkout(self: &Arc<Self>) -> Result<PooledConn> {
        let mut state = self.state.lock().unwrap();
        if state.checked_out == MAX_CONNECTIONS {
            return Err(MetadataError::PoolExhausted);
        }
        state.checked_out += 1;
        let mut guard = PooledConn {
            conn: state.idle.pop(),
            pool: Arc::clone(self),
        };
        drop(state);
        // The guard releases admission even if opening/configuring SQLite fails.
        // Filesystem and database work never hold the pool mutex.
        if guard.conn.is_none() {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let conn = Connection::open(&self.path)?;
            configure_connection(&conn)?;
            guard.conn = Some(conn);
        }
        Ok(guard)
    }

    pub(super) fn clear_idle(&self) {
        let idle = std::mem::take(&mut self.state.lock().unwrap().idle);
        drop(idle);
    }
}

pub(super) struct PooledConn {
    conn: Option<Connection>,
    pool: Arc<ConnectionPool>,
}

impl Drop for PooledConn {
    fn drop(&mut self) {
        let mut state = self.pool.state.lock().unwrap();
        if let Some(conn) = self.conn.take() {
            state.idle.push(conn);
        }
        state.checked_out -= 1;
    }
}

impl Deref for PooledConn {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        self.conn.as_ref().expect("connection present until drop")
    }
}

impl DerefMut for PooledConn {
    fn deref_mut(&mut self) -> &mut Connection {
        self.conn.as_mut().expect("connection present until drop")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhaustion_release_and_reuse() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(ConnectionPool::new(dir.path().join("metadata.sqlite")));
        let mut held: Vec<_> = (0..MAX_CONNECTIONS)
            .map(|_| pool.checkout().unwrap())
            .collect();
        assert!(matches!(pool.checkout(), Err(MetadataError::PoolExhausted)));
        held.pop();
        assert_eq!(pool.state.lock().unwrap().idle.len(), 1);
        let last = pool.checkout().unwrap();
        assert!(pool.state.lock().unwrap().idle.is_empty());
        drop((held, last));
        assert_eq!(pool.state.lock().unwrap().checked_out, 0);
        pool.clear_idle();
        assert!(pool.state.lock().unwrap().idle.is_empty());
        assert!(pool.checkout().is_ok());
    }

    #[test]
    fn failed_open_releases_admission() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.sqlite");
        std::fs::create_dir(&path).unwrap();
        let pool = Arc::new(ConnectionPool::new(path.clone()));
        for _ in 0..MAX_CONNECTIONS + 1 {
            assert!(matches!(pool.checkout(), Err(MetadataError::Sqlite(_))));
        }
        assert_eq!(pool.state.lock().unwrap().checked_out, 0);
        std::fs::remove_dir(path).unwrap();
        assert!(pool.checkout().is_ok());
    }

    #[test]
    fn concurrent_admission_never_exceeds_cap() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(ConnectionPool::new(dir.path().join("metadata.sqlite")));
        // Initialize WAL before the simultaneous checkouts.
        drop(pool.checkout().unwrap());
        let barrier = std::sync::Barrier::new(MAX_CONNECTIONS * 2);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..MAX_CONNECTIONS * 2)
                .map(|_| {
                    scope.spawn(|| {
                        let conn = pool.checkout();
                        barrier.wait();
                        match conn {
                            Ok(_) => true,
                            Err(MetadataError::PoolExhausted) => false,
                            Err(error) => panic!("unexpected checkout error: {error}"),
                        }
                    })
                })
                .collect();
            let admitted = handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .filter(|admitted| *admitted)
                .count();
            assert_eq!(admitted, MAX_CONNECTIONS);
        });
    }
}
