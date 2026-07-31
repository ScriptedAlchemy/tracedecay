//! Content fingerprinting of legacy source stores for idempotency markers.

use std::path::Path;

use sha2::{Digest, Sha256};

use super::copy::{MIGRATION_QUERY_PAGE_ROWS, quote_identifier, table_columns};
use super::{COPIED_MEMORY_TABLES, COPIED_TABLES};
use crate::db::engine::{QueryExecutor, Value, params};

pub(crate) fn hash_sqlite_value(hash: &mut Sha256, value: Value) {
    match value {
        Value::Null => hash.update(b"n"),
        Value::Integer(value) => {
            hash.update(b"i");
            hash.update(value.to_le_bytes());
        }
        Value::Real(value) => {
            hash.update(b"r");
            hash.update(value.to_bits().to_le_bytes());
        }
        Value::Text(value) => {
            hash.update(b"t");
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value.as_bytes());
        }
        Value::Blob(value) => {
            hash.update(b"b");
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value);
        }
    }
}

pub(crate) async fn hash_connection_tables<Q>(
    hash: &mut Sha256,
    source: &Q,
    tables: &[&str],
) -> Result<(), String>
where
    Q: QueryExecutor + ?Sized,
{
    for table in tables {
        let columns = table_columns(source, table).await?;
        if columns.is_empty() {
            continue;
        }
        hash.update(b"\0table\0");
        hash.update(table.as_bytes());
        for column in &columns {
            hash.update(b"\0column\0");
            hash.update(column.as_bytes());
        }
        let select = columns
            .iter()
            .map(|column| quote_identifier(column))
            .collect::<Vec<_>>()
            .join(", ");
        let mut last_rowid = i64::MIN;
        let mut first_page = true;
        loop {
            let sql = format!(
                "SELECT rowid, {select} FROM {}
                 WHERE rowid > ?1 OR (?3 = 1 AND rowid = ?1)
                 ORDER BY rowid LIMIT ?2",
                quote_identifier(table)
            );
            let mut rows = source
                .query(
                    &sql,
                    params![last_rowid, MIGRATION_QUERY_PAGE_ROWS, i64::from(first_page)],
                )
                .await
                .map_err(|error| format!("could not fingerprint source table {table}: {error}"))?;
            let mut page_rows = 0_i64;
            while let Some(row) = rows
                .next()
                .await
                .map_err(|error| format!("could not fingerprint source row in {table}: {error}"))?
            {
                let rowid = row
                    .get::<i64>(0)
                    .map_err(|error| format!("invalid source rowid in {table}: {error}"))?;
                if rowid < last_rowid || (rowid == last_rowid && (!first_page || page_rows > 0)) {
                    return Err(format!(
                        "source table {table} returned an unstable row order"
                    ));
                }
                last_rowid = rowid;
                page_rows += 1;
                hash.update(b"\0row\0");
                for index in 0..columns.len() {
                    let value = row.get::<Value>((index + 1) as i32).map_err(|error| {
                        format!("could not fingerprint source value in {table}: {error}")
                    })?;
                    hash_sqlite_value(hash, value);
                }
            }
            if page_rows < MIGRATION_QUERY_PAGE_ROWS {
                break;
            }
            first_page = false;
        }
    }
    Ok(())
}

pub(crate) async fn logical_source_fingerprint<S, M>(
    source: Option<&S>,
    source_path: &Path,
    memory_source: Option<(&M, &Path)>,
) -> Result<String, String>
where
    S: QueryExecutor + ?Sized,
    M: QueryExecutor + ?Sized,
{
    let mut hash = Sha256::new();
    hash.update(b"tracedecay-hermes-legacy-session-store-v1\0");
    hash.update(
        source_path
            .canonicalize()
            .unwrap_or_else(|_| source_path.to_path_buf())
            .to_string_lossy()
            .as_bytes(),
    );
    if let Some(source) = source {
        hash_connection_tables(&mut hash, source, COPIED_TABLES).await?;
    }
    if let Some((memory, memory_path)) = memory_source {
        hash.update(b"\0memory_path\0");
        hash.update(
            memory_path
                .canonicalize()
                .unwrap_or_else(|_| memory_path.to_path_buf())
                .to_string_lossy()
                .as_bytes(),
        );
        hash_connection_tables(&mut hash, memory, COPIED_MEMORY_TABLES).await?;
    }
    Ok(hex::encode(hash.finalize()))
}
