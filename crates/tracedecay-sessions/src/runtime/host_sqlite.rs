use std::sync::{Arc, Mutex, PoisonError};

/// A plain SQLite read or a verified foreign read. The verified case keeps
/// the SQLite connection alive through the last cloned handle. Unix must not
/// retain an additional raw inode pin: closing it releases unrelated locks.
enum SqliteReadHandle {
    Plain(rusqlite::Connection),
    Verified(tracedecay_rusqlite_runtime::VerifiedReader),
}

impl SqliteReadHandle {
    fn connection(&self) -> &rusqlite::Connection {
        match self {
            Self::Plain(connection) => connection,
            Self::Verified(reader) => reader.connection(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct SqliteReadConn {
    inner: Arc<Mutex<SqliteReadHandle>>,
}

impl SqliteReadConn {
    pub(crate) fn new(connection: rusqlite::Connection) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SqliteReadHandle::Plain(connection))),
        }
    }

    pub(crate) fn new_verified(reader: tracedecay_rusqlite_runtime::VerifiedReader) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SqliteReadHandle::Verified(reader))),
        }
    }

    #[tracing::instrument(name = "sessions.host_sqlite.with", level = "trace", skip_all)]
    pub(crate) async fn with<T, F>(&self, body: F) -> Option<T>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Connection) -> T + Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
            let guard = inner.lock().unwrap_or_else(PoisonError::into_inner);
            body(guard.connection())
        })
        .await
        .ok()
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    use super::SqliteReadConn;

    fn descriptors_for(path: &Path) -> usize {
        let identity = std::fs::metadata(path).expect("database metadata");
        std::fs::read_dir("/proc/self/fd")
            .expect("process file descriptors")
            .filter_map(Result::ok)
            .filter(|entry| {
                std::fs::metadata(entry.path()).is_ok_and(|metadata| {
                    metadata.dev() == identity.dev() && metadata.ino() == identity.ino()
                })
            })
            .count()
    }

    #[test]
    fn verified_foreign_reader_keeps_its_sqlite_handle_until_last_clone_closes() {
        let dir = tempfile::tempdir().expect("test directory");
        let path = dir.path().join("foreign.sqlite3");
        {
            let conn = rusqlite::Connection::open(&path).expect("create database");
            conn.execute_batch(
                "CREATE TABLE sample(value INTEGER); INSERT INTO sample VALUES (42)",
            )
            .expect("seed database");
        }
        assert_eq!(descriptors_for(&path), 0);

        let verified = tracedecay_rusqlite_runtime::open_verified_immutable_reader(&path)
            .expect("open verified foreign database");
        let pinned_descriptors = descriptors_for(&path);
        assert_eq!(
            pinned_descriptors, 1,
            "verified Unix reads must not open a second raw descriptor to the SQLite inode"
        );

        let reader = SqliteReadConn::new_verified(verified);
        assert_eq!(descriptors_for(&path), pinned_descriptors);
        let other = reader.clone();
        drop(reader);
        assert_eq!(descriptors_for(&path), pinned_descriptors);
        let value: i64 = other
            .inner
            .lock()
            .expect("reader lock")
            .connection()
            .query_row("SELECT value FROM sample", [], |row| row.get(0))
            .expect("read verified database");
        assert_eq!(value, 42);

        drop(other);
        assert_eq!(descriptors_for(&path), 0);
    }
}
