//! Whole-database canonical content digests.
//!
//! The digest is an admission/identity primitive: callers compare the SHA-256
//! of a database's logical contents across runtimes and processes. The framing
//! is therefore frozen — the domain tag, the table inventory order, the
//! per-table row order, and the per-value encoding below are all part of the
//! wire contract and must not be "improved" without a versioned domain tag.

use std::path::Path;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest as _, Sha256};

/// Failure while computing a whole-database canonical content digest.
///
/// `operation` names the storage step that failed and `message` carries the
/// driver's own description, so callers can map this onto their own database
/// error type without losing either half.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{operation}: {message}")]
pub struct CanonicalContentDigestError {
    /// The storage step that failed.
    pub operation: &'static str,
    /// The underlying driver message.
    pub message: String,
}

impl CanonicalContentDigestError {
    fn new(operation: &'static str, error: rusqlite::Error) -> Self {
        Self {
            operation,
            message: error.to_string(),
        }
    }
}

/// Canonical SHA-256 over the logical contents of a session-domain database.
///
/// Opens `path` read-only, enumerates every non-internal table except
/// `analytics_events` in name order, reads each table ordered by every column
/// left to right, and folds a self-delimiting encoding of every value into a
/// single digest under the `tracedecay.session-domain-state.v1` domain tag.
///
/// Analytics rows are excluded because they are observational: they record how
/// a store was used rather than what it holds, so they must not perturb an
/// identity comparison.
pub fn canonical_session_domain_content_sha256(
    path: &Path,
) -> Result<[u8; 32], CanonicalContentDigestError> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| CanonicalContentDigestError::new("open session database", error))?;
    let mut table_statement = connection
        .prepare(
            "SELECT name
             FROM sqlite_schema
             WHERE type = 'table'
               AND name NOT LIKE 'sqlite_%'
               AND name <> 'analytics_events'
             ORDER BY name",
        )
        .map_err(|error| {
            CanonicalContentDigestError::new("prepare session table inventory", error)
        })?;
    let tables = table_statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| CanonicalContentDigestError::new("query session table inventory", error))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| CanonicalContentDigestError::new("read session table inventory", error))?;
    drop(table_statement);

    let mut digest = Sha256::new();
    digest.update(b"tracedecay.session-domain-state.v1\0");
    for table in tables {
        digest_len_prefixed(&mut digest, table.as_bytes());
        let escaped = table.replace('"', "\"\"");
        let mut statement = connection
            .prepare(&format!("SELECT * FROM \"{escaped}\""))
            .map_err(|error| {
                CanonicalContentDigestError::new("prepare session table read", error)
            })?;
        let column_count = statement.column_count();
        let order = (1..=column_count)
            .map(|index| index.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("SELECT * FROM \"{escaped}\" ORDER BY {order}");
        drop(statement);
        statement = connection.prepare(&sql).map_err(|error| {
            CanonicalContentDigestError::new("prepare ordered session read", error)
        })?;
        let mut rows = statement
            .query([])
            .map_err(|error| CanonicalContentDigestError::new("query session table", error))?;
        while let Some(row) = rows
            .next()
            .map_err(|error| CanonicalContentDigestError::new("read session table row", error))?
        {
            digest.update(b"row\0");
            for index in 0..column_count {
                match row.get_ref(index).map_err(|error| {
                    CanonicalContentDigestError::new("decode session table value", error)
                })? {
                    ValueRef::Null => digest.update([0]),
                    ValueRef::Integer(value) => {
                        digest.update([1]);
                        digest.update(value.to_le_bytes());
                    }
                    ValueRef::Real(value) => {
                        digest.update([2]);
                        digest.update(value.to_bits().to_le_bytes());
                    }
                    ValueRef::Text(value) => {
                        digest.update([3]);
                        digest_len_prefixed(&mut digest, value);
                    }
                    ValueRef::Blob(value) => {
                        digest.update([4]);
                        digest_len_prefixed(&mut digest, value);
                    }
                }
            }
        }
    }
    Ok(digest.finalize().into())
}

fn digest_len_prefixed(digest: &mut Sha256, value: &[u8]) {
    digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    digest.update(value);
}
