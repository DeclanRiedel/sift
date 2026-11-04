//! Atomic in-place restore using a pinned read-only backup snapshot.
use super::*;

pub(crate) fn source(
    path: &std::path::Path,
    cancel: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
    deadline: std::time::Instant,
) -> Result<Connection, DriverError> {
    use std::io::Read;
    let mut magic = [0; 16];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut magic))
        .map_err(|_| invalid())?;
    if &magic != b"SQLite format 3\0" {
        return Err(invalid());
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE,
    )
    .map_err(|_| invalid())?;
    conn.busy_timeout(Duration::from_millis(100))
        .map_err(db_error)?;
    let progress_cancel = cancel.clone();
    let progress_closing = closing.clone();
    conn.progress_handler(
        1000,
        Some(move || {
            progress_cancel.load(Ordering::Acquire)
                || progress_closing.load(Ordering::Acquire)
                || std::time::Instant::now() >= deadline
        }),
    );
    conn.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA query_only=ON; BEGIN;")
        .map_err(|_| invalid())?;
    let check = conn
        .query_row("PRAGMA main.quick_check(1)", [], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|_| {
            if cancel.load(Ordering::Acquire)
                || closing.load(Ordering::Acquire)
                || std::time::Instant::now() >= deadline
            {
                error(
                    Code::QueryCanceled,
                    "SQLite restore verification canceled or timed out",
                )
            } else {
                invalid()
            }
        })?;
    if check != "ok" {
        return Err(invalid());
    }
    Ok(conn)
}
fn invalid() -> DriverError {
    error(Code::InvalidParameterValue, "SQLite restore backup is not a readable, valid SQLite database or verification was interrupted")
}

pub(crate) fn copy(
    source: &Connection,
    destination: &mut Connection,
    mut stop: impl FnMut() -> bool,
) -> Result<(), DriverError> {
    let backup = rusqlite::backup::Backup::new(source, destination).map_err(db_error)?;
    loop {
        if stop() {
            return Err(error(
                Code::QueryCanceled,
                "SQLite restore canceled or exceeded 120 seconds",
            ));
        }
        match backup.step(128).map_err(db_error)? {
            rusqlite::backup::StepResult::Done => return Ok(()),
            rusqlite::backup::StepResult::More => {}
            rusqlite::backup::StepResult::Busy | rusqlite::backup::StepResult::Locked => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => {
                return Err(error(
                    Code::DriverInternal,
                    "unexpected SQLite restore state",
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mid_copy_cancellation_rolls_back_the_existing_database() {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("archive.db");
        let source_db = Connection::open(&archive).unwrap();
        source_db.execute_batch("CREATE TABLE restored(id INTEGER PRIMARY KEY, data BLOB); INSERT INTO restored VALUES(1,zeroblob(4000000));").unwrap();
        drop(source_db);
        let flag = Arc::new(AtomicBool::new(false));
        let source = source(
            &archive,
            flag.clone(),
            flag,
            std::time::Instant::now() + Duration::from_secs(10),
        )
        .unwrap();
        let mut destination = Connection::open(root.path().join("destination.db")).unwrap();
        destination
            .execute_batch(
                "PRAGMA journal_mode=WAL; CREATE TABLE original(value TEXT); INSERT INTO original VALUES('kept');",
            )
            .unwrap();
        let mut checks = 0;
        let result = copy(&source, &mut destination, || {
            checks += 1;
            checks > 1
        });
        assert!(matches!(result, Err(error) if error.code == Code::QueryCanceled));
        assert_eq!(checks, 2, "cancel after copying the first 128 pages");
        assert!(destination.is_autocommit());
        assert_eq!(
            destination
                .query_row("SELECT value FROM original", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "kept"
        );
        assert!(destination.prepare("SELECT * FROM restored").is_err());
        assert_eq!(
            destination
                .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        let incompatible = Connection::open(root.path().join("large-pages.db")).unwrap();
        incompatible
            .execute_batch("PRAGMA page_size=8192; CREATE TABLE incompatible(value TEXT);")
            .unwrap();
        assert!(
            copy(&incompatible, &mut destination, || false).is_err(),
            "WAL destinations cannot silently change page size"
        );
        assert_eq!(
            destination
                .query_row("SELECT value FROM original", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "kept"
        );
        copy(&source, &mut destination, || false).unwrap();
        assert_eq!(
            destination
                .query_row("SELECT length(data) FROM restored", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            4000000
        );
        assert!(destination.prepare("SELECT * FROM original").is_err());
    }
}
