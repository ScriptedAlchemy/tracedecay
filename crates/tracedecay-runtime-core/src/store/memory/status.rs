//! The compatibility memory-status probe.

use crate::memory::encoding::HolographicEncoder;

use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;

use tracedecay_domain::FactOwnerV1;
use tracedecay_store::{
    FactStoreResult, ProjectMemoryFeedbackRepairProgressV1, ProjectMemoryMemoryAlgebraV1,
    ProjectMemoryMemoryFeedbackFunnelV1, ProjectMemoryMemoryRepairStatsV1,
    ProjectMemoryMemoryStatusV1, ProjectMemoryProjectionStateV1, ProjectMemoryResult,
};

use super::primitives::{
    OwnerKey, PROJECT_MEMORY_READ_OPERATION, PROJECT_MEMORY_WRITE_OPERATION,
    compatibility_source_store_id, nonnegative_u64, row_i64, storage_error, storage_message,
};

async fn project_memory_owner_status_counts_tx(
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
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "compatibility status is missing",
            )
        })?;
    let fact_count = nonnegative_u64(
        row_i64(&row, 0, PROJECT_MEMORY_WRITE_OPERATION)?,
        "fact count",
    )?;
    let trust = [
        nonnegative_u64(
            row_i64(&row, 1, PROJECT_MEMORY_WRITE_OPERATION)?,
            "trust count",
        )?,
        nonnegative_u64(
            row_i64(&row, 2, PROJECT_MEMORY_WRITE_OPERATION)?,
            "trust count",
        )?,
        nonnegative_u64(
            row_i64(&row, 3, PROJECT_MEMORY_WRITE_OPERATION)?,
            "trust count",
        )?,
        nonnegative_u64(
            row_i64(&row, 4, PROJECT_MEMORY_WRITE_OPERATION)?,
            "trust count",
        )?,
    ];
    let below_default = nonnegative_u64(
        row_i64(&row, 5, PROJECT_MEMORY_WRITE_OPERATION)?,
        "trust count",
    )?;
    let helpful = nonnegative_u64(
        row_i64(&row, 6, PROJECT_MEMORY_WRITE_OPERATION)?,
        "helpful count",
    )?;
    let unhelpful = nonnegative_u64(
        row_i64(&row, 7, PROJECT_MEMORY_WRITE_OPERATION)?,
        "unhelpful count",
    )?;
    let retrieval_total = nonnegative_u64(
        row_i64(&row, 8, PROJECT_MEMORY_WRITE_OPERATION)?,
        "retrieval total",
    )?;
    let access_total = nonnegative_u64(
        row_i64(&row, 9, PROJECT_MEMORY_WRITE_OPERATION)?,
        "access total",
    )?;
    let retrieved_fact_count = nonnegative_u64(
        row_i64(&row, 10, PROJECT_MEMORY_WRITE_OPERATION)?,
        "retrieved fact count",
    )?;
    let rated_fact_count = nonnegative_u64(
        row_i64(&row, 11, PROJECT_MEMORY_WRITE_OPERATION)?,
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

/// Recomputes the holographic bank count directly from eligible facts.
///
/// Plan 39 Task 7 (owner decision 2026-08-07, second) deleted the persisted
/// `memory_v2_banks` projection: stored bank vectors were never read back, and
/// recall re-encodes from canonical content at query time. The count is the
/// number of distinct bank-eligible categories plus the aggregate `all` bank,
/// using the same eligibility the deleted rebuild pass used — an eligible
/// current fact whose mirrored vector is canonical FHRR material.
async fn compatibility_owner_bank_count_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
) -> FactStoreResult<u64> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut rows = transaction
        .query(
            "SELECT COUNT(DISTINCT legacy_facts.category)
             FROM memory_facts AS legacy_facts
             JOIN memory_v2_facts AS mappings
               ON mappings.fact_id = legacy_facts.canonical_fact_id
             JOIN memory_v2_current_facts AS current_facts
               ON current_facts.fact_id = mappings.fact_id
              AND current_facts.owner_kind = mappings.owner_kind
              AND current_facts.project_id = mappings.project_id
             WHERE mappings.owner_kind = ?1
               AND mappings.project_id = ?2
               AND mappings.owner_json = ?3
               AND ?4 = 'legacy-memory-v1'
               AND current_facts.payload_access = 'eligible'
               AND legacy_facts.hrr_vector IS NOT NULL
               AND legacy_facts.hrr_algebra = 'amari_fhrr'
               AND legacy_facts.hrr_dim = ?5
               AND legacy_facts.hrr_precision = ?6
               AND length(legacy_facts.hrr_vector) = ?7",
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
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let category_count = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
        .map(|row| row_i64(&row, 0, PROJECT_MEMORY_READ_OPERATION))
        .transpose()?
        .unwrap_or(0);
    let category_count = nonnegative_u64(category_count, "bank category count")?;
    if category_count == 0 {
        return Ok(0);
    }
    Ok(category_count.saturating_add(1))
}

pub(super) async fn project_memory_status_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
) -> ProjectMemoryResult<ProjectMemoryMemoryStatusV1> {
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
    ) = project_memory_owner_status_counts_tx(transaction, owner).await?;
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut entity_rows = transaction
        .query(
            "SELECT COUNT(DISTINCT relations.entity_id)
             FROM memory_facts AS projections
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = projections.canonical_fact_id
             JOIN memory_fact_entities AS relations
               ON relations.fact_id = projections.fact_id
             WHERE facts.owner_kind = ?1 AND facts.project_id = ?2
               AND facts.owner_json = ?3 AND ?4 = 'legacy-memory-v1'",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str()
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let entity_row = entity_rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_READ_OPERATION,
                "compatibility entity count is missing",
            )
        })?;
    let entity_count = nonnegative_u64(
        row_i64(&entity_row, 0, PROJECT_MEMORY_READ_OPERATION)?,
        "entity count",
    )?;
    let mut missing_rows = transaction
        .query(
            "SELECT COUNT(*) FROM memory_facts AS legacy_facts
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = legacy_facts.canonical_fact_id
             JOIN memory_v2_current_facts AS current_facts
               ON current_facts.fact_id = facts.fact_id
              AND current_facts.owner_kind = facts.owner_kind
              AND current_facts.project_id = facts.project_id
             JOIN memory_v2_assertion_payloads AS payloads
               ON payloads.assertion_id = current_facts.active_assertion_id
              AND payloads.fact_id = current_facts.fact_id
              AND payloads.owner_kind = current_facts.owner_kind
              AND payloads.project_id = current_facts.project_id
             WHERE facts.owner_kind = ?1 AND facts.project_id = ?2
               AND facts.owner_json = ?3 AND ?4 = 'legacy-memory-v1'
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
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let missing_row = missing_rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_READ_OPERATION,
                "compatibility missing vector count is missing",
            )
        })?;
    let missing_vector_count = nonnegative_u64(
        row_i64(&missing_row, 0, PROJECT_MEMORY_READ_OPERATION)?,
        "missing vector count",
    )?;
    let bank_count = compatibility_owner_bank_count_tx(transaction, owner).await?;
    let projection_state = if missing_vector_count == 0 {
        ProjectMemoryProjectionStateV1::Ready
    } else {
        ProjectMemoryProjectionStateV1::Rebuilding
    };
    ProjectMemoryMemoryStatusV1::new(
        owner.clone(),
        fact_count,
        entity_count,
        bank_count,
        ProjectMemoryMemoryAlgebraV1::new(
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
        ProjectMemoryMemoryRepairStatsV1::new(0, 0),
        ProjectMemoryMemoryFeedbackFunnelV1::new(
            retrieval_count_total,
            access_count_total,
            retrieved_fact_count,
            rated_fact_count,
            feedback_total,
        ),
    )
    .map(|status| {
        status.with_feedback_history_repair(ProjectMemoryFeedbackRepairProgressV1::NotRequired)
    })
    .map_err(Into::into)
}
