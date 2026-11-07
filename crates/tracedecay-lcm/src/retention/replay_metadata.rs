//! Bounded, lossless removal of old replay strings duplicated by raw bodies.
//!
//! The raw reader verifies canonical bytes and their sanitizer receipt before
//! a duplicate is removed. Structured replay content and external/media
//! representations keep their own content. A durable rowid frontier and a byte
//! budget keep this repair off the foreground ingest path.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};

use crate::{LcmError, LcmRawMessage, gc, payload, raw, schema, security, summary_convergence};

#[cfg(test)]
mod tests;

const FRONTIER_KEY: &str = "replay_metadata_compaction_store_id_v1";
const OPERATION: &str = "compact duplicate LCM replay metadata";
pub const MAX_REPLAY_METADATA_COMPACTION_ROWS: usize = 48;
// Bound both receipt verification and metadata sanitization while a writer is
// held. The raw metadata envelope itself is capped at 1 MiB by ingestion.
const MAX_BATCH_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayMetadataCompactionReport {
    pub rows_scanned: u64,
    /// Transactional store-id frontier consumed by this page, absent on idle reads.
    pub frontier_before: Option<i64>,
    pub frontier_after: Option<i64>,
    pub rows_compacted: u64,
    /// Eligible rows retained because their representation, sanitizer result,
    /// or byte bound could not prove this narrow lossless transformation.
    pub rows_skipped: u64,
    /// Subset of skipped rows whose metadata plus canonical/stored body
    /// representations exceed this pass's 4 MiB read and verification budget.
    pub rows_skipped_oversized: u64,
    /// UTF-8 metadata bytes removed. This does not measure file shrinkage.
    pub metadata_bytes_removed: u64,
    pub has_more: bool,
}

fn db_error(error: impl std::fmt::Display) -> LcmError {
    LcmError::Db(format!("{OPERATION}: {error}"))
}

async fn frontier(conn: &(impl QueryExecutor + ?Sized)) -> Result<i64, LcmError> {
    match schema::get_gc_meta(conn, FRONTIER_KEY).await? {
        None => Ok(0),
        Some(value) => value
            .parse::<i64>()
            .ok()
            .filter(|value| *value >= 0)
            .ok_or_else(|| db_error("invalid replay metadata compaction frontier")),
    }
}

async fn has_work(conn: &(impl QueryExecutor + ?Sized), after: i64) -> Result<bool, LcmError> {
    Ok(conn
        .query(
            "SELECT 1 FROM lcm_raw_messages WHERE store_id > ?1 LIMIT 1",
            params![after],
        )
        .await?
        .next()
        .await?
        .is_some())
}

/// Advances one bounded repair page under the registered database writer.
/// Body bytes, lineage and effective replay content remain unchanged.
pub async fn compact_replay_metadata(
    database: &Database,
    max_rows: usize,
) -> Result<ReplayMetadataCompactionReport, LcmError> {
    let read = database.read_connection();
    if !has_work(&read, frontier(&read).await?).await? {
        return Ok(ReplayMetadataCompactionReport::default());
    }
    let transaction = database
        .begin_write_transaction(OPERATION)
        .await
        .map_err(db_error)?;
    match step(&transaction, max_rows).await {
        Ok(report) => {
            transaction.commit().await.map_err(db_error)?;
            Ok(report)
        }
        Err(error) => match transaction.rollback().await {
            Ok(()) => Err(error),
            Err(rollback) => Err(db_error(format!("{error}; rollback failed: {rollback}"))),
        },
    }
}

struct Candidate {
    store_id: i64,
    eligible: bool,
    read_bytes: Option<u64>,
}

/// Read only sizes and identities until each candidate earns the byte budget.
async fn candidates(
    conn: &(impl QueryExecutor + ?Sized),
    after: i64,
    limit: usize,
) -> Result<Vec<Candidate>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT r.store_id,
                    CASE WHEN r.storage_kind = 'inline' AND json_valid(r.metadata_json)
                         THEN json_type(r.metadata_json, '$.lcm_active_replay') = 'true'
                          AND json_type(r.metadata_json, '$.active_replay.content') = 'text'
                         ELSE 0 END,
                    LENGTH(CAST(r.metadata_json AS BLOB)),
                    COALESCE(LENGTH(CAST(r.content AS BLOB)), b.uncompressed_bytes),
                    COALESCE(LENGTH(b.body), 0)
             FROM lcm_raw_messages r
             LEFT JOIN session_canonical_bodies b ON b.content_hash = r.content_hash
             WHERE r.store_id > ?1 ORDER BY r.store_id LIMIT ?2",
            params![
                after,
                limit.clamp(1, MAX_REPLAY_METADATA_COMPACTION_ROWS) as i64
            ],
        )
        .await?;
    let mut result = Vec::new();
    while let Some(row) = rows.next().await? {
        let metadata: Option<i64> = row.get(2)?;
        let body: Option<i64> = row.get(3)?;
        let stored: i64 = row.get(4)?;
        let read_bytes = metadata
            .zip(body)
            .and_then(|(metadata, body)| {
                Some((
                    u64::try_from(metadata).ok()?,
                    u64::try_from(body).ok()?,
                    u64::try_from(stored).ok()?,
                ))
            })
            .and_then(|(metadata, body, stored)| metadata.checked_add(body)?.checked_add(stored));
        result.push(Candidate {
            store_id: row.get(0)?,
            eligible: row.get::<Option<i64>>(1)?.unwrap_or(0) != 0,
            read_bytes,
        });
    }
    Ok(result)
}

async fn step(
    conn: &(impl Executor + ?Sized),
    max_rows: usize,
) -> Result<ReplayMetadataCompactionReport, LcmError> {
    let mut after = frontier(conn).await?;
    let before = after;
    let candidates = candidates(conn, after, max_rows).await?;
    let mut report = ReplayMetadataCompactionReport::default();
    let mut remaining = MAX_BATCH_BYTES;
    let mut guard_suspended = false;
    for candidate in candidates {
        if candidate.eligible {
            let bytes = candidate.read_bytes.ok_or_else(|| {
                db_error(format!(
                    "raw row {} has no bounded canonical body",
                    candidate.store_id
                ))
            })?;
            if bytes > MAX_BATCH_BYTES {
                report.rows_skipped += 1;
                report.rows_skipped_oversized += 1;
            } else if bytes > remaining {
                break;
            } else {
                remaining -= bytes;
                let message = raw::load_raw_message_by_store_id(conn, candidate.store_id).await?;
                if let Some(metadata) = compacted_metadata(&message)? {
                    if !guard_suspended {
                        summary_convergence::suspend_raw_update_guard(conn).await?;
                        guard_suspended = true;
                    }
                    let changed =
                        apply_metadata(conn, candidate.store_id, &message, &metadata).await?;
                    if changed {
                        report.rows_compacted += 1;
                        report.metadata_bytes_removed += message
                            .metadata_json
                            .as_deref()
                            .map_or(0, str::len)
                            .saturating_sub(metadata.len())
                            as u64;
                    } else {
                        report.rows_skipped += 1;
                    }
                } else {
                    report.rows_skipped += 1;
                }
            }
        }
        report.rows_scanned += 1;
        after = candidate.store_id;
    }
    if guard_suspended {
        summary_convergence::restore_raw_update_guard(conn).await?;
    }
    if report.rows_scanned > 0 {
        if after <= before {
            return Err(db_error(
                "replay metadata page did not advance its frontier",
            ));
        }
        report.frontier_before = Some(before);
        report.frontier_after = Some(after);
    }
    schema::set_gc_meta(conn, FRONTIER_KEY, &after.to_string()).await?;
    report.has_more = has_work(conn, after).await?;
    Ok(report)
}

fn compacted_metadata(message: &LcmRawMessage) -> Result<Option<String>, LcmError> {
    if message.storage_kind != crate::LcmStorageKind::Inline {
        return Ok(None);
    }
    let Some(stored) = message.metadata_json.as_deref() else {
        return Ok(None);
    };
    let mut metadata: Value = serde_json::from_str(stored).map_err(db_error)?;
    if metadata.get("lcm_active_replay").and_then(Value::as_bool) != Some(true) {
        return Ok(None);
    }
    let Some(replay) = metadata
        .get_mut("active_replay")
        .and_then(Value::as_object_mut)
    else {
        return Ok(None);
    };
    let Some(content) = replay.get("content").and_then(Value::as_str) else {
        return Ok(None);
    };
    if content != message.content
        || gc::is_known_payload_placeholder_prefix(content.trim_start())
        || !payload::extract_payload_refs_from_text(content).is_empty()
        || security::contains_media_payload(content)
        // Ingest's large-media detector intentionally requires substantial
        // base64 data. Compaction is stricter: even a tiny data: URI carries
        // a provider media representation that must remain in replay metadata.
        || content
            .as_bytes()
            .windows(5)
            .any(|bytes| bytes.eq_ignore_ascii_case(b"data:"))
    {
        return Ok(None);
    }
    replay.remove("content");
    let rebound = raw::resanitize_stored_provider_metadata_json(&metadata.to_string())?;
    let mut expected = metadata;
    let mut actual: Value = serde_json::from_str(&rebound).map_err(db_error)?;
    // Only the metadata receipt may change in addition to the proven duplicate.
    // New sanitizer policy changes belong to privacy remediation, not this pass.
    remove_metadata_receipt(&mut expected);
    remove_metadata_receipt(&mut actual);
    if expected != actual || rebound.len() >= stored.len() {
        return Ok(None);
    }
    Ok(Some(rebound))
}

fn remove_metadata_receipt(metadata: &mut Value) {
    if let Some(protection) = metadata
        .get_mut("ingest_protection")
        .and_then(Value::as_object_mut)
    {
        protection.remove("metadata_sanitization_receipt");
    }
}

async fn apply_metadata(
    conn: &(impl Executor + ?Sized),
    store_id: i64,
    message: &LcmRawMessage,
    metadata: &str,
) -> Result<bool, LcmError> {
    let changed = conn
        .execute(
            "UPDATE lcm_raw_messages SET metadata_json = ?1
             WHERE store_id = ?2 AND provider = ?3 AND session_id = ?4 AND message_id = ?5
               AND content_hash = ?6 AND metadata_json IS ?7 AND storage_kind = 'inline'
               AND role = ?8 AND ordinal = ?9 AND timestamp IS ?10 AND payload_ref IS NULL",
            params![
                metadata,
                store_id,
                message.provider.as_str(),
                message.session_id.as_str(),
                message.message_id.as_str(),
                message.content_hash.as_str(),
                message.metadata_json.as_deref(),
                message.role.as_str(),
                message.ordinal,
                message.timestamp
            ],
        )
        .await?;
    Ok(changed == 1)
}
