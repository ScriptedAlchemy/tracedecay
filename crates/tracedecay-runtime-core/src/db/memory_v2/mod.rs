//! Owner-scoped V2 fact lineage schema and bounded legacy backfill.

use serde::Serialize;
use serde_json::Value;
use tracedecay_domain::{
    FactAssertionId, FactEventId, FactId, FactOwnerV1, PayloadAccessState, SourceStoreId, UtcMicros,
};

use crate::db::engine::{self, Executor, params};
use crate::errors::{Result, TraceDecayError};
use crate::memory::types::MemoryCategory;
use crate::privacy::sanitize_provider_metadata_text;
use crate::tracedecay::current_timestamp;

mod archive;
mod backfill;
mod cutover;
mod repair;
mod schema;
#[cfg(test)]
mod tests;
mod types;
mod writers;

pub use archive::{
    MemoryV2ArchiveDatabase, export_memory_v2_owner_archive, import_memory_v2_owner_archive,
    list_memory_v2_archive_owners, plan_memory_v2_owner_archive_import,
};
pub(super) use cutover::{
    backfill_memory_v2_batch, finalize_memory_v2_cutover, load_or_capture_memory_v2_frontiers,
    reopen_memory_v2_cutover_for_legacy_union,
};
pub(super) use repair::{
    feedback_history_repair_progress, repair_memory_v2_feedback_history_batch_in_transaction,
};
pub(in crate::db) use schema::create_schema;
pub(super) use schema::{
    install_v22_fresh_schema, install_v23_fresh_schema, upgrade_v20_schema, upgrade_v21_schema,
    upgrade_v22_schema, upgrade_v23_schema,
};
pub use types::{
    CapturedMemoryV2Frontiers, MemoryV2BackfillBatchOutcome, MemoryV2CutoverOutcome,
    MemoryV2CutoverReceipt, MemoryV2FeedbackHistoryRepairBatchOutcome,
    MemoryV2FeedbackHistoryRepairProgress,
};
use types::{CurrentFactState, OwnerKey, Progress};
pub use writers::MemoryV2LegacyPurgeReceipt;
#[cfg(test)]
pub(super) use writers::purge_memory_v2_fact;
pub(super) use writers::purge_memory_v2_fact_in_transaction;
pub(super) use writers::{
    clear_memory_v2_compatibility_bank_dirty_in_transaction,
    delete_memory_v2_compatibility_bank_in_transaction,
    mark_memory_v2_compatibility_bank_dirty_in_transaction,
    upsert_memory_v2_compatibility_bank_in_transaction,
};

const OPERATION: &str = "memory_v2_backfill_v1";
const MAX_BATCH_SIZE: i64 = 500;
const MAX_FEEDBACK_HISTORY_REPAIR_BATCH_SIZE: i64 = 512;
const RETENTION_CLASS: &str = "legacy-current-snapshot-v1";
const V1_COMPATIBILITY_SOURCE_STORE: &str = "legacy-memory-v1";
const V23_COMPATIBILITY_BANK_VECTOR_BYTES: usize = 8 + 2048 * 4;
const V23_COMPATIBILITY_BANK_VECTOR_HEADER: [u8; 8] = 2048_u64.to_le_bytes();

pub(in crate::db) trait MemoryV2Executor: Executor + Sync {}

impl<T> MemoryV2Executor for T where T: Executor + Sync + ?Sized {}

async fn load_or_create_progress(
    conn: &impl MemoryV2Executor,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
    frontiers: CapturedMemoryV2Frontiers,
) -> Result<Progress> {
    let mut rows = conn
        .query(
            "SELECT phase, feedback_frontier, oplog_frontier, fact_frontier,
                    feedback_cursor, oplog_cursor, fact_cursor, started_at
             FROM memory_v2_backfill_progress
             WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3",
            params![
                owner.kind,
                owner.project_id.as_str(),
                source_store_id.as_str()
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    if let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        let progress = Progress {
            phase: row.get(0).map_err(|error| db_error(OPERATION, error))?,
            feedback_frontier: row.get(1).map_err(|error| db_error(OPERATION, error))?,
            oplog_frontier: row.get(2).map_err(|error| db_error(OPERATION, error))?,
            fact_frontier: row.get(3).map_err(|error| db_error(OPERATION, error))?,
            feedback_cursor: row.get(4).map_err(|error| db_error(OPERATION, error))?,
            oplog_cursor: row.get(5).map_err(|error| db_error(OPERATION, error))?,
            fact_cursor: row.get(6).map_err(|error| db_error(OPERATION, error))?,
            started_at: row.get(7).map_err(|error| db_error(OPERATION, error))?,
        };
        if (
            progress.feedback_frontier,
            progress.oplog_frontier,
            progress.fact_frontier,
        ) != (frontiers.feedback, frontiers.oplog, frontiers.facts)
        {
            return Err(db_message(
                OPERATION,
                "captured backfill frontier changed across retry",
            ));
        }
        return Ok(progress);
    }
    let started_at = now_micros()?;
    conn.execute(
        "INSERT INTO memory_v2_backfill_progress(
            owner_kind, project_id, owner_json, source_store_id, phase,
            feedback_frontier, oplog_frontier, fact_frontier, started_at, updated_at
         ) VALUES(?1, ?2, ?3, ?4, 'feedback', ?5, ?6, ?7, ?8, ?8)",
        params![
            owner.kind,
            owner.project_id.as_str(),
            owner.json.as_str(),
            source_store_id.as_str(),
            frontiers.feedback,
            frontiers.oplog,
            frontiers.facts,
            started_at
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(Progress {
        phase: "feedback".to_owned(),
        feedback_frontier: frontiers.feedback,
        oplog_frontier: frontiers.oplog,
        fact_frontier: frontiers.facts,
        feedback_cursor: 0,
        oplog_cursor: 0,
        fact_cursor: 0,
        started_at,
    })
}

async fn update_phase(
    conn: &impl MemoryV2Executor,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
    phase: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE memory_v2_backfill_progress SET phase = ?1, updated_at = ?2
         WHERE owner_kind = ?3 AND project_id = ?4 AND source_store_id = ?5",
        params![
            phase,
            now_micros()?,
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str()
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

async fn update_cursor(
    conn: &impl MemoryV2Executor,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
    column: &str,
    cursor: i64,
) -> Result<()> {
    let sql = match column {
        "feedback_cursor" => {
            "UPDATE memory_v2_backfill_progress
             SET feedback_cursor = ?1, updated_at = ?2
             WHERE owner_kind = ?3 AND project_id = ?4 AND source_store_id = ?5"
        }
        "oplog_cursor" => {
            "UPDATE memory_v2_backfill_progress
             SET oplog_cursor = ?1, updated_at = ?2
             WHERE owner_kind = ?3 AND project_id = ?4 AND source_store_id = ?5"
        }
        "fact_cursor" => {
            "UPDATE memory_v2_backfill_progress
             SET fact_cursor = ?1, updated_at = ?2
             WHERE owner_kind = ?3 AND project_id = ?4 AND source_store_id = ?5"
        }
        _ => return Err(db_message(OPERATION, "invalid backfill cursor column")),
    };
    conn.execute(
        sql,
        params![
            cursor,
            now_micros()?,
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str()
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

async fn current_fact_state(
    conn: &impl MemoryV2Executor,
    owner: &OwnerKey,
    fact_id: &FactId,
) -> Result<CurrentFactState> {
    let mut rows = conn
        .query(
            "SELECT current.payload_access, current.last_event_id,
                current.active_assertion_id, assertion.kind_json,
                assertion.payload_reference_json
         FROM memory_v2_current_facts current
         LEFT JOIN memory_v2_assertions assertion
           ON assertion.assertion_id = current.active_assertion_id
          AND assertion.fact_id = current.fact_id
          AND assertion.owner_kind = current.owner_kind
          AND assertion.project_id = current.project_id
         WHERE current.fact_id = ?1
           AND current.owner_kind = ?2 AND current.project_id = ?3",
            params![fact_id.as_str(), owner.kind, owner.project_id.as_str()],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
        .ok_or_else(|| db_message(OPERATION, "current fact projection is missing"))?;
    let access = row
        .get::<String>(0)
        .map_err(|error| db_error(OPERATION, error))?;
    let event_id = FactEventId::new(
        row.get::<String>(1)
            .map_err(|error| db_error(OPERATION, error))?,
    )
    .map_err(|_| db_message(OPERATION, "stored last event identity is invalid"))?;
    let active_assertion_id = row
        .get::<Option<String>>(2)
        .map_err(|error| db_error(OPERATION, error))?
        .map(FactAssertionId::new)
        .transpose()
        .map_err(|_| db_message(OPERATION, "stored active assertion identity is invalid"))?;
    let active_kind = row
        .get::<Option<String>>(3)
        .map_err(|error| db_error(OPERATION, error))?
        .map(|value| serde_json::from_str(&value))
        .transpose()
        .map_err(|_| db_message(OPERATION, "stored assertion kind is invalid"))?;
    let active_payload_reference = row
        .get::<Option<String>>(4)
        .map_err(|error| db_error(OPERATION, error))?
        .map(|value| serde_json::from_str(&value))
        .transpose()
        .map_err(|_| db_message(OPERATION, "stored payload reference is invalid"))?;
    Ok(CurrentFactState {
        access: parse_payload_access(&access)?,
        last_event_id: event_id,
        active_assertion_id,
        active_kind,
        active_payload_reference,
    })
}

async fn load_legacy_entities(
    conn: &impl MemoryV2Executor,
    legacy_fact_id: i64,
) -> Result<Vec<String>> {
    const PAGE_SIZE: i64 = 512;

    let mut entities = Vec::new();
    let mut name_cursor: Option<String> = None;
    let mut id_cursor: Option<i64> = None;
    loop {
        let mut rows = conn
            .query(
                "SELECT e.name, e.entity_id FROM memory_entities e
                 JOIN memory_fact_entities fe ON fe.entity_id = e.entity_id
                 WHERE fe.fact_id = ?1
                   AND (
                       ?2 IS NULL
                       OR e.name > ?2
                       OR (e.name = ?2 AND e.entity_id > ?3)
                   )
                 ORDER BY e.name, e.entity_id
                 LIMIT ?4",
                params![legacy_fact_id, name_cursor.as_ref(), id_cursor, PAGE_SIZE],
            )
            .await
            .map_err(|error| db_error(OPERATION, error))?;
        let mut page_count = 0;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| db_error(OPERATION, error))?
        {
            let name: String = row.get(0).map_err(|error| db_error(OPERATION, error))?;
            id_cursor = Some(row.get(1).map_err(|error| db_error(OPERATION, error))?);
            name_cursor = Some(name.clone());
            entities.push(name);
            page_count += 1;
        }
        if page_count < PAGE_SIZE {
            break;
        }
    }
    Ok(entities)
}

async fn load_legacy_entity_ids(
    conn: &impl MemoryV2Executor,
    legacy_fact_id: i64,
    operation: &str,
) -> Result<Vec<i64>> {
    const PAGE_SIZE: i64 = 512;

    let mut entity_ids = Vec::new();
    let mut cursor: Option<i64> = None;
    loop {
        let mut rows = conn
            .query(
                "SELECT entity_id FROM memory_fact_entities
                 WHERE fact_id = ?1 AND (?2 IS NULL OR entity_id > ?2)
                 ORDER BY entity_id
                 LIMIT ?3",
                params![legacy_fact_id, cursor, PAGE_SIZE],
            )
            .await
            .map_err(|error| db_error(operation, error))?;
        let mut page_count = 0;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| db_error(operation, error))?
        {
            let entity_id = row.get(0).map_err(|error| db_error(operation, error))?;
            cursor = Some(entity_id);
            entity_ids.push(entity_id);
            page_count += 1;
        }
        if page_count < PAGE_SIZE {
            break;
        }
    }
    Ok(entity_ids)
}

fn owner_key(owner: &FactOwnerV1) -> Result<OwnerKey> {
    owner
        .validate()
        .map_err(|_| db_message(OPERATION, "fact owner is invalid"))?;
    let (kind, project_id) = match owner {
        FactOwnerV1::Profile => ("profile", String::new()),
        FactOwnerV1::Project { project_id } => ("project", project_id.as_str().to_owned()),
    };
    Ok(OwnerKey {
        kind,
        project_id,
        json: json_text(owner)?,
    })
}

fn validate_scope(owner: &FactOwnerV1, source_store_id: &SourceStoreId) -> Result<()> {
    owner
        .validate()
        .map_err(|_| db_message(OPERATION, "fact owner is invalid"))?;
    source_store_id
        .validate()
        .map_err(|_| db_message(OPERATION, "source store identity is invalid"))?;
    Ok(())
}

fn validate_v1_compatibility_source(source_store_id: &SourceStoreId) -> Result<()> {
    if source_store_id.as_str() == V1_COMPATIBILITY_SOURCE_STORE {
        Ok(())
    } else {
        Err(db_message(
            OPERATION,
            "V1 compatibility mappings require the fixed legacy-memory-v1 source store",
        ))
    }
}

fn validate_frontiers(frontiers: CapturedMemoryV2Frontiers) -> Result<()> {
    if frontiers.feedback < 0 || frontiers.oplog < 0 || frontiers.facts < 0 {
        Err(db_message(
            OPERATION,
            "backfill frontier cannot be negative",
        ))
    } else {
        Ok(())
    }
}

/// Parses a durably stored V1 category label. Only the exact canonical
/// spellings round-trip; aliases stay a parse failure so a legacy row is
/// skipped rather than reinterpreted.
fn parse_category(value: &str) -> std::result::Result<tracedecay_domain::FactCategoryV1, ()> {
    MemoryCategory::from_canonical_label(value)
        .map(tracedecay_domain::FactCategoryV1::from)
        .ok_or(())
}

fn category_label(category: tracedecay_domain::FactCategoryV1) -> &'static str {
    MemoryCategory::from(category).as_str()
}

fn payload_access_label(state: PayloadAccessState) -> &'static str {
    match state {
        PayloadAccessState::Eligible => "eligible",
        PayloadAccessState::Redacted => "redacted",
        PayloadAccessState::Quarantined => "quarantined",
        PayloadAccessState::RetentionExpired => "retention_expired",
        PayloadAccessState::Deleted => "deleted",
        PayloadAccessState::Unavailable => "unavailable",
        PayloadAccessState::Ambiguous => "ambiguous",
    }
}

fn parse_payload_access(value: &str) -> Result<PayloadAccessState> {
    match value {
        "eligible" => Ok(PayloadAccessState::Eligible),
        "redacted" => Ok(PayloadAccessState::Redacted),
        "quarantined" => Ok(PayloadAccessState::Quarantined),
        "retention_expired" => Ok(PayloadAccessState::RetentionExpired),
        "deleted" => Ok(PayloadAccessState::Deleted),
        "unavailable" => Ok(PayloadAccessState::Unavailable),
        "ambiguous" => Ok(PayloadAccessState::Ambiguous),
        _ => Err(db_message(
            OPERATION,
            "stored payload access state is invalid",
        )),
    }
}

fn value_strings(value: &Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}

fn seconds_to_micros(seconds: i64) -> Option<UtcMicros> {
    seconds.checked_mul(1_000_000).map(UtcMicros)
}

fn sanitize_legacy_feedback_details(
    source: Option<&str>,
    note: Option<&str>,
) -> (
    Option<String>,
    Option<String>,
    &'static str,
    Option<&'static str>,
) {
    let Some(source) = source else {
        return (None, None, "unknown", Some("feedback_details_unknown"));
    };
    let Some(source) = sanitize_provider_metadata_text(source) else {
        return (
            None,
            None,
            "legacy_redacted",
            Some("feedback_details_redacted"),
        );
    };
    let note = match note {
        Some(note) => match sanitize_provider_metadata_text(note) {
            Some(note) => Some(note),
            None => {
                return (
                    None,
                    None,
                    "legacy_redacted",
                    Some("feedback_details_redacted"),
                );
            }
        },
        None => None,
    };
    (Some(source), note, "available", None)
}

fn now_micros() -> Result<i64> {
    current_timestamp()
        .checked_mul(1_000_000)
        .ok_or_else(|| db_message(OPERATION, "current timestamp is outside supported range"))
}

fn json_text(value: &(impl Serialize + ?Sized)) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|_| db_message(OPERATION, "canonical JSON encoding failed"))
}

fn canonical_replay(existing: String, candidate: &str, record: &str) -> Result<()> {
    if existing == candidate {
        Ok(())
    } else {
        Err(db_message(
            OPERATION,
            format!("{record} identity collision"),
        ))
    }
}

/// A cutover command's identity is its receipt id, owner/source, and drained
/// frontier. Its completion timestamp is generated by the first successful
/// finalization and must not make a retry collide with that completed receipt.
fn canonical_cutover_replay(existing: String, candidate: &str) -> Result<()> {
    canonical_replay(
        cutover_replay_identity(&existing)?,
        &cutover_replay_identity(candidate)?,
        "cutover receipt",
    )
}

fn cutover_replay_identity(receipt_json: &str) -> Result<String> {
    let mut receipt: Value = serde_json::from_str(receipt_json)
        .map_err(|_| db_message(OPERATION, "stored cutover receipt is invalid JSON"))?;
    let object = receipt
        .as_object_mut()
        .ok_or_else(|| db_message(OPERATION, "stored cutover receipt is not an object"))?;
    if object.remove("dual_write_activated_at").is_none() {
        return Err(db_message(
            OPERATION,
            "stored cutover receipt lacks its completion timestamp",
        ));
    }
    object.remove("coverage");
    json_text(&receipt)
}

/// A legacy mapping's identity is (owner, source store, legacy fact) -> fact
/// id. Its import attributes (history coverage, `migrated_at`) are produced by
/// whichever path imported the fact first — the compatibility write path
/// records Complete/now while the backfill records `Unknown/started_at` — so a
/// replayed or duplicate import must not collide on them.
fn canonical_mapping_replay(existing: String, candidate: &str) -> Result<()> {
    canonical_replay(
        mapping_replay_identity(&existing)?,
        &mapping_replay_identity(candidate)?,
        "legacy mapping",
    )
}

fn mapping_replay_identity(mapping_json: &str) -> Result<String> {
    let mut mapping: Value = serde_json::from_str(mapping_json)
        .map_err(|_| db_message(OPERATION, "stored legacy mapping is invalid JSON"))?;
    let object = mapping
        .as_object_mut()
        .ok_or_else(|| db_message(OPERATION, "stored legacy mapping is not an object"))?;
    object.remove("history_coverage");
    object.remove("migrated_at");
    json_text(&mapping)
}

async fn begin(conn: &engine::Connection, operation: &str) -> Result<engine::Transaction> {
    conn.transaction_with_behavior(engine::TransactionBehavior::Immediate)
        .await
        .map_err(|error| db_error(operation, error))
}

async fn finish_transaction<T>(
    transaction: engine::Transaction,
    result: Result<T>,
    operation: &str,
) -> Result<T> {
    match result {
        Ok(value) => match transaction.commit().await {
            Ok(()) => Ok(value),
            Err(commit_error) => Err(db_message(
                operation,
                format!("commit failed; writer transaction retired: {commit_error}"),
            )),
        },
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(error)
        }
    }
}

async fn scalar_i64(conn: &impl MemoryV2Executor, sql: &str) -> Result<i64> {
    scalar_i64_params(conn, sql, ()).await
}

async fn scalar_i64_params(
    conn: &impl MemoryV2Executor,
    sql: &str,
    params: impl engine::IntoParams,
) -> Result<i64> {
    let mut rows = conn
        .query(sql, params)
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    rows.next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
        .ok_or_else(|| db_message(OPERATION, "scalar query returned no row"))?
        .get(0)
        .map_err(|error| db_error(OPERATION, error))
}

async fn optional_string(
    conn: &impl MemoryV2Executor,
    sql: &str,
    params: impl engine::IntoParams,
) -> Result<Option<String>> {
    let mut rows = conn
        .query(sql, params)
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    rows.next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
        .map(|row| row.get(0).map_err(|error| db_error(OPERATION, error)))
        .transpose()
}

async fn optional_i64(
    conn: &impl MemoryV2Executor,
    sql: &str,
    params: impl engine::IntoParams,
) -> Result<Option<i64>> {
    let mut rows = conn
        .query(sql, params)
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    rows.next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
        .map(|row| row.get(0).map_err(|error| db_error(OPERATION, error)))
        .transpose()
}

async fn row_exists(
    conn: &impl MemoryV2Executor,
    sql: &str,
    params: impl engine::IntoParams,
) -> Result<bool> {
    let mut rows = conn
        .query(sql, params)
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
        .is_some())
}

fn db_error(operation: &str, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        message: format!("{operation}: storage operation failed: {error}"),
        operation: operation.to_owned(),
    }
}

fn db_message(operation: &str, message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Database {
        message: message.into(),
        operation: operation.to_owned(),
    }
}
