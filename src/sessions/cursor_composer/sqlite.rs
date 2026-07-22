//! Length-gated, strictly read-only `SQLite` access helpers for the Cursor
//! composer stores (`state.vscdb` KV lookups and `store.db` handles).

use std::path::Path;

use libsql::OpenFlags;
use serde_json::Value;

use crate::privacy::MAX_OBSERVATION_RECORD_BYTES;
use crate::sessions::source::MAX_JSONL_RECORD_BYTES;

/// `SQLITE_OPEN_URI` — not exposed by libsql's [`OpenFlags`], so we OR the raw
/// bit in (libsql forwards `flags.bits()` verbatim to `sqlite3_open_v2`). This
/// makes `SQLite` interpret the `file:…?immutable=1` URI filename.
const SQLITE_OPEN_URI: i32 = 0x0000_0040;

/// Outcome of a length-gated `SQLite` text/blob fetch that never materializes
/// oversized or over-budget payloads into `Rust`.
#[derive(Debug)]
pub(crate) enum BoundedSqliteValue<T> {
    Missing,
    Ready { byte_len: u64, value: T },
    Oversized { byte_len: u64 },
    BudgetExceeded { byte_len: u64 },
    Malformed { byte_len: u64 },
}

pub(crate) fn effective_sqlite_cap(max_bytes: u64, remaining: Option<u64>) -> u64 {
    match remaining {
        Some(remaining) => remaining.min(max_bytes),
        None => max_bytes,
    }
}

fn composer_payload_bytes(value: &Value) -> u64 {
    serde_json::to_vec(value)
        .ok()
        .and_then(|encoded| u64::try_from(encoded.len()).ok())
        .unwrap_or(u64::MAX)
}

pub(crate) fn max_composer_record_bytes() -> u64 {
    u64::try_from(MAX_OBSERVATION_RECORD_BYTES).unwrap_or(u64::MAX)
}

pub(crate) fn composer_source_charge(bytes: u64) -> u64 {
    bytes.min(max_composer_record_bytes().saturating_add(1))
}

pub(crate) fn composer_budget_bytes(value: &Value) -> u64 {
    composer_payload_bytes(value).min(max_composer_record_bytes().saturating_add(1))
}

pub(crate) fn composer_id_from_envelope_key(key: &str) -> Option<&str> {
    key.strip_prefix("composerData:")
        .filter(|id| !id.is_empty() && id.len() as u64 <= MAX_COMPOSER_SQLITE_KEY_BYTES)
}

/// Maximum bytes materializable for one `composerData:` session envelope.
/// Reuses the JSONL frame ceiling so long header lists stay within one
/// transcript-frame-sized allocation.
pub(crate) const MAX_COMPOSER_ENVELOPE_BYTES: u64 = MAX_JSONL_RECORD_BYTES as u64;

/// Default cumulative sweep ceiling: one maximum-size envelope plus the byte
/// needed by bounded readers to prove that a record crossed the ceiling.
pub(crate) const DEFAULT_COMPOSER_SWEEP_BYTES: u64 = MAX_COMPOSER_ENVELOPE_BYTES + 1;

/// Maximum UTF-8 bytes in one `SQLite` key / blob id.
pub(crate) const MAX_COMPOSER_SQLITE_KEY_BYTES: u64 = 512;

/// A read-only connection paired with its owning [`libsql::Database`] so the
/// underlying handle stays alive for the connection's lifetime.
pub(crate) struct ReadOnlyDb {
    _db: libsql::Database,
    pub(crate) conn: libsql::Connection,
}

/// Open a `SQLite` file strictly read-only and immutable (no locking, no
/// `-wal`/`-shm` writes) via a `file:…?immutable=1&mode=ro` URI.
pub(crate) async fn open_readonly_immutable(db_path: &Path) -> Option<ReadOnlyDb> {
    let uri = crate::sqlite_read_snapshot::immutable_uri(db_path).ok()?;
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::from_bits_retain(SQLITE_OPEN_URI);
    let db = crate::db::libsql_local::open_local_database_with_flags(std::path::Path::new(&uri), flags)
        .await
        .ok()?;
    let conn = db.connect().ok()?;
    // Belt-and-suspenders against ever mutating the live store.
    let _ = conn.execute_batch("PRAGMA query_only = ON;").await;
    Some(ReadOnlyDb { _db: db, conn })
}

pub(crate) async fn fetch_kv_text_bounded(
    conn: &libsql::Connection,
    key: &str,
    max_bytes: u64,
    remaining: Option<u64>,
) -> BoundedSqliteValue<String> {
    let effective_cap = effective_sqlite_cap(max_bytes, remaining);
    let Ok(mut rows) = conn
        .query(
            "SELECT length(CAST(value AS BLOB)) AS nbytes, \
             CASE WHEN length(CAST(value AS BLOB)) <= ?1 THEN value ELSE NULL END AS payload \
             FROM cursorDiskKV WHERE key = ?2",
            libsql::params![effective_cap as i64, key],
        )
        .await
    else {
        return BoundedSqliteValue::Missing;
    };
    let Ok(Some(row)) = rows.next().await else {
        return BoundedSqliteValue::Missing;
    };
    let Ok(nbytes_i) = row.get::<i64>(0) else {
        return BoundedSqliteValue::Missing;
    };
    if nbytes_i < 0 {
        return BoundedSqliteValue::Missing;
    }
    let byte_len = nbytes_i as u64;
    match row.get::<String>(1) {
        Ok(value) => BoundedSqliteValue::Ready { byte_len, value },
        Err(_) if byte_len > max_bytes => BoundedSqliteValue::Oversized { byte_len },
        Err(_) if remaining.is_some_and(|cap| byte_len > cap) => {
            BoundedSqliteValue::BudgetExceeded { byte_len }
        }
        Err(_) => BoundedSqliteValue::Missing,
    }
}

pub(crate) async fn fetch_bubble_bounded(
    conn: &libsql::Connection,
    composer_id: &str,
    bubble_id: &str,
    remaining: Option<u64>,
) -> BoundedSqliteValue<Value> {
    let key = format!("bubbleId:{composer_id}:{bubble_id}");
    if key.len() as u64 > MAX_COMPOSER_SQLITE_KEY_BYTES {
        return BoundedSqliteValue::Missing;
    }
    match fetch_kv_text_bounded(conn, &key, max_composer_record_bytes(), remaining).await {
        BoundedSqliteValue::Missing => BoundedSqliteValue::Missing,
        BoundedSqliteValue::Oversized { byte_len } => BoundedSqliteValue::Oversized { byte_len },
        BoundedSqliteValue::BudgetExceeded { byte_len } => {
            BoundedSqliteValue::BudgetExceeded { byte_len }
        }
        BoundedSqliteValue::Malformed { byte_len } => BoundedSqliteValue::Malformed { byte_len },
        BoundedSqliteValue::Ready { byte_len, value } => {
            match serde_json::from_str::<Value>(&value) {
                Ok(parsed) => BoundedSqliteValue::Ready {
                    byte_len,
                    value: parsed,
                },
                Err(_) => BoundedSqliteValue::Malformed { byte_len },
            }
        }
    }
}

pub(crate) fn envelope_project(envelope: &Value) -> Option<ComposerProject> {
    if let Some(uri) = envelope
        .get("workspaceIdentifier")
        .and_then(|w| w.get("uri"))
    {
        for key in ["fsPath", "path"] {
            if let Some(path) = uri
                .get(key)
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty())
            {
                return Some(ComposerProject {
                    path: path.to_string(),
                });
            }
        }
    }
    if let Some(repos) = envelope.get("trackedGitRepos").and_then(Value::as_array) {
        for repo in repos {
            if let Some(path) = repo
                .get("repoPath")
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty())
            {
                return Some(ComposerProject {
                    path: path.to_string(),
                });
            }
        }
    }
    None
}

pub(crate) fn workspace_hash(envelope: &Value) -> Option<String> {
    envelope
        .get("workspaceIdentifier")
        .and_then(|w| w.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() as u64 <= MAX_COMPOSER_SQLITE_KEY_BYTES)
        .map(str::to_string)
}

pub(crate) fn bubble_epoch(bubble: &Value, key: &str) -> Option<i64> {
    epoch_ms_to_secs(bubble.get(key).and_then(Value::as_i64))
}

pub(crate) fn epoch_ms_to_secs(ms: Option<i64>) -> Option<i64> {
    ms.filter(|v| *v > 0).map(|v| v / 1000)
}

/// Resolved project for a composer envelope.
pub(crate) struct ComposerProject {
    pub(crate) path: String,
}
