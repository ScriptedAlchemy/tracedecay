//! Bounded, lossless removal of old replay strings duplicated by raw bodies.
//!
//! The raw reader verifies canonical bytes and their sanitizer receipt before
//! a duplicate is removed. Structured replay content and external/media
//! representations keep their own content. A durable rowid frontier and a byte
//! budget keep this repair off the foreground ingest path.

use std::collections::BTreeSet;

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
// held. Eligibility parsing has its own equal byte budget: ineligible rows
// still cost JSON work. Ingestion caps each metadata envelope at 1 MiB.
const MAX_BATCH_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayMetadataCompactionReport {
    pub rows_scanned: u64,
    /// Transactional store-id frontier consumed by this page, absent on idle reads.
    pub frontier_before: Option<i64>,
    pub frontier_after: Option<i64>,
    pub rows_compacted: u64,
    /// Rows retained because their representation, sanitizer result, or byte
    /// bound could not prove this narrow lossless transformation.
    pub rows_skipped: u64,
    /// Subset of skipped rows whose metadata plus canonical/stored body
    /// representations exceed this pass's 4 MiB read and verification budget.
    pub rows_skipped_oversized: u64,
    /// Metadata bytes admitted to SQL eligibility parsing, including ineligible
    /// rows and any prefetched rows deferred by the separate body byte budget.
    pub metadata_bytes_examined: u64,
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
    metadata_oversized: bool,
    read_bytes: Option<u64>,
}

/// Admit a size-bounded prefix before parsing any metadata. Eligibility must
/// not parse all 48 documents before the body's separate byte budget is applied.
async fn candidates(
    conn: &(impl QueryExecutor + ?Sized),
    after: i64,
    limit: usize,
) -> Result<(Vec<Candidate>, u64), LcmError> {
    let mut rows = conn
        .query(
            "SELECT r.store_id, r.storage_kind = 'inline',
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
    let mut eligibility_ids = Vec::new();
    let mut remaining = MAX_BATCH_BYTES;
    while let Some(row) = rows.next().await? {
        let store_id: i64 = row.get(0)?;
        let inline = row.get::<i64>(1)? != 0;
        let metadata: Option<i64> = row.get(2)?;
        let body: Option<i64> = row.get(3)?;
        let stored: i64 = row.get(4)?;
        let metadata_bytes = if inline {
            u64::try_from(metadata.unwrap_or(0)).map_err(db_error)?
        } else {
            0
        };
        let metadata_oversized = metadata_bytes > MAX_BATCH_BYTES;
        if !metadata_oversized {
            if metadata_bytes > remaining {
                break;
            }
            remaining -= metadata_bytes;
            if inline && metadata.is_some() {
                eligibility_ids.push(store_id);
            }
        }
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
            store_id,
            eligible: false,
            metadata_oversized,
            read_bytes,
        });
    }
    drop(rows);
    // Resolve the admitted prefix in one query, not one query per raw row.
    // Both reads share the writer transaction, so sizes and identities cannot
    // change between admission and parsing. Oversized documents stay untouched.
    if !eligibility_ids.is_empty() {
        let ids = serde_json::to_string(&eligibility_ids).map_err(db_error)?;
        let mut rows = conn
            .query(
                "SELECT store_id FROM lcm_raw_messages
                 WHERE store_id IN (SELECT value FROM json_each(?1))
                   AND CASE WHEN storage_kind = 'inline' AND json_valid(metadata_json)
                            THEN json_type(metadata_json, '$.lcm_active_replay') = 'true'
                             AND json_type(metadata_json, '$.active_replay.content') = 'text'
                            ELSE 0 END",
                params![ids],
            )
            .await?;
        let mut eligible = BTreeSet::new();
        while let Some(row) = rows.next().await? {
            eligible.insert(row.get::<i64>(0)?);
        }
        for candidate in &mut result {
            candidate.eligible = eligible.contains(&candidate.store_id);
        }
    }
    Ok((result, MAX_BATCH_BYTES - remaining))
}

#[tracing::instrument(
    name = "lcm.retention.replay_metadata.page",
    level = "trace",
    skip_all,
    fields(metadata_bytes_examined = tracing::field::Empty),
    err
)]
async fn step(
    conn: &(impl Executor + ?Sized),
    max_rows: usize,
) -> Result<ReplayMetadataCompactionReport, LcmError> {
    let mut after = frontier(conn).await?;
    let before = after;
    let (candidates, metadata_bytes_examined) = candidates(conn, after, max_rows).await?;
    tracing::Span::current().record("metadata_bytes_examined", metadata_bytes_examined);
    let mut report = ReplayMetadataCompactionReport {
        metadata_bytes_examined,
        ..ReplayMetadataCompactionReport::default()
    };
    let mut remaining = MAX_BATCH_BYTES;
    let mut guard_suspended = false;
    for candidate in candidates {
        if candidate.metadata_oversized {
            report.rows_skipped += 1;
            report.rows_skipped_oversized += 1;
        } else if candidate.eligible {
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
        schema::set_gc_meta(conn, FRONTIER_KEY, &after.to_string()).await?;
        // An advancing local key is not proof that its checkpoint was stored.
        // Verify inside the same writer so a refused or rewritten checkpoint
        // rolls back this page instead of committing a false progress report.
        let persisted = frontier(conn).await?;
        if persisted != after {
            return Err(db_error(format!(
                "replay metadata frontier was not persisted: expected {after}, found {persisted}"
            )));
        }
        report.frontier_before = Some(before);
        report.frontier_after = Some(persisted);
    }
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
