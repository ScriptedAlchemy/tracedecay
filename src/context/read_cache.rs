//! Compatibility facade for cross-session read-cache operations.

use libsql::Connection;

use crate::errors::{Result, TraceDecayError};

use tracedecay_usecases::context::read_cache as usecases;
pub use tracedecay_usecases::context::read_cache::{
    CachedRead, GLOBAL_SESSION, args_hash, digest_bytes, file_mtime_ns,
};

fn root_error(error: usecases::ReadCacheError) -> TraceDecayError {
    TraceDecayError::Database {
        message: error.message,
        operation: error.operation.to_string(),
    }
}

pub async fn get(
    conn: &Connection,
    project_id: &str,
    session_id: &str,
    file_path: &str,
    mode: &str,
    args_hash: &str,
    current_mtime_ns: i64,
) -> Result<Option<CachedRead>> {
    usecases::get(
        conn,
        project_id,
        session_id,
        file_path,
        mode,
        args_hash,
        current_mtime_ns,
    )
    .await
    .map_err(root_error)
}

#[allow(clippy::too_many_arguments)]
pub async fn put(
    conn: &Connection,
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
    usecases::put(
        conn,
        project_id,
        session_id,
        file_path,
        mtime_ns,
        mode,
        args_hash,
        digest,
        body,
        token_count,
    )
    .await
    .map_err(root_error)
}

pub async fn sweep(conn: &Connection) -> Result<u64> {
    usecases::sweep(conn).await.map_err(root_error)
}
