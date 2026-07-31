use sha2::{Digest, Sha256};

use crate::db::engine::{IntoParams, QueryExecutor, Value, params};

use super::LcmError;

#[cfg(unix)]
pub(crate) fn file_mtime_seconds(metadata: &std::fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    metadata.mtime()
}

#[cfg(not(unix))]
pub(crate) fn file_mtime_seconds(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

pub(crate) fn opt_text(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |s| Value::Text(s.to_string()))
}

pub(crate) fn opt_i64(value: Option<i64>) -> Value {
    value.map_or(Value::Null, Value::Integer)
}

pub(crate) fn sha256_hex(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    hex::encode(hasher.finalize())
}

pub(crate) async fn fetch_i64(
    conn: &(impl QueryExecutor + ?Sized),
    sql: &str,
    params: impl IntoParams,
    empty_message: &str,
) -> Result<i64, LcmError> {
    let mut rows = conn.query(sql, params).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db(empty_message.to_string()))?;
    Ok(row.get::<i64>(0)?)
}

pub(crate) async fn count_by_provider_session(
    conn: &(impl QueryExecutor + ?Sized),
    table: &str,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let sql = format!(
        "SELECT COUNT(*) FROM {table} WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)"
    );
    fetch_i64(
        conn,
        &sql,
        params![provider, opt_text(session_id)],
        "count query returned no rows",
    )
    .await
}
