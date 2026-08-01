//! Row-level table copy primitives shared by the session and memory merges.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::root_seam::db::engine::{Executor, QueryExecutor, Value, params, params_from_iter};

pub const MIGRATION_QUERY_PAGE_ROWS: i64 = 256;
pub const MAX_MIGRATION_MATERIALIZED_ROWS: usize = 1_000_000;
const MAX_MIGRATED_PAYLOAD_BYTES: u64 = 64 * 1024 * 1024;

pub fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

pub fn compatible_columns(
    source_columns: Vec<String>,
    target_columns: &[String],
    excluded: &[&str],
    table: &str,
) -> Result<Vec<String>, String> {
    let unsupported = source_columns
        .iter()
        .filter(|column| !excluded.contains(&column.as_str()) && !target_columns.contains(*column))
        .cloned()
        .collect::<Vec<_>>();
    if !unsupported.is_empty() {
        return Err(format!(
            "source table {table} has unsupported columns that would be dropped: {}",
            unsupported.join(", ")
        ));
    }
    Ok(source_columns
        .into_iter()
        .filter(|column| !excluded.contains(&column.as_str()))
        .collect())
}

pub fn ensure_materialized_row_room(current_rows: usize, collection: &str) -> Result<(), String> {
    if current_rows >= MAX_MIGRATION_MATERIALIZED_ROWS {
        Err(format!(
            "source {collection} exceeds the migration materialization ceiling of \
             {MAX_MIGRATION_MATERIALIZED_ROWS} rows"
        ))
    } else {
        Ok(())
    }
}

pub async fn table_columns<Q>(conn: &Q, table: &str) -> Result<Vec<String>, String>
where
    Q: QueryExecutor + ?Sized,
{
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

pub async fn count_exact_rows<Q>(
    target: &Q,
    table: &str,
    columns: &[String],
    values: &[Value],
) -> Result<u64, String>
where
    Q: QueryExecutor + ?Sized,
{
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
        .query(&sql, params_from_iter(values.iter().cloned()))
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
pub async fn insert_row_or_skip_exact<E>(
    target: &E,
    table: &str,
    columns: &[String],
    values: &[Value],
) -> Result<u64, String>
where
    E: Executor + ?Sized,
{
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
        .query(&exact_sql, params_from_iter(values.iter().cloned()))
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
            params_from_iter(values.iter().cloned()),
        )
        .await
        .map_err(|error| {
            format!(
                "legacy {table} row collides with a different target row; migration was rolled back: {error}"
            )
        })
}

pub async fn copy_table<S, T, F>(
    source: &S,
    target: &T,
    table: &str,
    excluded: &[&str],
    mut transform: F,
) -> Result<u64, String>
where
    S: QueryExecutor + ?Sized,
    T: Executor + ?Sized,
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
    let columns = compatible_columns(source_columns, &target_columns, excluded, table)?;
    if columns.is_empty() {
        return Ok(0);
    }
    let quoted = columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let mut inserted = 0;
    let mut last_rowid = i64::MIN;
    let mut first_page = true;
    loop {
        let select_sql = format!(
            "SELECT rowid, {quoted} FROM {}
             WHERE rowid > ?1 OR (?3 = 1 AND rowid = ?1)
             ORDER BY rowid LIMIT ?2",
            quote_identifier(table)
        );
        let mut source_rows = source
            .query(
                &select_sql,
                params![last_rowid, MIGRATION_QUERY_PAGE_ROWS, i64::from(first_page)],
            )
            .await
            .map_err(|error| format!("could not read source table {table}: {error}"))?;
        let mut page_rows = 0_i64;
        while let Some(row) = source_rows
            .next()
            .await
            .map_err(|error| format!("could not read source row from {table}: {error}"))?
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
            let mut values = Vec::with_capacity(columns.len());
            for index in 0..columns.len() {
                values.push(row.get::<Value>((index + 1) as i32).map_err(|error| {
                    format!("could not decode source row from {table}: {error}")
                })?);
            }
            transform(&columns, &mut values)?;
            inserted += insert_row_or_skip_exact(target, table, &columns, &values).await?;
        }
        if page_rows < MIGRATION_QUERY_PAGE_ROWS {
            break;
        }
        first_page = false;
    }
    Ok(inserted)
}

pub fn remap_store_id_columns(
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

pub fn remap_summary_source(
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

pub async fn copy_raw_messages<S, T>(
    source: &S,
    target: &T,
) -> Result<(u64, HashMap<i64, i64>), String>
where
    S: QueryExecutor + ?Sized,
    T: Executor + ?Sized,
{
    let source_columns = table_columns(source, "lcm_raw_messages").await?;
    if source_columns.is_empty() {
        return Ok((0, HashMap::new()));
    }
    if !source_columns.iter().any(|column| column == "store_id") {
        return Err("source lcm_raw_messages has no store_id".to_string());
    }
    let target_columns = table_columns(target, "lcm_raw_messages").await?;
    if target_columns.is_empty() {
        return Err("target is missing required table lcm_raw_messages".to_string());
    }
    let columns = compatible_columns(
        source_columns,
        &target_columns,
        &["store_id"],
        "lcm_raw_messages",
    )?;
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
    let mut inserted = 0;
    let mut id_map = HashMap::new();
    let mut last_store_id = i64::MIN;
    let mut first_page = true;
    loop {
        let select_sql = format!(
            "SELECT store_id, {quoted} FROM lcm_raw_messages
             WHERE store_id > ?1 OR (?3 = 1 AND store_id = ?1)
             ORDER BY store_id LIMIT ?2"
        );
        let mut rows = source
            .query(
                &select_sql,
                params![
                    last_store_id,
                    MIGRATION_QUERY_PAGE_ROWS,
                    i64::from(first_page)
                ],
            )
            .await
            .map_err(|error| format!("could not read source raw messages: {error}"))?;
        let mut page_rows = 0_i64;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("could not read source raw message: {error}"))?
        {
            let source_id: i64 = row
                .get(0)
                .map_err(|error| format!("invalid source raw store_id: {error}"))?;
            if source_id < last_store_id
                || (source_id == last_store_id && (!first_page || page_rows > 0))
            {
                return Err("source raw messages returned an unstable store_id order".to_string());
            }
            last_store_id = source_id;
            page_rows += 1;
            let provider: String = row
                .get((provider_index + 1) as i32)
                .map_err(|error| format!("invalid source raw provider: {error}"))?;
            let message_id: String = row
                .get((message_index + 1) as i32)
                .map_err(|error| format!("invalid source raw message_id: {error}"))?;
            let mut values = Vec::with_capacity(columns.len());
            for index in 0..columns.len() {
                values
                    .push(row.get::<Value>((index + 1) as i32).map_err(|error| {
                        format!("could not decode source raw message: {error}")
                    })?);
            }
            inserted +=
                insert_row_or_skip_exact(target, "lcm_raw_messages", &columns, &values).await?;
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
            ensure_materialized_row_room(id_map.len(), "raw-message identity map")?;
            id_map.insert(source_id, target_id);
        }
        if page_rows < MIGRATION_QUERY_PAGE_ROWS {
            break;
        }
        first_page = false;
    }
    Ok((inserted, id_map))
}

fn hash_file_bounded(path: &Path, expected_bytes: u64) -> Result<String, String> {
    if expected_bytes > MAX_MIGRATED_PAYLOAD_BYTES {
        return Err(format!(
            "payload '{}' exceeds the migration byte ceiling",
            path.display()
        ));
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("could not inspect payload '{}': {error}", path.display()))?;
    if !metadata.file_type().is_file() || metadata.len() != expected_bytes {
        return Err(format!(
            "payload '{}' has inconsistent file metadata",
            path.display()
        ));
    }
    let mut file = fs::File::open(path)
        .map_err(|error| format!("could not open payload '{}': {error}", path.display()))?;
    let mut hash = Sha256::new();
    let mut read_bytes = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("could not read payload '{}': {error}", path.display()))?;
        if read == 0 {
            break;
        }
        read_bytes = read_bytes.saturating_add(read as u64);
        if read_bytes > expected_bytes {
            return Err(format!(
                "payload '{}' changed while reading",
                path.display()
            ));
        }
        hash.update(&buffer[..read]);
    }
    if read_bytes != expected_bytes {
        return Err(format!(
            "payload '{}' changed while reading",
            path.display()
        ));
    }
    Ok(hex::encode(hash.finalize()))
}

fn copy_file_bounded(
    source: &Path,
    target: &mut fs::File,
    expected_bytes: u64,
) -> Result<String, String> {
    if expected_bytes > MAX_MIGRATED_PAYLOAD_BYTES {
        return Err(format!(
            "payload '{}' exceeds the migration byte ceiling",
            source.display()
        ));
    }
    let metadata = fs::symlink_metadata(source).map_err(|error| {
        format!(
            "source payload '{}' is unavailable: {error}",
            source.display()
        )
    })?;
    if !metadata.file_type().is_file() || metadata.len() != expected_bytes {
        return Err(format!(
            "source payload '{}' has inconsistent file metadata",
            source.display()
        ));
    }
    let mut source_file = fs::File::open(source).map_err(|error| {
        format!(
            "could not open source payload '{}': {error}",
            source.display()
        )
    })?;
    let mut hash = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let read = source_file.read(&mut buffer).map_err(|error| {
            format!(
                "could not read source payload '{}': {error}",
                source.display()
            )
        })?;
        if read == 0 {
            break;
        }
        copied = copied.saturating_add(read as u64);
        if copied > expected_bytes {
            return Err(format!(
                "source payload '{}' changed while reading",
                source.display()
            ));
        }
        hash.update(&buffer[..read]);
        target
            .write_all(&buffer[..read])
            .map_err(|error| format!("could not persist target payload: {error}"))?;
    }
    if copied != expected_bytes {
        return Err(format!(
            "source payload '{}' changed while reading",
            source.display()
        ));
    }
    Ok(hex::encode(hash.finalize()))
}

pub async fn copy_external_payload_files<S>(
    source: &S,
    source_db_path: &Path,
    target_db_path: &Path,
    created: &mut Vec<PathBuf>,
) -> Result<(), String>
where
    S: QueryExecutor + ?Sized,
{
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
    let mut last_payload_ref = String::new();
    let mut first_page = true;
    loop {
        let mut rows = source
            .query(
                "SELECT payload_ref, content_hash, byte_count
                 FROM lcm_external_payloads
                 WHERE payload_ref > ?1 OR (?3 = 1 AND payload_ref = ?1)
                 ORDER BY payload_ref LIMIT ?2",
                params![
                    last_payload_ref.as_str(),
                    MIGRATION_QUERY_PAGE_ROWS,
                    i64::from(first_page)
                ],
            )
            .await
            .map_err(|error| format!("could not inspect source payloads: {error}"))?;
        let mut page_rows = 0_i64;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("could not read source payload: {error}"))?
        {
            let payload_ref: String = row
                .get(0)
                .map_err(|error| format!("invalid source payload ref: {error}"))?;
            if payload_ref < last_payload_ref
                || (payload_ref == last_payload_ref && (!first_page || page_rows > 0))
            {
                return Err("source payloads returned an unstable payload_ref order".to_string());
            }
            last_payload_ref.clone_from(&payload_ref);
            page_rows += 1;
            let expected_hash: String = row
                .get(1)
                .map_err(|error| format!("invalid source payload hash: {error}"))?;
            let expected_bytes = row
                .get::<i64>(2)
                .map_err(|error| format!("invalid source payload byte count: {error}"))?;
            let expected_bytes = u64::try_from(expected_bytes)
                .map_err(|_| "source payload has a negative byte count".to_string())?;
            crate::root_seam::sessions::lcm::payload::validate_payload_ref(&payload_ref)
                .map_err(|error| format!("unsafe source payload ref '{payload_ref}': {error}"))?;
            let source_file = source_dir.join(&payload_ref);
            fs::create_dir_all(&target_dir)
                .map_err(|error| format!("could not create target payload directory: {error}"))?;
            let target_metadata = fs::symlink_metadata(&target_dir)
                .map_err(|error| format!("could not inspect target payload directory: {error}"))?;
            if !target_metadata.file_type().is_dir() {
                return Err("target payload directory is not a regular directory".to_string());
            }
            let target_file = target_dir.join(&payload_ref);
            if target_file.exists() {
                if hash_file_bounded(&source_file, expected_bytes)? != expected_hash {
                    return Err(format!(
                        "source payload '{}' failed its content hash",
                        source_file.display()
                    ));
                }
                if hash_file_bounded(&target_file, expected_bytes)? != expected_hash {
                    return Err(format!(
                        "target payload '{}' conflicts with the legacy source",
                        target_file.display()
                    ));
                }
                continue;
            }
            ensure_materialized_row_room(created.len(), "created payload list")?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target_file)
                .map_err(|error| format!("could not create target payload: {error}"))?;
            created.push(target_file.clone());
            let actual_hash = copy_file_bounded(&source_file, &mut file, expected_bytes)?;
            if actual_hash != expected_hash {
                return Err(format!(
                    "source payload '{}' failed its content hash",
                    source_file.display()
                ));
            }
            file.sync_all()
                .map_err(|error| format!("could not persist target payload: {error}"))?;
        }
        if page_rows < MIGRATION_QUERY_PAGE_ROWS {
            break;
        }
        first_page = false;
    }
    Ok(())
}

pub fn remove_created_payloads(paths: &[PathBuf]) {
    for path in paths.iter().rev() {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatible_columns_rejects_source_data_the_target_cannot_represent() {
        let error = compatible_columns(
            vec!["provider".into(), "future_data".into()],
            &["provider".into()],
            &[],
            "sessions",
        )
        .unwrap_err();
        assert!(error.contains("future_data"), "{error}");
    }

    #[test]
    fn compatible_columns_preserves_older_sources_and_explicit_exclusions() {
        let columns = compatible_columns(
            vec!["store_id".into(), "provider".into()],
            &["store_id".into(), "provider".into(), "current_only".into()],
            &["store_id"],
            "lcm_raw_messages",
        )
        .unwrap();
        assert_eq!(columns, ["provider"]);
    }

    #[test]
    fn materialized_collections_have_a_hard_row_ceiling() {
        assert!(
            ensure_materialized_row_room(MAX_MIGRATION_MATERIALIZED_ROWS - 1, "test map").is_ok()
        );
        let error =
            ensure_materialized_row_room(MAX_MIGRATION_MATERIALIZED_ROWS, "test map").unwrap_err();
        assert!(error.contains("materialization ceiling"), "{error}");
    }

    #[test]
    fn payload_hashing_rejects_oversized_materialization_before_open() {
        let error =
            hash_file_bounded(Path::new("unused"), MAX_MIGRATED_PAYLOAD_BYTES + 1).unwrap_err();
        assert!(error.contains("byte ceiling"), "{error}");
    }
}
