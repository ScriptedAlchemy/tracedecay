//! Content fingerprinting of legacy source stores for idempotency markers.

use std::path::Path;

use libsql::{Connection, Value};
use sha2::{Digest, Sha256};

use super::copy::{quote_identifier, table_columns};
use super::{COPIED_MEMORY_TABLES, COPIED_TABLES};

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

pub(crate) async fn hash_connection_tables(
    hash: &mut Sha256,
    source: &Connection,
    tables: &[&str],
) -> Result<(), String> {
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
        let sql = format!(
            "SELECT {select} FROM {} ORDER BY rowid",
            quote_identifier(table)
        );
        let mut rows = source
            .query(&sql, ())
            .await
            .map_err(|error| format!("could not fingerprint source table {table}: {error}"))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("could not fingerprint source row in {table}: {error}"))?
        {
            hash.update(b"\0row\0");
            for index in 0..columns.len() {
                let value = row.get::<Value>(index as i32).map_err(|error| {
                    format!("could not fingerprint source value in {table}: {error}")
                })?;
                hash_sqlite_value(hash, value);
            }
        }
    }
    Ok(())
}

pub(crate) async fn logical_source_fingerprint(
    source: Option<&Connection>,
    source_path: &Path,
    memory_source: Option<(&Connection, &Path)>,
) -> Result<String, String> {
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
