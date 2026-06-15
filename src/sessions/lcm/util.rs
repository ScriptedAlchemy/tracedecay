use libsql::{params::IntoParams, Connection, Value};
use sha2::{Digest, Sha256};

use super::LcmError;

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
    conn: &Connection,
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
    conn: &Connection,
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
        libsql::params![provider, opt_text(session_id)],
        "count query returned no rows",
    )
    .await
}

/// Runs `work` inside a `BEGIN IMMEDIATE` transaction, committing on success
/// and rolling back on error.
pub(crate) async fn with_immediate_tx<T>(
    conn: &Connection,
    work: impl std::future::Future<Output = Result<T, LcmError>>,
) -> Result<T, LcmError> {
    conn.execute("BEGIN IMMEDIATE", ()).await?;
    match work.await {
        Ok(value) => {
            if let Err(err) = conn.execute("COMMIT", ()).await {
                let _ = conn.execute("ROLLBACK", ()).await;
                return Err(err.into());
            }
            Ok(value)
        }
        Err(err) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(err)
        }
    }
}
