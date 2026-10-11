//! Whole-database canonical content digests.
//!
//! The digest is an admission/identity primitive: callers compare the SHA-256
//! of a database's logical contents across runtimes and processes. The framing
//! is therefore frozen. The domain tag, the table inventory order, the
//! per-table row order, and the per-value encoding below are all part of the
//! wire contract and must not be "improved" without a versioned domain tag.

use std::path::Path;

use sha2::{Digest as _, Sha256};
use tracedecay_sql::types::ValueRef;
use tracedecay_sql::{Connection, OpenFlags};

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
    fn new(operation: &'static str, error: tracedecay_sql::Error) -> Self {
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
#[tracing::instrument(
    name = "rusqlite_runtime.content_digest.canonical_sha256",
    level = "trace",
    skip_all
)]
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
        .collect::<tracedecay_sql::Result<Vec<_>>>()
        .map_err(|error| CanonicalContentDigestError::new("read session table inventory", error))?;
    drop(table_statement);

    let mut digest = Sha256::new();
    digest.update(b"tracedecay.session-domain-state.v1\0");
    for table in tables {
        digest_len_prefixed(&mut digest, table.as_bytes());
        digest_table(&connection, &mut digest, &table)?;
    }
    Ok(digest.finalize().into())
}

fn digest_table(
    connection: &Connection,
    digest: &mut Sha256,
    table: &str,
) -> Result<(), CanonicalContentDigestError> {
    digest_ordered_sql(connection, digest, &ordered_table_read(connection, table)?)
}

/// The read that fixes a table's row order: by its shortest NOT NULL unique
/// declaration prefix when one exists, otherwise by every column.
fn ordered_table_read(
    connection: &Connection,
    table: &str,
) -> Result<String, CanonicalContentDigestError> {
    let escaped = table.replace('"', "\"\"");
    let order_len = match unique_declaration_prefix_len(connection, &escaped)? {
        Some(prefix) => prefix,
        None => connection
            .prepare(&format!("SELECT * FROM \"{escaped}\""))
            .map_err(|error| CanonicalContentDigestError::new("prepare session table read", error))?
            .column_count(),
    };
    let order = (1..=order_len)
        .map(|index| index.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!("SELECT * FROM \"{escaped}\" ORDER BY {order}"))
}

fn unique_declaration_prefix_len(
    connection: &Connection,
    escaped: &str,
) -> Result<Option<usize>, CanonicalContentDigestError> {
    let mut info = connection
        .prepare(&format!("PRAGMA table_info(\"{escaped}\")"))
        .map_err(|error| CanonicalContentDigestError::new("prepare session table info", error))?;
    let columns = info
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(|error| CanonicalContentDigestError::new("query session table info", error))?
        .collect::<tracedecay_sql::Result<Vec<_>>>()
        .map_err(|error| CanonicalContentDigestError::new("read session table info", error))?;
    drop(info);
    let mut pk = columns
        .iter()
        .filter(|(_, _, _, pk)| *pk > 0)
        .map(|(cid, typ, notnull, pk)| (*pk, *cid, typ.as_str(), *notnull))
        .collect::<Vec<_>>();
    pk.sort_by_key(|(ordinal, _, _, _)| *ordinal);
    if pk.is_empty() {
        return unique_index_declaration_prefix_len(connection, escaped, &columns);
    }
    let prefix = pk.len();
    let unique_safe = pk.iter().all(|(_, _, typ, notnull)| {
        *notnull != 0 || typ.eq_ignore_ascii_case("INTEGER") || typ.eq_ignore_ascii_case("INT")
    });
    let is_prefix = pk.iter().enumerate().all(|(index, (ordinal, cid, _, _))| {
        *ordinal == (index as i64) + 1 && *cid == index as i64
    });
    if unique_safe && is_prefix {
        Ok(Some(prefix))
    } else {
        unique_index_declaration_prefix_len(connection, escaped, &columns)
    }
}

fn unique_index_declaration_prefix_len(
    connection: &Connection,
    escaped: &str,
    columns: &[(i64, String, i64, i64)],
) -> Result<Option<usize>, CanonicalContentDigestError> {
    let mut list = connection
        .prepare(&format!("PRAGMA index_list(\"{escaped}\")"))
        .map_err(|error| CanonicalContentDigestError::new("prepare session index list", error))?;
    let indexes = list
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(|error| CanonicalContentDigestError::new("query session index list", error))?
        .collect::<tracedecay_sql::Result<Vec<_>>>()
        .map_err(|error| CanonicalContentDigestError::new("read session index list", error))?;
    drop(list);
    let mut best = None;
    for (name, unique, partial) in indexes {
        if unique == 0 || partial != 0 {
            continue;
        }
        let escaped_index = name.replace('"', "\"\"");
        let mut info = connection
            .prepare(&format!("PRAGMA index_info(\"{escaped_index}\")"))
            .map_err(|error| {
                CanonicalContentDigestError::new("prepare session index info", error)
            })?;
        let cids = info
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
            .map_err(|error| CanonicalContentDigestError::new("query session index info", error))?
            .collect::<tracedecay_sql::Result<Vec<_>>>()
            .map_err(|error| CanonicalContentDigestError::new("read session index info", error))?;
        drop(info);
        if cids.is_empty() || cids.iter().any(|(_, cid)| *cid < 0) {
            continue;
        }
        let prefix = cids.len();
        let is_prefix = cids
            .iter()
            .enumerate()
            .all(|(index, (seqno, cid))| *seqno == index as i64 && *cid == index as i64);
        let notnull = cids.iter().all(|(_, cid)| {
            columns
                .iter()
                .any(|(column_cid, _, notnull, _)| *column_cid == *cid && *notnull != 0)
        });
        if is_prefix && notnull {
            best = Some(best.map_or(prefix, |current: usize| current.min(prefix)));
        }
    }
    Ok(best)
}

fn digest_ordered_sql(
    connection: &Connection,
    digest: &mut Sha256,
    sql: &str,
) -> Result<(), CanonicalContentDigestError> {
    let mut statement = connection
        .prepare(sql)
        .map_err(|error| CanonicalContentDigestError::new("prepare ordered session read", error))?;
    digest_query_rows(digest, &mut statement)
}

fn digest_query_rows(
    digest: &mut Sha256,
    statement: &mut tracedecay_sql::Statement<'_>,
) -> Result<(), CanonicalContentDigestError> {
    let column_count = statement.column_count();
    let mut rows = statement
        .query([])
        .map_err(|error| CanonicalContentDigestError::new("query session table", error))?;
    while let Some(row) = rows
        .next()
        .map_err(|error| CanonicalContentDigestError::new("read session table row", error))?
    {
        digest.update(b"row\0");
        for index in 0..column_count {
            digest_value_ref(
                digest,
                row.get_ref(index).map_err(|error| {
                    CanonicalContentDigestError::new("decode session table value", error)
                })?,
            );
        }
    }
    Ok(())
}

fn digest_value_ref(digest: &mut Sha256, value: ValueRef<'_>) {
    match value {
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
            digest_len_prefixed(digest, value);
        }
        ValueRef::Blob(value) => {
            digest.update([4]);
            digest_len_prefixed(digest, value);
        }
    }
}

fn digest_len_prefixed(digest: &mut Sha256, value: &[u8]) {
    digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    digest.update(value);
}

#[cfg(test)]
mod tests {
    use sha2::Sha256;
    use tracedecay_sql::Connection;

    use super::*;

    const ROW_COUNT: i64 = 64;

    #[test]
    fn declaration_prefix_read_avoids_fullscan_sort_and_keeps_byte_exact_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-domain.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE analytics_events (
                    id INTEGER PRIMARY KEY,
                    payload TEXT NOT NULL
                 );
                 CREATE TABLE messages (
                    provider TEXT NOT NULL,
                    message_id TEXT NOT NULL,
                    payload TEXT NOT NULL,
                    extra TEXT NOT NULL,
                    PRIMARY KEY (provider, message_id)
                 );",
            )
            .unwrap();
        for index in (1..=ROW_COUNT).rev() {
            connection
                .execute(
                    "INSERT INTO messages VALUES (?1, ?2, ?3, ?4)",
                    (
                        "cursor",
                        format!("msg-{index:04}"),
                        format!("payload-{index:04}"),
                        format!("extra-{index:04}"),
                    ),
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO analytics_events VALUES (?1, ?2)",
                    (index, format!("event-{index:04}")),
                )
                .unwrap();
        }
        let read = ordered_table_read(&connection, "messages").unwrap();
        let plan = connection
            .prepare(&format!("EXPLAIN QUERY PLAN {read}"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<tracedecay_sql::Result<Vec<_>>>()
            .unwrap();
        drop(connection);
        assert_eq!(read, "SELECT * FROM \"messages\" ORDER BY 1, 2");
        assert!(
            !plan.is_empty() && plan.iter().all(|step| !step.contains("TEMP B-TREE")),
            "the declaration-prefix read must follow the primary-key index, not sort; got {plan:?}"
        );

        let expected = reference_session_domain_digest(&path);
        let actual = canonical_session_domain_content_sha256(&path).unwrap();
        assert_eq!(actual, expected, "digest identity must stay byte-exact");
    }

    fn reference_session_domain_digest(path: &std::path::Path) -> [u8; 32] {
        let connection =
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut digest = Sha256::new();
        digest.update(b"tracedecay.session-domain-state.v1\0");
        digest_len_prefixed(&mut digest, b"messages");
        let mut statement = connection
            .prepare("SELECT * FROM \"messages\" ORDER BY 1, 2, 3, 4")
            .unwrap();
        let column_count = statement.column_count();
        let mut rows = statement.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            digest.update(b"row\0");
            for index in 0..column_count {
                match row.get_ref(index).unwrap() {
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
        digest.finalize().into()
    }
}
