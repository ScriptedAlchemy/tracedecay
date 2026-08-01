// Rust guideline compliant 2025-10-17
//! Cross-session response cache for `tracedecay_read`.
//!
//! Cached entries are keyed by `(project_id, file_path, mode, args_hash)` and
//! survive across MCP sessions. The `mtime_ns` column on each row is the
//! source-of-truth for freshness: a row is served only if the file's current
//! `mtime_ns` matches what was cached. Any mismatch triggers a recomputation
//! and replaces the row.
//!
//! Per-session entries are still possible (set `session_id` to a real session
//! identifier), but the canonical 5.0 mode passes `GLOBAL_SESSION` so a single
//! row backs all sessions on the same project.
//!
//! The cache lives in the same SQLite database as the code graph.

use sha2::{Digest, Sha256};
use tracedecay_domain::canonical_json_value;

use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::db::engine::{QueryExecutor, params};
use tracedecay_runtime_core::errors::{Result, TraceDecayError};

/// Sentinel session id used for cross-session cache rows. Picked so it cannot
/// collide with a real session UUID.
pub const GLOBAL_SESSION: &str = "global";

/// A cached read response.
#[derive(Debug, Clone)]
pub struct CachedRead {
    pub mtime_ns: i64,
    pub digest: String,
    pub token_count: u32,
}

/// Computes a stable hash of the per-call arguments that affect output. Used
/// as the `args_hash` cache-key component so two calls with different `lines`
/// or `limit` values map to distinct rows.
pub fn args_hash(args: &serde_json::Value) -> Result<String> {
    let canonical = canonical_json_value(args).map_err(|error| TraceDecayError::Config {
        message: format!("cannot canonicalize read cache arguments: {error}"),
    })?;
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    Ok(hex::encode(hasher.finalize()))
}

/// SHA-256 of arbitrary bytes, hex-encoded. Used as the body digest so callers
/// can detect content changes even when only the cache layer changed.
pub fn digest_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Looks up a cached row. Returns `Some` only when the row exists *and* its
/// `mtime_ns` matches `current_mtime_ns`. A stale row (file changed since the
/// cache was written) is reported as a miss; the caller is expected to
/// recompute and `put` a fresh row, which replaces the stale one via the
/// primary-key `INSERT OR REPLACE`.
pub(crate) async fn get(
    conn: &impl QueryExecutor,
    project_id: &str,
    session_id: &str,
    file_path: &str,
    mode: &str,
    args_hash: &str,
    current_mtime_ns: i64,
) -> Result<Option<CachedRead>> {
    let mut rows = conn
        .query(
            "SELECT mtime_ns, digest, token_count
             FROM read_cache
             WHERE project_id = ?1
               AND session_id = ?2
               AND file_path  = ?3
               AND mode       = ?4
               AND args_hash  = ?5",
            params![project_id, session_id, file_path, mode, args_hash],
        )
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("read_cache lookup failed: {e}"),
            operation: "read_cache::get".to_string(),
        })?;

    let row = rows.next().await.map_err(|e| TraceDecayError::Database {
        message: format!("read_cache row fetch failed: {e}"),
        operation: "read_cache::get".to_string(),
    })?;

    let Some(row) = row else { return Ok(None) };

    let cached_mtime: i64 = row.get(0).map_err(|e| TraceDecayError::Database {
        message: format!("read_cache column 0: {e}"),
        operation: "read_cache::get".to_string(),
    })?;

    if cached_mtime != current_mtime_ns {
        return Ok(None);
    }

    let digest: String = row.get(1).map_err(|e| TraceDecayError::Database {
        message: format!("read_cache column 1: {e}"),
        operation: "read_cache::get".to_string(),
    })?;
    let token_count: i64 = row.get(2).map_err(|e| TraceDecayError::Database {
        message: format!("read_cache column 2: {e}"),
        operation: "read_cache::get".to_string(),
    })?;

    Ok(Some(CachedRead {
        mtime_ns: cached_mtime,
        digest,
        token_count: token_count.max(0) as u32,
    }))
}

/// Parameters for the internal [`put_write`] entry point.
pub(crate) struct ReadCacheWrite<'a> {
    pub project_id: &'a str,
    pub session_id: &'a str,
    pub file_path: &'a str,
    pub mtime_ns: i64,
    pub mode: &'a str,
    pub args_hash: &'a str,
    pub digest: &'a str,
    pub body: &'a [u8],
    pub token_count: u32,
}

/// Inserts or replaces a cache row. The primary key is
/// `(project_id, session_id, file_path, mode, args_hash)`; a re-`put` with
/// matching keys replaces the prior row, which is how stale entries (mtime
/// mismatch) get evicted.
#[expect(
    clippy::too_many_arguments,
    reason = "preserves the public read-cache API"
)]
pub async fn put(
    db: &Database,
    project_id: &str,
    session_id: &str,
    file_path: &str,
    mtime_ns: i64,
    mode: &str,
    args_hash: &str,
    digest: &str,
    body: &[u8],
    token_count: u32,
) -> Result<()> {
    put_write(
        db,
        ReadCacheWrite {
            project_id,
            session_id,
            file_path,
            mtime_ns,
            mode,
            args_hash,
            digest,
            body,
            token_count,
        },
    )
    .await
}

pub(crate) async fn put_write(db: &Database, write: ReadCacheWrite<'_>) -> Result<()> {
    let ReadCacheWrite {
        project_id,
        session_id,
        file_path,
        mtime_ns,
        mode,
        args_hash,
        digest,
        body,
        token_count,
    } = write;
    let now = unix_seconds();
    let transaction = db.begin_write_transaction("read_cache::put").await?;
    transaction
        .execute_engine(
            "INSERT OR REPLACE INTO read_cache
            (project_id, session_id, file_path, mtime_ns, mode, args_hash,
             digest, body, token_count, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                project_id,
                session_id,
                file_path,
                mtime_ns,
                mode,
                args_hash,
                digest,
                body,
                i64::from(token_count),
                now
            ],
        )
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("read_cache write failed: {error}"),
            operation: "read_cache::put".to_owned(),
        })?;
    transaction.commit().await
}

fn unix_seconds() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Reads a file's modification time, normalised to nanoseconds since the
/// UNIX epoch. Used as the freshness key for cache lookups.
pub fn file_mtime_ns(path: &std::path::Path) -> std::io::Result<i64> {
    use std::time::UNIX_EPOCH;
    let metadata = std::fs::metadata(path)?;
    let mtime = metadata.modified()?;
    let dur = mtime
        .duration_since(UNIX_EPOCH)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "mtime before epoch"))?;
    let nanos = i128::from(dur.as_secs()) * 1_000_000_000 + i128::from(dur.subsec_nanos());
    Ok(nanos.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn args_hash_uses_domain_canonical_encoding_for_escaped_object_keys() {
        let args = json!({
            "quoted\"key": {
                "slash\\key": "control:\u{0001}",
            },
        });
        let canonical =
            tracedecay_domain::canonical_json_value(&args).expect("domain canonical JSON");

        assert_eq!(
            args_hash(&args).expect("cache argument hash"),
            digest_bytes(canonical.as_bytes())
        );
    }

    #[test]
    fn args_hash_preserves_legacy_production_key() {
        let args = json!({
            "lines": "1:2",
            "last_sync_at": "revision",
        });

        assert_eq!(
            args_hash(&args).expect("cache argument hash"),
            "60181422b29f04d5ba1d28641de87fcd4e2afadd25d5be878ced4905927c0507"
        );
    }
}
