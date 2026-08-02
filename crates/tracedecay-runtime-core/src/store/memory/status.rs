//! The compatibility memory-status probe.

use crate::memory::encoding::HolographicEncoder;

use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;

use tracedecay_domain::FactOwnerV1;
use tracedecay_store::{
    CompatibilityFeedbackRepairProgressV1, CompatibilityMemoryAlgebraV1,
    CompatibilityMemoryFeedbackFunnelV1, CompatibilityMemoryRepairStatsV1,
    CompatibilityMemoryStatusV1, CompatibilityProjectionStateV1, FactCompatibilityResult,
    FactStoreResult,
};

use super::primitives::{
    COMPATIBILITY_READ_OPERATION, COMPATIBILITY_WRITE_OPERATION, OwnerKey,
    compatibility_source_store_id, nonnegative_u64, row_i64, storage_error, storage_message,
};

async fn compatibility_owner_status_counts_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
) -> FactStoreResult<(u64, u64, u64, [u64; 4], u64, u64, u64, u64, u64, u64)> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT
                COUNT(*),
                COALESCE(SUM(CASE WHEN current_facts.trust_score < 0.25 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN current_facts.trust_score >= 0.25 AND current_facts.trust_score < 0.50 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN current_facts.trust_score >= 0.50 AND current_facts.trust_score < 0.75 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN current_facts.trust_score >= 0.75 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN current_facts.trust_score < ?4 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(current_facts.helpful_count), 0),
                COALESCE(SUM(current_facts.unhelpful_count), 0),
                COALESCE(SUM(current_facts.retrieval_count), 0),
                COALESCE(SUM(current_facts.access_count), 0),
                COALESCE(SUM(CASE WHEN current_facts.retrieval_count > 0 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN current_facts.helpful_count + current_facts.unhelpful_count > 0 THEN 1 ELSE 0 END), 0)
             FROM memory_v2_current_facts AS current_facts
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = current_facts.fact_id
              AND facts.owner_kind = current_facts.owner_kind
              AND facts.project_id = current_facts.project_id
             WHERE current_facts.owner_kind = ?1
               AND current_facts.project_id = ?2
               AND facts.owner_json = ?3
               AND current_facts.active_assertion_id IS NOT NULL",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                crate::memory::trust::DEFAULT_MIN_TRUST
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility status is missing",
            )
        })?;
    let fact_count = nonnegative_u64(
        row_i64(&row, 0, COMPATIBILITY_WRITE_OPERATION)?,
        "fact count",
    )?;
    let trust = [
        nonnegative_u64(
            row_i64(&row, 1, COMPATIBILITY_WRITE_OPERATION)?,
            "trust count",
        )?,
        nonnegative_u64(
            row_i64(&row, 2, COMPATIBILITY_WRITE_OPERATION)?,
            "trust count",
        )?,
        nonnegative_u64(
            row_i64(&row, 3, COMPATIBILITY_WRITE_OPERATION)?,
            "trust count",
        )?,
        nonnegative_u64(
            row_i64(&row, 4, COMPATIBILITY_WRITE_OPERATION)?,
            "trust count",
        )?,
    ];
    let below_default = nonnegative_u64(
        row_i64(&row, 5, COMPATIBILITY_WRITE_OPERATION)?,
        "trust count",
    )?;
    let helpful = nonnegative_u64(
        row_i64(&row, 6, COMPATIBILITY_WRITE_OPERATION)?,
        "helpful count",
    )?;
    let unhelpful = nonnegative_u64(
        row_i64(&row, 7, COMPATIBILITY_WRITE_OPERATION)?,
        "unhelpful count",
    )?;
    let retrieval_total = nonnegative_u64(
        row_i64(&row, 8, COMPATIBILITY_WRITE_OPERATION)?,
        "retrieval total",
    )?;
    let access_total = nonnegative_u64(
        row_i64(&row, 9, COMPATIBILITY_WRITE_OPERATION)?,
        "access total",
    )?;
    let retrieved_fact_count = nonnegative_u64(
        row_i64(&row, 10, COMPATIBILITY_WRITE_OPERATION)?,
        "retrieved fact count",
    )?;
    let rated_fact_count = nonnegative_u64(
        row_i64(&row, 11, COMPATIBILITY_WRITE_OPERATION)?,
        "rated fact count",
    )?;
    Ok((
        fact_count,
        helpful,
        unhelpful,
        trust,
        below_default,
        retrieval_total,
        access_total,
        retrieved_fact_count,
        rated_fact_count,
        helpful.saturating_add(unhelpful),
    ))
}

async fn compatibility_owner_has_dirty_banks_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
) -> FactStoreResult<bool> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut rows = transaction
        .query(
            "SELECT 1
             FROM memory_v2_compatibility_bank_dirty AS dirty
             WHERE dirty.owner_kind = ?1 AND dirty.project_id = ?2
               AND dirty.owner_json = ?3 AND dirty.source_store_id = ?4
             LIMIT 1",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
        .is_some())
}

pub(super) async fn compatibility_memory_status_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    feedback_repair: CompatibilityFeedbackRepairProgressV1,
) -> FactCompatibilityResult<CompatibilityMemoryStatusV1> {
    let (
        fact_count,
        helpful_count,
        unhelpful_count,
        trust,
        below_default_recall_threshold_count,
        retrieval_count_total,
        access_count_total,
        retrieved_fact_count,
        rated_fact_count,
        feedback_total,
    ) = compatibility_owner_status_counts_tx(transaction, owner).await?;
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut entity_rows = transaction
        .query(
            "SELECT COUNT(DISTINCT relations.entity_id)
             FROM memory_v2_legacy_map AS mappings
             JOIN memory_fact_entities AS relations ON relations.fact_id = mappings.legacy_fact_id
             WHERE mappings.owner_kind = ?1 AND mappings.project_id = ?2
               AND mappings.owner_json = ?3 AND mappings.source_store_id = ?4",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str()
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let entity_row = entity_rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility entity count is missing",
            )
        })?;
    let entity_count = nonnegative_u64(
        row_i64(&entity_row, 0, COMPATIBILITY_READ_OPERATION)?,
        "entity count",
    )?;
    let mut missing_rows = transaction
        .query(
            "SELECT COUNT(*) FROM memory_v2_legacy_map AS mappings
             JOIN memory_facts AS legacy_facts ON legacy_facts.fact_id = mappings.legacy_fact_id
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
               AND (legacy_facts.hrr_vector IS NULL
                    OR legacy_facts.hrr_algebra <> 'amari_fhrr'
                    OR legacy_facts.hrr_dim <> ?5
                    OR legacy_facts.hrr_precision <> ?6
                    OR length(legacy_facts.hrr_vector) <> ?7)",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
                HolographicEncoder::DIMENSIONS as i64,
                HolographicEncoder::HRR_PRECISION,
                HolographicEncoder::SERIALIZED_F32_BYTES as i64,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let missing_row = missing_rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility missing vector count is missing",
            )
        })?;
    let missing_vector_count = nonnegative_u64(
        row_i64(&missing_row, 0, COMPATIBILITY_READ_OPERATION)?,
        "missing vector count",
    )?;
    let dirty_banks = compatibility_owner_has_dirty_banks_tx(transaction, owner).await?;
    let mut bank_rows = transaction
        .query(
            "SELECT COUNT(*) FROM memory_v2_compatibility_banks AS banks
             WHERE banks.owner_kind = ?1 AND banks.project_id = ?2
               AND banks.owner_json = ?3 AND banks.source_store_id = ?4",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str()
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let bank_row = bank_rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility bank count is missing",
            )
        })?;
    let bank_count = nonnegative_u64(
        row_i64(&bank_row, 0, COMPATIBILITY_READ_OPERATION)?,
        "bank count",
    )?;
    let projection_state = if missing_vector_count == 0 && !dirty_banks {
        CompatibilityProjectionStateV1::Ready
    } else {
        CompatibilityProjectionStateV1::Rebuilding
    };
    CompatibilityMemoryStatusV1::new(
        owner.clone(),
        fact_count,
        entity_count,
        bank_count,
        CompatibilityMemoryAlgebraV1::new(
            "amari_fhrr".to_owned(),
            HolographicEncoder::DIMENSIONS as u64,
            fact_count.saturating_mul(HolographicEncoder::DIMENSIONS as u64),
        )?,
        trust[0],
        trust[1],
        trust[2],
        trust[3],
        below_default_recall_threshold_count,
        helpful_count,
        unhelpful_count,
        missing_vector_count,
        projection_state,
        CompatibilityMemoryRepairStatsV1::new(0, 0),
        CompatibilityMemoryFeedbackFunnelV1::new(
            retrieval_count_total,
            access_count_total,
            retrieved_fact_count,
            rated_fact_count,
            feedback_total,
        ),
    )
    .map(|status| status.with_feedback_history_repair(feedback_repair))
    .map_err(Into::into)
}
