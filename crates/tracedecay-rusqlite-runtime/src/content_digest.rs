//! Whole-database canonical content digests.
//!
//! The digest is an admission/identity primitive: callers compare the SHA-256
//! of a database's logical contents across runtimes and processes. The framing
//! is therefore frozen — the domain tag, the table inventory order, the
//! per-table row order, and the per-value encoding below are all part of the
//! wire contract and must not be "improved" without a versioned domain tag.

use std::cmp::Ordering;
use std::path::Path;

use rusqlite::types::{Value, ValueRef};
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
#[hotpath::measure]
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
        digest_table(&connection, &mut digest, &table)?;
    }
    Ok(digest.finalize().into())
}

fn digest_table(
    connection: &Connection,
    digest: &mut Sha256,
    table: &str,
) -> Result<(), CanonicalContentDigestError> {
    let escaped = table.replace('"', "\"\"");
    if let Some(prefix) = unique_declaration_prefix_len(connection, &escaped)? {
        let order = (1..=prefix)
            .map(|index| index.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return digest_ordered_sql(
            connection,
            digest,
            &format!("SELECT * FROM \"{escaped}\" ORDER BY {order}"),
        );
    }
    digest_collected_in_column_order(connection, digest, &escaped)
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
        .collect::<rusqlite::Result<Vec<_>>>()
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
        .collect::<rusqlite::Result<Vec<_>>>()
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
            .collect::<rusqlite::Result<Vec<_>>>()
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
    digest_query_rows(digest, &mut statement)?;
    crate::telemetry::observe_statement(&statement);
    Ok(())
}

fn digest_collected_in_column_order(
    connection: &Connection,
    digest: &mut Sha256,
    escaped: &str,
) -> Result<(), CanonicalContentDigestError> {
    let mut statement = connection
        .prepare(&format!("SELECT * FROM \"{escaped}\""))
        .map_err(|error| CanonicalContentDigestError::new("prepare session table read", error))?;
    let column_count = statement.column_count();
    let mut rows = statement
        .query([])
        .map_err(|error| CanonicalContentDigestError::new("query session table", error))?;
    let mut collected = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| CanonicalContentDigestError::new("read session table row", error))?
    {
        let mut values = Vec::with_capacity(column_count);
        for index in 0..column_count {
            values.push(row.get::<_, Value>(index).map_err(|error| {
                CanonicalContentDigestError::new("decode session table value", error)
            })?);
        }
        collected.push(values);
    }
    drop(rows);
    crate::telemetry::observe_statement(&statement);
    collected.sort_by(|left, right| sqlite_row_cmp(left, right));
    for values in collected {
        digest.update(b"row\0");
        for value in values {
            digest_owned_value(digest, &value);
        }
    }
    Ok(())
}

fn digest_query_rows(
    digest: &mut Sha256,
    statement: &mut rusqlite::Statement<'_>,
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

fn digest_owned_value(digest: &mut Sha256, value: &Value) {
    match value {
        Value::Null => digest.update([0]),
        Value::Integer(value) => {
            digest.update([1]);
            digest.update(value.to_le_bytes());
        }
        Value::Real(value) => {
            digest.update([2]);
            digest.update(value.to_bits().to_le_bytes());
        }
        Value::Text(value) => {
            digest.update([3]);
            digest_len_prefixed(digest, value.as_bytes());
        }
        Value::Blob(value) => {
            digest.update([4]);
            digest_len_prefixed(digest, value);
        }
    }
}

fn sqlite_row_cmp(left: &[Value], right: &[Value]) -> Ordering {
    for (left, right) in left.iter().zip(right.iter()) {
        let order = sqlite_value_cmp(left, right);
        if order != Ordering::Equal {
            return order;
        }
    }
    Ordering::Equal
}

fn sqlite_value_cmp(left: &Value, right: &Value) -> Ordering {
    match (left, right) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        (Value::Integer(left), Value::Integer(right)) => left.cmp(right),
        (Value::Integer(left), Value::Real(right)) => cmp_integer_real(*left, *right),
        (Value::Real(left), Value::Integer(right)) => cmp_integer_real(*right, *left).reverse(),
        (Value::Real(left), Value::Real(right)) => left.total_cmp(right),
        (Value::Integer(_) | Value::Real(_), _) => Ordering::Less,
        (_, Value::Integer(_) | Value::Real(_)) => Ordering::Greater,
        (Value::Text(left), Value::Text(right)) => left.as_bytes().cmp(right.as_bytes()),
        (Value::Text(_), Value::Blob(_)) => Ordering::Less,
        (Value::Blob(_), Value::Text(_)) => Ordering::Greater,
        (Value::Blob(left), Value::Blob(right)) => left.cmp(right),
    }
}

fn cmp_integer_real(integer: i64, real: f64) -> Ordering {
    let as_real = integer as f64;
    if as_real.is_finite() && as_real as i64 == integer {
        return as_real.total_cmp(&real);
    }
    if !real.is_finite() {
        return if real.is_sign_negative() {
            Ordering::Greater
        } else {
            Ordering::Less
        };
    }
    if real < as_real {
        Ordering::Greater
    } else {
        Ordering::Less
    }
}

fn digest_len_prefixed(digest: &mut Sha256, value: &[u8]) {
    digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    digest.update(value);
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, StatementStatus};
    use sha2::{Digest as _, Sha256};

    use super::*;
    use crate::telemetry::take_observed_vm;

    const ROW_COUNT: i64 = 64;

    #[test]
    fn ordered_session_read_avoids_fullscan_sort_and_keeps_byte_exact_identity() {
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
                    payload TEXT NOT NULL,
                    extra TEXT NOT NULL,
                    provider TEXT NOT NULL,
                    message_id TEXT NOT NULL,
                    PRIMARY KEY (provider, message_id)
                 );",
            )
            .unwrap();
        for index in (1..=ROW_COUNT).rev() {
            connection
                .execute(
                    "INSERT INTO messages VALUES (?1, ?2, ?3, ?4)",
                    (
                        format!("payload-{index:04}"),
                        format!("extra-{index:04}"),
                        "cursor",
                        format!("msg-{index:04}"),
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
        let leftover = leftover_order_by_all_columns_vm(&connection);
        assert_eq!(leftover.visited, ROW_COUNT);
        assert!(
            leftover.fullscan_steps > 0 || leftover.sort_steps > 0,
            "ORDER BY every column on a non-prefix unique key must fullscan or sort; got {leftover:?}"
        );
        drop(connection);

        let expected = reference_session_domain_digest(&path);
        let _ = take_observed_vm();
        let actual = canonical_session_domain_content_sha256(&path).unwrap();
        let snapshot = take_observed_vm();
        assert_eq!(actual, expected, "digest identity must stay byte-exact");
        assert!(
            snapshot.fullscan_steps <= u64::try_from(ROW_COUNT).unwrap(),
            "unordered collect must visit each row at most once; got {snapshot:?}"
        );
        assert_eq!(
            snapshot.sort_steps, 0,
            "full-table ORDER BY must not run in SQLite; got {snapshot:?}"
        );
        assert!(
            snapshot.vm_steps > 0,
            "digest must record sqlite_vm steps for the table read"
        );
        assert!(
            snapshot.vm_steps < leftover.vm_steps,
            "index-ordered read must do fewer VM steps than full-table ORDER BY ({snapshot:?} vs {leftover:?})"
        );
        assert!(
            snapshot.vm_steps < u64::try_from(ROW_COUNT).unwrap().saturating_mul(32),
            "ordered read must stay linear in visited rows; got {snapshot:?}"
        );
    }

    #[derive(Debug)]
    struct LeftoverVm {
        visited: i64,
        fullscan_steps: u64,
        sort_steps: u64,
        vm_steps: u64,
    }

    fn leftover_order_by_all_columns_vm(connection: &Connection) -> LeftoverVm {
        let mut statement = connection
            .prepare("SELECT * FROM \"messages\" ORDER BY 1, 2, 3, 4")
            .unwrap();
        let visited = statement
            .query_map([], |_| Ok(()))
            .unwrap()
            .count()
            .try_into()
            .unwrap();
        LeftoverVm {
            visited,
            fullscan_steps: u64::try_from(
                statement.get_status(StatementStatus::FullscanStep).max(0),
            )
            .unwrap_or(u64::MAX),
            sort_steps: u64::try_from(statement.get_status(StatementStatus::Sort).max(0))
                .unwrap_or(u64::MAX),
            vm_steps: u64::try_from(statement.get_status(StatementStatus::VmStep).max(0))
                .unwrap_or(u64::MAX),
        }
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
