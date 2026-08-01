use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tracedecay_application::DirectorySyncPolicy;

use tracedecay_runtime_core::db::engine::{QueryExecutor, params};

use super::{LCM_SCAN_PAGE_ROWS, LcmError};

#[derive(Clone, Copy)]
pub(super) enum BackupKind {
    Clean,
    Gc,
}

impl BackupKind {
    fn name(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Gc => "gc",
        }
    }
}

static BACKUP_NONCE: AtomicU64 = AtomicU64::new(0);

pub(super) async fn backup_database(
    db_path: &Path,
    storage_root: &Path,
    kind: BackupKind,
) -> Result<Value, LcmError> {
    let backup_dir = storage_root.join("lcm-clean-backups");
    fs::create_dir_all(&backup_dir).map_err(|err| LcmError::Io(err.to_string()))?;
    let (staging_dir, published_dir) = allocate_backup_directory(&backup_dir, kind)?;
    let staging_path = staging_dir.join("sessions.db");
    let backup_path = published_dir.join("sessions.db");
    let result = async {
        tracedecay_runtime_core::sqlite_read_snapshot::backup_live_sqlite_database(db_path, &staging_path)
            .await
            .map_err(|error| LcmError::Io(error.to_string()))?;
        sync_file(&staging_path)?;
        let byte_count = fs::metadata(&staging_path)
            .map_err(|error| LcmError::Io(error.to_string()))?
            .len();
        verify_sqlite_backup(&staging_path)?;
        sync_directory(&staging_dir)?;
        fs::rename(&staging_dir, &published_dir).map_err(|err| LcmError::Io(err.to_string()))?;
        sync_directory(&backup_dir)?;
        Ok(byte_count)
    }
    .await;
    let byte_count = match result {
        Ok(byte_count) => byte_count,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging_dir);
            return Err(error);
        }
    };
    Ok(json!({
        "ok": true,
        "path": backup_path,
        "byte_count": byte_count,
    }))
}

fn allocate_backup_directory(
    backup_root: &Path,
    kind: BackupKind,
) -> Result<(PathBuf, PathBuf), LcmError> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    loop {
        let nonce = BACKUP_NONCE.fetch_add(1, Ordering::Relaxed);
        let name = format!(
            "sessions-{}-{timestamp}-{}-{nonce}",
            kind.name(),
            std::process::id()
        );
        let published = backup_root.join(&name);
        if published.exists() {
            continue;
        }
        let staging = backup_root.join(format!(".{name}.tmp"));
        match fs::create_dir(&staging) {
            Ok(()) => return Ok((staging, published)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(LcmError::Io(error.to_string())),
        }
    }
}

fn verify_sqlite_backup(path: &Path) -> Result<(), LcmError> {
    tracedecay_rusqlite_runtime::backup::verify_sqlite_snapshot(path)
        .map_err(|error| LcmError::Db(error.to_string()))
}

fn sync_file(path: &Path) -> Result<(), LcmError> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| LcmError::Io(error.to_string()))
}

fn sync_directory(path: &Path) -> Result<(), LcmError> {
    tracedecay_application::sync_directory(path, DirectorySyncPolicy::Strict)
        .map_err(|error| LcmError::Io(error.to_string()))
}

pub(super) async fn all_payload_metadata_refs(
    conn: &(impl QueryExecutor + ?Sized),
) -> Result<BTreeSet<String>, LcmError> {
    payload_metadata_refs(conn, "all", None).await
}

pub(super) async fn payload_metadata_refs_for_scope(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<BTreeSet<String>, LcmError> {
    payload_metadata_refs(conn, provider, session_id).await
}

/// Collects payload-metadata references through `rowid` keyset pages: the
/// whole `lcm_external_payloads` table exceeds what the `SQLite` runtime will
/// materialize for one query on a long-lived profile.
async fn payload_metadata_refs(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<BTreeSet<String>, LcmError> {
    let mut refs = BTreeSet::new();
    let mut after_rowid = 0_i64;
    loop {
        let mut rows = conn
            .query(
                "SELECT payload_ref, rowid
                 FROM lcm_external_payloads
                 WHERE (?1 = 'all' OR provider = ?1)
                   AND (?2 IS NULL OR session_id = ?2)
                   AND rowid > ?3
                 ORDER BY rowid
                 LIMIT ?4",
                params![provider, session_id, after_rowid, LCM_SCAN_PAGE_ROWS],
            )
            .await?;
        let mut page_rows = 0_i64;
        while let Some(row) = rows.next().await? {
            refs.insert(row.get(0)?);
            after_rowid = row.get(1)?;
            page_rows += 1;
        }
        drop(rows);
        if page_rows < LCM_SCAN_PAGE_ROWS {
            return Ok(refs);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_runtime_core::db::engine::TestConnection;
    use rusqlite::Connection as RusqliteConnection;

    #[tokio::test]
    async fn gc_backups_are_unique_verified_and_non_destructive() {
        let root = tempfile::tempdir().unwrap();
        let source_path = root.path().join("sessions.db");
        RusqliteConnection::open(&source_path)
            .unwrap()
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE messages(id INTEGER PRIMARY KEY, body TEXT NOT NULL);
                 INSERT INTO messages(body) VALUES ('retained');",
            )
            .unwrap();

        let first = backup_database(&source_path, root.path(), BackupKind::Gc)
            .await
            .unwrap();
        let second = backup_database(&source_path, root.path(), BackupKind::Gc)
            .await
            .unwrap();
        let first_path = PathBuf::from(first["path"].as_str().unwrap());
        let second_path = PathBuf::from(second["path"].as_str().unwrap());
        assert_ne!(first_path, second_path);
        assert!(first_path.is_file());
        assert!(second_path.is_file());

        let backup_conn = TestConnection::open(&first_path);
        let count: i64 = backup_conn
            .query("SELECT COUNT(*) FROM messages", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn invalid_backup_is_never_published() {
        let root = tempfile::tempdir().unwrap();
        let source_path = root.path().join("sessions.db");
        fs::write(&source_path, b"not sqlite").unwrap();

        assert!(
            backup_database(&source_path, root.path(), BackupKind::Gc)
                .await
                .is_err()
        );
        let backup_root = root.path().join("lcm-clean-backups");
        assert_eq!(fs::read_dir(backup_root).unwrap().count(), 0);
    }
}
