//! Compatibility feedback-history repair, missing-vector repair, and dirty-bank rebuilds.

use crate::db::Database;
use crate::memory::encoding::HolographicEncoder;

use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use serde_json::json;

use tracedecay_domain::{ActorId, FactId, FactOwnerV1, UtcMicros};
use tracedecay_store::{
    CompatibilityFactRepairVectorV1, CompatibilityFeedbackRepairProgressV1,
    CompatibilityMemoryRepairCommandV1, CompatibilityMemoryRepairStatsV1, FactCompatibilityResult,
    FactStoreError, FactStoreResult,
};

use super::crud::{
    compatibility_mark_owner_banks_dirty_tx, compatibility_mirror_vector, load_current_fact_tx,
};
use super::curation::{
    compatibility_available_curation_fact_tx, compatibility_curation_evidence_ids_tx,
};
use super::envelope::{
    compatibility_digest, compatibility_lookup_operation_receipt_tx, compatibility_receipt_u64,
    compatibility_record_operation_receipt_tx,
};
use super::primitives::{
    COMPATIBILITY_READ_OPERATION, COMPATIBILITY_WRITE_OPERATION, OwnerKey,
    compatibility_legacy_timestamp, compatibility_now, compatibility_source_store_id,
    nonnegative_u64, row_i64, row_string, storage_error, storage_message,
};
use super::projection::compatibility_required_mapping_tx;

/// Per-repair-pass batch caps. The daemon scheduler treats a pass that hits
/// either cap as incomplete and keeps ticking rather than going idle with a
/// converging backlog.
pub(crate) const COMPATIBILITY_REPAIR_VECTOR_BATCH: i64 = 512;

pub(crate) const COMPATIBILITY_REPAIR_BANK_BATCH: i64 = 32;

/// True when a repair pass filled either per-pass batch cap, so backlog may
/// remain behind the cap. Only the store computes this — it owns the caps — so
/// the daemon scheduler can consume [`CompatibilityMemoryRepairStatsV1::saturated`]
/// without depending on these store-internal constants.
fn compatibility_repair_batches_saturated(
    missing_vectors_repaired: u64,
    banks_rebuilt: u64,
) -> bool {
    missing_vectors_repaired >= COMPATIBILITY_REPAIR_VECTOR_BATCH as u64
        || banks_rebuilt >= COMPATIBILITY_REPAIR_BANK_BATCH as u64
}

pub(super) async fn compatibility_repair_vector_for_fact_tx(
    db: &Database,
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    operation: &CompatibilityFactRepairVectorV1,
    now: UtcMicros,
) -> FactStoreResult<FactId> {
    let _evidence =
        compatibility_curation_evidence_ids_tx(transaction, owner, operation.evidence_facts())
            .await?;
    let (fact_id, fact, mapping) =
        compatibility_available_curation_fact_tx(transaction, operation.fact()).await?;
    let payload = fact
        .payload()
        .ok_or(FactStoreError::PayloadAccessMismatch)?;
    let changed = transaction
        .execute(
            "UPDATE memory_facts SET
                hrr_vector = ?1, hrr_algebra = 'amari_fhrr', hrr_dim = ?2, hrr_precision = ?3,
                updated_at = ?4
             WHERE fact_id = ?5",
            params![
                compatibility_mirror_vector(payload)?,
                HolographicEncoder::DIMENSIONS as i64,
                HolographicEncoder::HRR_PRECISION,
                compatibility_legacy_timestamp(now),
                mapping
                    .legacy_fact_id()
                    .ok_or(FactStoreError::FactMismatch)?,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    if changed != 1 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility vector target is missing from the legacy mirror",
        ));
    }
    compatibility_mark_owner_banks_dirty_tx(db, transaction, owner, payload.category(), now)
        .await?;
    Ok(fact_id)
}

pub(super) fn compatibility_repair_request_digest(
    request: &CompatibilityMemoryRepairCommandV1,
) -> FactStoreResult<String> {
    compatibility_digest(json!({
        "owner": request.owner(),
        "actor": request.actor().map(ActorId::as_str),
    }))
}

pub(super) async fn repair_compatibility_memory_tx(
    db: &Database,
    transaction: &Transaction<'_>,
    request: &CompatibilityMemoryRepairCommandV1,
) -> FactCompatibilityResult<CompatibilityMemoryRepairStatsV1> {
    let request_digest = compatibility_repair_request_digest(request)?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "repair",
        &request_digest,
    )
    .await?
    {
        let missing_vectors_repaired =
            compatibility_receipt_u64(&receipt.receipt, "missing_vectors_repaired")?;
        let banks_rebuilt = compatibility_receipt_u64(&receipt.receipt, "banks_rebuilt")?;
        return Ok(
            CompatibilityMemoryRepairStatsV1::new(missing_vectors_repaired, banks_rebuilt)
                .with_saturated(compatibility_repair_batches_saturated(
                    missing_vectors_repaired,
                    banks_rebuilt,
                )),
        );
    }
    let now = compatibility_now()?;
    let missing_vectors_repaired = compatibility_repair_missing_vectors_tx(
        db,
        transaction,
        request.owner(),
        COMPATIBILITY_REPAIR_VECTOR_BATCH,
    )
    .await?;
    compatibility_mark_absent_banks_dirty_tx(db, transaction, request.owner(), now).await?;
    let banks_rebuilt =
        compatibility_rebuild_dirty_banks_tx(db, transaction, request.owner()).await?;
    let receipt = json!({
        "missing_vectors_repaired": missing_vectors_repaired,
        "banks_rebuilt": banks_rebuilt,
    });
    compatibility_record_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "repair",
        &request_digest,
        None,
        None,
        &receipt,
        now,
    )
    .await?;
    Ok(
        CompatibilityMemoryRepairStatsV1::new(missing_vectors_repaired, banks_rebuilt)
            .with_saturated(compatibility_repair_batches_saturated(
                missing_vectors_repaired,
                banks_rebuilt,
            )),
    )
}

pub(super) async fn compatibility_repair_missing_vectors_tx(
    db: &Database,
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    limit: i64,
) -> FactStoreResult<u64> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut rows = transaction
        .query(
            "SELECT mappings.fact_id
             FROM memory_v2_legacy_map AS mappings
             JOIN memory_facts AS legacy_facts
               ON legacy_facts.fact_id = mappings.legacy_fact_id
             JOIN memory_v2_current_facts AS current_facts
               ON current_facts.fact_id = mappings.fact_id
              AND current_facts.owner_kind = mappings.owner_kind
              AND current_facts.project_id = mappings.project_id
             JOIN memory_v2_assertion_payloads AS payloads
               ON payloads.assertion_id = current_facts.active_assertion_id
              AND payloads.fact_id = current_facts.fact_id
              AND payloads.owner_kind = current_facts.owner_kind
              AND payloads.project_id = current_facts.project_id
             WHERE mappings.owner_kind = ?1
               AND mappings.project_id = ?2
               AND mappings.owner_json = ?3
               AND mappings.source_store_id = ?4
               AND current_facts.payload_access = 'eligible'
               AND (
                    legacy_facts.hrr_vector IS NULL
                    OR legacy_facts.hrr_algebra <> 'amari_fhrr'
                    OR legacy_facts.hrr_dim <> ?5
                    OR legacy_facts.hrr_precision <> ?6
                    OR length(legacy_facts.hrr_vector) <> ?7
               )
             ORDER BY legacy_facts.updated_at DESC, mappings.fact_id ASC
             LIMIT ?8",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
                HolographicEncoder::DIMENSIONS as i64,
                HolographicEncoder::HRR_PRECISION,
                HolographicEncoder::SERIALIZED_F32_BYTES as i64,
                limit,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let mut fact_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        fact_ids.push(
            FactId::new(row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?)
                .map_err(FactStoreError::from)?,
        );
    }
    drop(rows);
    let now = compatibility_now()?;
    let mut repaired = 0_u64;
    for fact_id in fact_ids {
        let Some(fact) = load_current_fact_tx(transaction, &key, owner, &fact_id).await? else {
            continue;
        };
        let Some(payload) = fact.payload() else {
            continue;
        };
        let mapping = compatibility_required_mapping_tx(transaction, owner, &fact_id).await?;
        let vector = compatibility_mirror_vector(payload)?;
        let changed = transaction
            .execute(
                "UPDATE memory_facts
                 SET hrr_vector = ?1,
                     hrr_algebra = 'amari_fhrr',
                     hrr_dim = ?2,
                     hrr_precision = ?3
                 WHERE fact_id = ?4",
                params![
                    vector,
                    HolographicEncoder::DIMENSIONS as i64,
                    HolographicEncoder::HRR_PRECISION,
                    mapping.legacy_fact_id(),
                ],
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
        if changed != 1 {
            return Err(storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility vector target is missing from the legacy mirror",
            ));
        }
        compatibility_mark_owner_banks_dirty_tx(db, transaction, owner, payload.category(), now)
            .await?;
        repaired = repaired.saturating_add(1);
    }
    Ok(repaired)
}

fn compatibility_average_vectors(vectors: &[Vec<f64>]) -> Vec<f64> {
    let mut average = vec![0.0; HolographicEncoder::DIMENSIONS];
    let mut count = 0_u64;
    for vector in vectors {
        if vector.len() != HolographicEncoder::DIMENSIONS {
            continue;
        }
        count = count.saturating_add(1);
        for (target, value) in average.iter_mut().zip(vector) {
            *target += value;
        }
    }
    if count != 0 {
        for value in &mut average {
            *value /= count as f64;
        }
    }
    average
}

/// Marks every populated bank dirty when the owner has eligible facts but no
/// materialized bank projections at all — the state a store lands in when its
/// legacy cutover predates dirty-marking (or a bank table was lost). Repair
/// then rebuilds them in the same pass; stores with any banks are untouched.
async fn compatibility_mark_absent_banks_dirty_tx(
    db: &Database,
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    now: UtcMicros,
) -> FactStoreResult<()> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut rows = transaction
        .query(
            "SELECT COUNT(*) FROM memory_v2_compatibility_banks
             WHERE owner_kind = ?1 AND project_id = ?2
               AND owner_json = ?3 AND source_store_id = ?4",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let bank_count = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
        .map(|row| row_i64(&row, 0, COMPATIBILITY_WRITE_OPERATION))
        .transpose()?
        .unwrap_or(0);
    drop(rows);
    if bank_count > 0 {
        return Ok(());
    }
    let mut rows = transaction
        .query(
            "SELECT DISTINCT json_extract(payloads.payload_json, '$.category')
             FROM memory_v2_current_facts AS current_facts
             JOIN memory_v2_assertion_payloads AS payloads
               ON payloads.assertion_id = current_facts.active_assertion_id
              AND payloads.fact_id = current_facts.fact_id
              AND payloads.owner_kind = current_facts.owner_kind
              AND payloads.project_id = current_facts.project_id
             WHERE current_facts.owner_kind = ?1
               AND current_facts.project_id = ?2
               AND current_facts.payload_access = 'eligible'
               AND json_extract(payloads.payload_json, '$.category') IS NOT NULL",
            params![key.kind, key.project_id.as_str()],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let mut bank_names = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        bank_names.push(row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?);
    }
    drop(rows);
    if bank_names.is_empty() {
        return Ok(());
    }
    bank_names.push("all".to_owned());
    for bank_name in bank_names {
        db.mark_memory_v2_compatibility_bank_dirty_in_transaction(
            transaction,
            owner,
            &source_store_id,
            &bank_name,
            now,
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    }
    Ok(())
}

pub(super) async fn compatibility_rebuild_dirty_banks_tx(
    db: &Database,
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
) -> FactStoreResult<u64> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut rows = transaction
        .query(
            "SELECT bank_name, updated_at
             FROM memory_v2_compatibility_bank_dirty
             WHERE owner_kind = ?1 AND project_id = ?2
               AND owner_json = ?3 AND source_store_id = ?4
             ORDER BY bank_name ASC
             LIMIT ?5",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
                COMPATIBILITY_REPAIR_BANK_BATCH,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let mut dirty = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        dirty.push((
            row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?,
            UtcMicros(row_i64(&row, 1, COMPATIBILITY_WRITE_OPERATION)?),
        ));
    }
    drop(rows);
    let now = compatibility_now()?;
    let mut rebuilt = 0_u64;
    for (bank_name, dirty_updated_at) in dirty {
        if bank_name != "all"
            && !matches!(
                bank_name.as_str(),
                "general" | "user_pref" | "project" | "tool" | "decision" | "code_area"
            )
        {
            return Err(storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility dirty bank has an unsupported category",
            ));
        }
        let mut vectors = transaction
            .query(
                "SELECT legacy_facts.fact_id, mappings.fact_id, legacy_facts.hrr_vector
                 FROM memory_v2_legacy_map AS mappings
                 JOIN memory_facts AS legacy_facts
                   ON legacy_facts.fact_id = mappings.legacy_fact_id
                 JOIN memory_v2_current_facts AS current_facts
                   ON current_facts.fact_id = mappings.fact_id
                  AND current_facts.owner_kind = mappings.owner_kind
                  AND current_facts.project_id = mappings.project_id
                 JOIN memory_v2_assertion_payloads AS payloads
                   ON payloads.assertion_id = current_facts.active_assertion_id
                  AND payloads.fact_id = current_facts.fact_id
                  AND payloads.owner_kind = current_facts.owner_kind
                  AND payloads.project_id = current_facts.project_id
                 WHERE mappings.owner_kind = ?1 AND mappings.project_id = ?2
                   AND mappings.owner_json = ?3 AND mappings.source_store_id = ?4
                   AND current_facts.payload_access = 'eligible'
                   AND legacy_facts.hrr_vector IS NOT NULL
                   AND legacy_facts.hrr_algebra = 'amari_fhrr'
                   AND legacy_facts.hrr_dim = ?6
                   AND legacy_facts.hrr_precision = ?7
                   AND length(legacy_facts.hrr_vector) = ?8
                   AND (?5 = 'all' OR legacy_facts.category = ?5)
                 ORDER BY legacy_facts.fact_id ASC",
                params![
                    key.kind,
                    key.project_id.as_str(),
                    key.json.as_str(),
                    source_store_id.as_str(),
                    bank_name.as_str(),
                    HolographicEncoder::DIMENSIONS as i64,
                    HolographicEncoder::HRR_PRECISION,
                    HolographicEncoder::SERIALIZED_F32_BYTES as i64,
                ],
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
        let mut decoded = Vec::new();
        let mut malformed_legacy_fact_ids = Vec::new();
        while let Some(row) = vectors
            .next()
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
        {
            let legacy_fact_id = row_i64(&row, 0, COMPATIBILITY_WRITE_OPERATION)?;
            let fact_id = FactId::new(row_string(&row, 1, COMPATIBILITY_WRITE_OPERATION)?)
                .map_err(FactStoreError::from)?;
            let vector = match row.get::<crate::db::engine::Value>(2) {
                Ok(crate::db::engine::Value::Blob(bytes)) => {
                    HolographicEncoder::deserialize(&bytes)
                        .ok()
                        .filter(|vector| {
                            vector.len() == HolographicEncoder::DIMENSIONS
                                && vector.iter().all(|value| value.is_finite())
                        })
                }
                Ok(_) | Err(_) => None,
            };
            match vector {
                Some(vector) => decoded.push(vector),
                None => malformed_legacy_fact_ids.push((legacy_fact_id, fact_id)),
            }
        }
        drop(vectors);
        for (legacy_fact_id, fact_id) in malformed_legacy_fact_ids {
            let replacement = match load_current_fact_tx(transaction, &key, owner, &fact_id).await?
            {
                Some(fact) => fact.payload().and_then(|payload| {
                    compatibility_mirror_vector(payload).ok().and_then(|bytes| {
                        HolographicEncoder::deserialize(&bytes)
                            .ok()
                            .filter(|vector| {
                                vector.len() == HolographicEncoder::DIMENSIONS
                                    && vector.iter().all(|value| value.is_finite())
                            })
                            .map(|vector| (bytes, vector))
                    })
                }),
                None => None,
            };
            match replacement {
                Some((vector, decoded_vector)) => {
                    transaction
                        .execute(
                            "UPDATE memory_facts
                             SET hrr_vector = ?1,
                                 hrr_algebra = 'amari_fhrr',
                                 hrr_dim = ?2,
                                 hrr_precision = ?3
                             WHERE fact_id = ?4",
                            params![
                                vector,
                                HolographicEncoder::DIMENSIONS as i64,
                                HolographicEncoder::HRR_PRECISION,
                                legacy_fact_id,
                            ],
                        )
                        .await
                        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
                    decoded.push(decoded_vector);
                }
                None => {
                    transaction
                        .execute(
                            "UPDATE memory_facts
                             SET hrr_vector = NULL,
                                 hrr_algebra = 'amari_fhrr',
                                 hrr_dim = ?1,
                                 hrr_precision = ?2
                             WHERE fact_id = ?3",
                            params![
                                HolographicEncoder::DIMENSIONS as i64,
                                HolographicEncoder::HRR_PRECISION,
                                legacy_fact_id,
                            ],
                        )
                        .await
                        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
                }
            }
        }
        if decoded.is_empty() {
            db.delete_memory_v2_compatibility_bank_in_transaction(
                transaction,
                owner,
                &source_store_id,
                bank_name.as_str(),
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
        } else {
            let vector = HolographicEncoder::serialize(&compatibility_average_vectors(&decoded))
                .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
            db.upsert_memory_v2_compatibility_bank_in_transaction(
                transaction,
                owner,
                &source_store_id,
                bank_name.as_str(),
                &vector,
                decoded.len() as u64,
                now,
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
        }
        if db
            .clear_memory_v2_compatibility_bank_dirty_in_transaction(
                transaction,
                owner,
                &source_store_id,
                bank_name.as_str(),
                dirty_updated_at,
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
        {
            rebuilt = rebuilt.saturating_add(1);
        }
    }
    Ok(rebuilt)
}

pub(super) async fn compatibility_feedback_history_repair_progress_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
) -> FactCompatibilityResult<CompatibilityFeedbackRepairProgressV1> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut rows = transaction
        .query(
            "SELECT owner_json, feedback_frontier, feedback_cursor, phase
             FROM memory_v2_feedback_history_repair_progress
             WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3",
            params![key.kind, key.project_id.as_str(), source_store_id.as_str()],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    else {
        return Ok(CompatibilityFeedbackRepairProgressV1::NotRequired);
    };
    if row_string(&row, 0, COMPATIBILITY_READ_OPERATION)? != key.json {
        return Err(FactStoreError::OwnerMismatch.into());
    }
    let frontier = nonnegative_u64(
        row_i64(&row, 1, COMPATIBILITY_READ_OPERATION)?,
        "feedback repair frontier",
    )?;
    let cursor = nonnegative_u64(
        row_i64(&row, 2, COMPATIBILITY_READ_OPERATION)?,
        "feedback repair cursor",
    )?;
    if cursor > frontier {
        return Err(storage_message(
            COMPATIBILITY_READ_OPERATION,
            "feedback repair cursor exceeds captured frontier",
        )
        .into());
    }
    match row_string(&row, 3, COMPATIBILITY_READ_OPERATION)?.as_str() {
        "pending" => Ok(CompatibilityFeedbackRepairProgressV1::Incomplete {
            processed: 0,
            remaining: Some(frontier.saturating_sub(cursor)),
        }),
        "complete" => Ok(CompatibilityFeedbackRepairProgressV1::Complete { processed: 0 }),
        _ => Err(storage_message(
            COMPATIBILITY_READ_OPERATION,
            "feedback repair progress has an unsupported phase",
        )
        .into()),
    }
}
