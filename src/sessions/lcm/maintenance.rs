use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use libsql::{Connection, params};
use serde_json::{Value, json};

use super::LcmError;

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

    fn checkpoint_label(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Gc => "GC",
        }
    }
}

pub(super) fn backup_database(
    db_path: &Path,
    storage_root: &Path,
    kind: BackupKind,
) -> Result<Value, LcmError> {
    let backup_dir = storage_root.join("lcm-clean-backups");
    fs::create_dir_all(&backup_dir).map_err(|err| LcmError::Io(err.to_string()))?;
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let stamp = match kind {
        BackupKind::Clean => elapsed.as_millis(),
        BackupKind::Gc => u128::from(elapsed.as_secs()),
    };
    let backup_path = backup_dir.join(format!(
        "sessions-{}-{stamp}-{}.db",
        kind.name(),
        std::process::id()
    ));
    let byte_count = copy_sqlite_file_set(db_path, &backup_path)?;
    Ok(json!({
        "ok": true,
        "path": backup_path,
        "byte_count": byte_count,
    }))
}

fn copy_sqlite_file_set(db_path: &Path, backup_path: &Path) -> Result<u64, LcmError> {
    let mut byte_count =
        fs::copy(db_path, backup_path).map_err(|err| LcmError::Io(err.to_string()))?;
    // Copy only the WAL sidecar. The -shm file is rebuildable shared memory
    // that SQLite never reads from a backup, and its live byte-range locks
    // make plain file reads fail with ERROR_LOCK_VIOLATION (os error 33) on
    // Windows while any connection is open.
    let source = sqlite_sidecar_path(db_path, "-wal");
    if source.is_file() {
        let target = sqlite_sidecar_path(backup_path, "-wal");
        byte_count += fs::copy(&source, target).map_err(|err| LcmError::Io(err.to_string()))?;
    }
    Ok(byte_count)
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

pub(super) async fn checkpoint_wal_for_backup(
    conn: &Connection,
    kind: BackupKind,
) -> Result<(), LcmError> {
    let mut rows = conn.query("PRAGMA wal_checkpoint(TRUNCATE);", ()).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db("WAL checkpoint returned no status row".to_string()))?;
    let busy: i64 = row.get(0)?;
    let log_frames: i64 = row.get(1)?;
    let checkpointed_frames: i64 = row.get(2)?;
    if busy != 0 || checkpointed_frames < log_frames {
        return Err(LcmError::Db(format!(
            "WAL checkpoint incomplete before {} backup: busy={busy}, log_frames={log_frames}, checkpointed_frames={checkpointed_frames}",
            kind.checkpoint_label()
        )));
    }
    Ok(())
}

pub(super) async fn all_payload_metadata_refs(
    conn: &Connection,
) -> Result<BTreeSet<String>, LcmError> {
    let mut refs = BTreeSet::new();
    let mut rows = conn
        .query("SELECT payload_ref FROM lcm_external_payloads", ())
        .await?;
    while let Some(row) = rows.next().await? {
        refs.insert(row.get(0)?);
    }
    Ok(refs)
}

pub(super) async fn payload_metadata_refs_for_scope(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<BTreeSet<String>, LcmError> {
    let mut refs = BTreeSet::new();
    let mut rows = conn
        .query(
            "SELECT payload_ref
             FROM lcm_external_payloads
             WHERE (?1 = 'all' OR provider = ?1)
               AND (?2 IS NULL OR session_id = ?2)",
            params![provider, super::util::opt_text(session_id)],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        refs.insert(row.get(0)?);
    }
    Ok(refs)
}
