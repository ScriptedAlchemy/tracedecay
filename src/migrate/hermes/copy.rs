//! Row-level table copy primitives shared by the session and memory merges.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use libsql::{Connection, Value, params};
use sha2::{Digest, Sha256};

pub(crate) fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

pub(crate) async fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>, String> {
    let sql = format!("PRAGMA table_info({})", quote_identifier(table));
    let mut rows = conn
        .query(&sql, ())
        .await
        .map_err(|error| format!("could not inspect table {table}: {error}"))?;
    let mut columns = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| format!("could not read table {table} columns: {error}"))?
    {
        columns.push(
            row.get(1)
                .map_err(|error| format!("invalid table {table} column: {error}"))?,
        );
    }
    Ok(columns)
}

pub(crate) async fn count_exact_rows(
    target: &Connection,
    table: &str,
    columns: &[String],
    values: &[Value],
) -> Result<u64, String> {
    let predicates = columns
        .iter()
        .enumerate()
        .map(|(index, column)| format!("{} IS ?{}", quote_identifier(column), index + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    let sql = format!(
        "SELECT COUNT(*) FROM {} WHERE {predicates}",
        quote_identifier(table)
    );
    let mut rows = target
        .query(&sql, libsql::params_from_iter(values.iter().cloned()))
        .await
        .map_err(|error| format!("could not count target {table} rows: {error}"))?;
    rows.next()
        .await
        .map_err(|error| format!("could not read target {table} row count: {error}"))?
        .ok_or_else(|| format!("target {table} row count is absent"))?
        .get::<i64>(0)
        .map(|count| count.max(0) as u64)
        .map_err(|error| format!("invalid target {table} row count: {error}"))
}

/// Exact duplicates are explicit idempotent skips. Any uniqueness collision
/// with different data is an error, never an `INSERT OR IGNORE` data loss.
pub(crate) async fn insert_row_or_skip_exact(
    target: &Connection,
    table: &str,
    columns: &[String],
    values: &[Value],
) -> Result<u64, String> {
    let predicates = columns
        .iter()
        .enumerate()
        .map(|(index, column)| format!("{} IS ?{}", quote_identifier(column), index + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    let exact_sql = format!(
        "SELECT 1 FROM {} WHERE {predicates} LIMIT 1",
        quote_identifier(table)
    );
    let mut exact = target
        .query(&exact_sql, libsql::params_from_iter(values.iter().cloned()))
        .await
        .map_err(|error| format!("could not check target {table} row: {error}"))?;
    if exact
        .next()
        .await
        .map_err(|error| format!("could not read target {table} row: {error}"))?
        .is_some()
    {
        return Ok(0);
    }

    let quoted = columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = (1..=columns.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let insert_sql = format!(
        "INSERT INTO {} ({quoted}) VALUES ({placeholders})",
        quote_identifier(table)
    );
    target
        .execute(
            &insert_sql,
            libsql::params_from_iter(values.iter().cloned()),
        )
        .await
        .map_err(|error| {
            format!(
                "legacy {table} row collides with a different target row; migration was rolled back: {error}"
            )
        })
}

pub(crate) async fn copy_table<F>(
    source: &Connection,
    target: &Connection,
    table: &str,
    excluded: &[&str],
    mut transform: F,
) -> Result<u64, String>
where
    F: FnMut(&[String], &mut Vec<Value>) -> Result<(), String>,
{
    let source_columns = table_columns(source, table).await?;
    if source_columns.is_empty() {
        return Ok(0);
    }
    let target_columns = table_columns(target, table).await?;
    if target_columns.is_empty() {
        return Err(format!("target is missing required table {table}"));
    }
    let columns = source_columns
        .into_iter()
        .filter(|column| target_columns.contains(column) && !excluded.contains(&column.as_str()))
        .collect::<Vec<_>>();
    if columns.is_empty() {
        return Ok(0);
    }
    let quoted = columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let select_sql = format!(
        "SELECT {quoted} FROM {} ORDER BY rowid",
        quote_identifier(table)
    );
    let mut source_rows = source
        .query(&select_sql, ())
        .await
        .map_err(|error| format!("could not read source table {table}: {error}"))?;
    let mut inserted = 0;
    while let Some(row) = source_rows
        .next()
        .await
        .map_err(|error| format!("could not read source row from {table}: {error}"))?
    {
        let mut values = Vec::with_capacity(columns.len());
        for index in 0..columns.len() {
            values.push(
                row.get::<Value>(index as i32).map_err(|error| {
                    format!("could not decode source row from {table}: {error}")
                })?,
            );
        }
        transform(&columns, &mut values)?;
        inserted += insert_row_or_skip_exact(target, table, &columns, &values).await?;
    }
    Ok(inserted)
}

pub(crate) fn remap_store_id_columns(
    columns: &[String],
    values: &mut [Value],
    id_map: &HashMap<i64, i64>,
    remapped_columns: &[&str],
) -> Result<(), String> {
    for (column, value) in columns.iter().zip(values.iter_mut()) {
        if !remapped_columns.contains(&column.as_str()) {
            continue;
        }
        let Value::Integer(source_id) = value else {
            continue;
        };
        let target_id = id_map
            .get(source_id)
            .ok_or_else(|| format!("referenced raw store_id {source_id} was not copied"))?;
        *value = Value::Integer(*target_id);
    }
    Ok(())
}

pub(crate) fn remap_summary_source(
    columns: &[String],
    values: &mut [Value],
    id_map: &HashMap<i64, i64>,
) -> Result<(), String> {
    let kind_index = columns
        .iter()
        .position(|column| column == "source_kind")
        .ok_or_else(|| "summary source has no source_kind".to_string())?;
    let id_index = columns
        .iter()
        .position(|column| column == "source_id")
        .ok_or_else(|| "summary source has no source_id".to_string())?;
    if matches!(&values[kind_index], Value::Text(kind) if kind == "raw_message") {
        let Value::Text(source_id) = &values[id_index] else {
            return Err("raw summary source has a non-text source_id".to_string());
        };
        let source_id = source_id
            .parse::<i64>()
            .map_err(|_| "raw summary source has an invalid store_id".to_string())?;
        let target_id = id_map
            .get(&source_id)
            .ok_or_else(|| format!("raw summary source {source_id} was not copied"))?;
        values[id_index] = Value::Text(target_id.to_string());
    }
    Ok(())
}

pub(crate) async fn copy_raw_messages(
    source: &Connection,
    target: &Connection,
) -> Result<(u64, HashMap<i64, i64>), String> {
    let source_columns = table_columns(source, "lcm_raw_messages").await?;
    if source_columns.is_empty() {
        return Ok((0, HashMap::new()));
    }
    if !source_columns.iter().any(|column| column == "store_id") {
        return Err("source lcm_raw_messages has no store_id".to_string());
    }
    let target_columns = table_columns(target, "lcm_raw_messages").await?;
    let columns = source_columns
        .into_iter()
        .filter(|column| column != "store_id" && target_columns.contains(column))
        .collect::<Vec<_>>();
    let provider_index = columns
        .iter()
        .position(|column| column == "provider")
        .ok_or_else(|| "source raw messages have no provider".to_string())?;
    let message_index = columns
        .iter()
        .position(|column| column == "message_id")
        .ok_or_else(|| "source raw messages have no message_id".to_string())?;
    let quoted = columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let select_sql = format!("SELECT store_id, {quoted} FROM lcm_raw_messages ORDER BY store_id");
    let mut rows = source
        .query(&select_sql, ())
        .await
        .map_err(|error| format!("could not read source raw messages: {error}"))?;
    let mut inserted = 0;
    let mut id_map = HashMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| format!("could not read source raw message: {error}"))?
    {
        let source_id: i64 = row
            .get(0)
            .map_err(|error| format!("invalid source raw store_id: {error}"))?;
        let provider: String = row
            .get((provider_index + 1) as i32)
            .map_err(|error| format!("invalid source raw provider: {error}"))?;
        let message_id: String = row
            .get((message_index + 1) as i32)
            .map_err(|error| format!("invalid source raw message_id: {error}"))?;
        let mut values = Vec::with_capacity(columns.len());
        for index in 0..columns.len() {
            values.push(
                row.get::<Value>((index + 1) as i32)
                    .map_err(|error| format!("could not decode source raw message: {error}"))?,
            );
        }
        inserted += insert_row_or_skip_exact(target, "lcm_raw_messages", &columns, &values).await?;
        let mut target_rows = target
            .query(
                "SELECT store_id FROM lcm_raw_messages WHERE provider = ?1 AND message_id = ?2",
                params![provider, message_id],
            )
            .await
            .map_err(|error| format!("could not resolve target raw store_id: {error}"))?;
        let target_id = target_rows
            .next()
            .await
            .map_err(|error| format!("could not read target raw store_id: {error}"))?
            .ok_or_else(|| "copied raw message is absent from target".to_string())?
            .get(0)
            .map_err(|error| format!("invalid target raw store_id: {error}"))?;
        id_map.insert(source_id, target_id);
    }
    Ok((inserted, id_map))
}

pub(crate) async fn copy_external_payload_files(
    source: &Connection,
    source_db_path: &Path,
    target_db_path: &Path,
    created: &mut Vec<PathBuf>,
) -> Result<(), String> {
    if table_columns(source, "lcm_external_payloads")
        .await?
        .is_empty()
    {
        return Ok(());
    }
    let source_dir = source_db_path
        .parent()
        .ok_or_else(|| "source session DB has no parent directory".to_string())?
        .join("lcm-payloads");
    let target_dir = target_db_path
        .parent()
        .ok_or_else(|| "target session DB has no parent directory".to_string())?
        .join("lcm-payloads");
    let mut rows = source
        .query(
            "SELECT payload_ref, content_hash FROM lcm_external_payloads ORDER BY payload_ref",
            (),
        )
        .await
        .map_err(|error| format!("could not inspect source payloads: {error}"))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| format!("could not read source payload: {error}"))?
    {
        let payload_ref: String = row
            .get(0)
            .map_err(|error| format!("invalid source payload ref: {error}"))?;
        let expected_hash: String = row
            .get(1)
            .map_err(|error| format!("invalid source payload hash: {error}"))?;
        crate::sessions::lcm::payload::validate_payload_ref(&payload_ref)
            .map_err(|error| format!("unsafe source payload ref '{payload_ref}': {error}"))?;
        let source_file = source_dir.join(&payload_ref);
        let metadata = fs::symlink_metadata(&source_file).map_err(|error| {
            format!(
                "source payload '{}' is unavailable: {error}",
                source_file.display()
            )
        })?;
        if !metadata.file_type().is_file() {
            return Err(format!(
                "source payload '{}' is not a regular file",
                source_file.display()
            ));
        }
        let bytes = fs::read(&source_file).map_err(|error| {
            format!(
                "could not read source payload '{}': {error}",
                source_file.display()
            )
        })?;
        let actual_hash = hex::encode(Sha256::digest(&bytes));
        if actual_hash != expected_hash {
            return Err(format!(
                "source payload '{}' failed its content hash",
                source_file.display()
            ));
        }
        fs::create_dir_all(&target_dir)
            .map_err(|error| format!("could not create target payload directory: {error}"))?;
        let target_metadata = fs::symlink_metadata(&target_dir)
            .map_err(|error| format!("could not inspect target payload directory: {error}"))?;
        if !target_metadata.file_type().is_dir() {
            return Err("target payload directory is not a regular directory".to_string());
        }
        let target_file = target_dir.join(&payload_ref);
        if target_file.exists() {
            let existing = fs::read(&target_file)
                .map_err(|error| format!("could not read existing target payload: {error}"))?;
            if hex::encode(Sha256::digest(&existing)) != expected_hash {
                return Err(format!(
                    "target payload '{}' conflicts with the legacy source",
                    target_file.display()
                ));
            }
            continue;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target_file)
            .map_err(|error| format!("could not create target payload: {error}"))?;
        created.push(target_file.clone());
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("could not persist target payload: {error}"))?;
    }
    Ok(())
}

pub(crate) fn remove_created_payloads(paths: &[PathBuf]) {
    for path in paths.iter().rev() {
        let _ = fs::remove_file(path);
    }
}
