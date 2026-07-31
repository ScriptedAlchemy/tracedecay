//! Length-gated, strictly read-only `SQLite` access helpers for the Cursor
//! composer stores (`state.vscdb` KV lookups and `store.db` handles).
//!
//! S11: this module reads foreign Cursor databases through the bundled
//! rusqlite engine (immutable read-only URI opens). Every SQL call runs on a
//! blocking thread via [`CursorConn::with`], so async ingest callers never
//! block the executor and futures holding a [`CursorConn`] stay `Send`.

use std::path::Path;

use serde_json::Value;

use crate::privacy::MAX_OBSERVATION_RECORD_BYTES;
use crate::runtime::source::MAX_JSONL_RECORD_BYTES;

/// Outcome of a length-gated `SQLite` text/blob fetch that never materializes
/// oversized or over-budget payloads into `Rust`.
#[derive(Debug)]
pub enum BoundedSqliteValue<T> {
    Missing,
    Ready { byte_len: u64, value: T },
    Oversized { byte_len: u64 },
    BudgetExceeded { byte_len: u64 },
    Malformed { byte_len: u64 },
    Corrupt,
}

pub fn effective_sqlite_cap(max_bytes: u64, remaining: Option<u64>) -> u64 {
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

pub fn max_composer_record_bytes() -> u64 {
    u64::try_from(MAX_OBSERVATION_RECORD_BYTES).unwrap_or(u64::MAX)
}

pub fn composer_source_charge(bytes: u64) -> u64 {
    bytes.min(max_composer_record_bytes().saturating_add(1))
}

pub fn composer_budget_bytes(value: &Value) -> u64 {
    composer_payload_bytes(value).min(max_composer_record_bytes().saturating_add(1))
}

pub fn composer_id_from_envelope_key(key: &str) -> Option<&str> {
    key.strip_prefix("composerData:")
        .filter(|id| !id.is_empty() && id.len() as u64 <= MAX_COMPOSER_SQLITE_KEY_BYTES)
}

/// Maximum bytes materializable for one `composerData:` session envelope.
/// Reuses the JSONL frame ceiling so long header lists stay within one
/// transcript-frame-sized allocation.
pub const MAX_COMPOSER_ENVELOPE_BYTES: u64 = MAX_JSONL_RECORD_BYTES as u64;

/// Default cumulative sweep ceiling: one maximum-size envelope plus the byte
/// needed by bounded readers to prove that a record crossed the ceiling.
pub const DEFAULT_COMPOSER_SWEEP_BYTES: u64 = MAX_COMPOSER_ENVELOPE_BYTES + 1;

/// Maximum UTF-8 bytes in one `SQLite` key / blob id.
pub const MAX_COMPOSER_SQLITE_KEY_BYTES: u64 = 512;

/// Rows fetched per keyset-paginated `composerData:` key scan page. The scan
/// walks the `cursorDiskKV` primary key in order, so pagination reproduces the
/// original indexed prefix scan while keeping at most one page in memory.
pub const COMPOSER_KEY_SCAN_PAGE: usize = 1024;

/// Shared foreign-store read handle (see [`crate::runtime::shared`]),
/// aliased locally for the Cursor composer reader signatures.
pub use crate::runtime::shared::SqliteReadConn as CursorConn;

/// A read-only connection to a Cursor composer store.
pub struct ReadOnlyDb {
    pub conn: CursorConn,
}

/// Open a `SQLite` file strictly read-only and immutable (no locking, no
/// `-wal`/`-shm` writes) via a `file:…?immutable=1&mode=ro` URI. The runtime
/// helper also pins `busy_timeout = 0` and verifies `query_only = ON`.
pub async fn open_readonly_immutable(db_path: &Path) -> Option<ReadOnlyDb> {
    let path = db_path.to_path_buf();
    let conn = tokio::task::spawn_blocking(move || {
        tracedecay_rusqlite_runtime::open_immutable_reader(&path).ok()
    })
    .await
    .ok()??;
    Some(ReadOnlyDb {
        conn: CursorConn::new(conn),
    })
}

/// One keyset page of `composerData:` keys with their value byte lengths.
/// Passing `after = None` starts at the prefix lower bound; passing the last
/// key of the previous page continues the primary-key-ordered scan. Never
/// materializes envelope text — keys and byte lengths only.
pub async fn scan_composer_keys_page(
    conn: &CursorConn,
    after: Option<&str>,
    limit: usize,
) -> Result<Vec<(String, u64)>, String> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let after = after.map(str::to_string);
    conn.with(move |conn| {
        let (sql, lower) = match &after {
            Some(last) => (
                "SELECT key, length(CAST(value AS BLOB)) AS nbytes \
                 FROM cursorDiskKV \
                 WHERE key > ?1 AND key < 'composerData;' \
                   AND typeof(key) = 'text' AND value IS NOT NULL \
                   AND length(CAST(key AS BLOB)) <= ?2 \
                 ORDER BY key \
                 LIMIT ?3",
                last.as_str(),
            ),
            None => (
                "SELECT key, length(CAST(value AS BLOB)) AS nbytes \
                 FROM cursorDiskKV \
                 WHERE key >= ?1 AND key < 'composerData;' \
                   AND typeof(key) = 'text' AND value IS NOT NULL \
                   AND length(CAST(key AS BLOB)) <= ?2 \
                 ORDER BY key \
                 LIMIT ?3",
                "composerData:",
            ),
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|error| format!("could not prepare Cursor composer key scan: {error}"))?;
        let limit = i64::try_from(limit)
            .map_err(|_| "Cursor composer key scan limit is invalid".to_string())?;
        let rows = stmt
            .query_map(
                rusqlite::params![lower, MAX_COMPOSER_SQLITE_KEY_BYTES as i64, limit],
                |row| {
                    let key = row.get::<_, String>(0)?;
                    let nbytes = row.get::<_, i64>(1)?;
                    Ok((key, nbytes))
                },
            )
            .map_err(|error| format!("could not query Cursor composer keys: {error}"))?;
        rows.map(|row| {
            let (key, nbytes) =
                row.map_err(|error| format!("could not read Cursor composer key: {error}"))?;
            let nbytes = u64::try_from(nbytes)
                .map_err(|_| "Cursor composer value has invalid byte length".to_string())?;
            Ok((key, nbytes))
        })
        .collect::<Result<Vec<_>, String>>()
    })
    .await
    .unwrap_or_else(|| Err("Cursor composer key scan task failed".to_string()))
}

fn fetch_kv_text_bounded_sync(
    conn: &rusqlite::Connection,
    key: &str,
    max_bytes: u64,
    remaining: Option<u64>,
) -> BoundedSqliteValue<String> {
    let effective_cap = effective_sqlite_cap(max_bytes, remaining);
    let Ok(mut stmt) = conn.prepare(
        "SELECT length(CAST(value AS BLOB)) AS nbytes, \
         CASE WHEN length(CAST(value AS BLOB)) <= ?1 THEN value ELSE NULL END AS payload \
         FROM cursorDiskKV WHERE key = ?2",
    ) else {
        return BoundedSqliteValue::Corrupt;
    };
    let Ok(mut rows) = stmt.query(rusqlite::params![effective_cap as i64, key]) else {
        return BoundedSqliteValue::Corrupt;
    };
    let row = match rows.next() {
        Ok(Some(row)) => row,
        Ok(None) => return BoundedSqliteValue::Missing,
        Err(_) => return BoundedSqliteValue::Corrupt,
    };
    let Ok(nbytes_i) = row.get::<_, i64>(0) else {
        return BoundedSqliteValue::Corrupt;
    };
    if nbytes_i < 0 {
        return BoundedSqliteValue::Malformed { byte_len: 0 };
    }
    let byte_len = nbytes_i as u64;
    match row.get::<_, String>(1) {
        Ok(value) => BoundedSqliteValue::Ready { byte_len, value },
        Err(_) if byte_len > max_bytes => BoundedSqliteValue::Oversized { byte_len },
        Err(_) if remaining.is_some_and(|cap| byte_len > cap) => {
            BoundedSqliteValue::BudgetExceeded { byte_len }
        }
        Err(_) => BoundedSqliteValue::Malformed { byte_len },
    }
}

pub async fn fetch_kv_text_bounded(
    conn: &CursorConn,
    key: &str,
    max_bytes: u64,
    remaining: Option<u64>,
) -> BoundedSqliteValue<String> {
    let key = key.to_string();
    conn.with(move |conn| fetch_kv_text_bounded_sync(conn, &key, max_bytes, remaining))
        .await
        .unwrap_or(BoundedSqliteValue::Corrupt)
}

pub async fn fetch_bubble_bounded(
    conn: &CursorConn,
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
        BoundedSqliteValue::Corrupt => BoundedSqliteValue::Corrupt,
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

pub fn envelope_project(envelope: &Value) -> Option<ComposerProject> {
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

pub fn workspace_hash(envelope: &Value) -> Option<String> {
    envelope
        .get("workspaceIdentifier")
        .and_then(|w| w.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() as u64 <= MAX_COMPOSER_SQLITE_KEY_BYTES)
        .map(str::to_string)
}

pub fn epoch_ms_to_secs(ms: Option<i64>) -> Option<i64> {
    ms.filter(|v| *v > 0).map(|v| v / 1000)
}

/// Resolved project for a composer envelope.
pub struct ComposerProject {
    pub path: String,
}
