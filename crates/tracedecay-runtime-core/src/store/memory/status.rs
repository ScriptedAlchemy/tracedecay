//! Canonical project-memory status derived from current facts and payloads.

use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use crate::memory::encoding::HolographicEncoder;

use amari_holographic::{BindingAlgebra, FHRRAlgebra};
use tracedecay_domain::FactOwnerV1;
use tracedecay_store::{
    FactReadControl, FactStoreResult, ProjectMemoryMemoryAlgebraV1,
    ProjectMemoryMemoryFeedbackFunnelV1, ProjectMemoryMemoryStatusV1,
};

use super::primitives::{
    OwnerKey, PROJECT_MEMORY_READ_OPERATION, ensure_project_memory_read_active, nonnegative_u64,
    row_i64, storage_error, storage_message,
};

struct StatusCounts {
    fact_count: u64,
    helpful_count: u64,
    unhelpful_count: u64,
    trust: [u64; 4],
    below_default_recall_threshold_count: u64,
    retrieval_count_total: u64,
    access_count_total: u64,
    retrieved_fact_count: u64,
    rated_fact_count: u64,
}

fn estimated_holographic_capacity() -> FactStoreResult<u64> {
    u64::try_from(
        FHRRAlgebra::<{ HolographicEncoder::DIMENSIONS }>::fhrr_identity().theoretical_capacity(),
    )
    .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))
}

async fn project_memory_owner_status_counts_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    read_control: &FactReadControl,
) -> FactStoreResult<StatusCounts> {
    ensure_project_memory_read_active(read_control)?;
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
               AND current_facts.payload_access = 'eligible'
               AND current_facts.active_assertion_id IS NOT NULL",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                crate::memory::trust::DEFAULT_MIN_TRUST,
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    ensure_project_memory_read_active(read_control)?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
        .ok_or_else(|| storage_message(PROJECT_MEMORY_READ_OPERATION, "status row is missing"))?;
    let count = |index: i32, field: &'static str| {
        nonnegative_u64(row_i64(&row, index, PROJECT_MEMORY_READ_OPERATION)?, field)
    };
    Ok(StatusCounts {
        fact_count: count(0, "fact count")?,
        trust: [
            count(1, "trust count")?,
            count(2, "trust count")?,
            count(3, "trust count")?,
            count(4, "trust count")?,
        ],
        below_default_recall_threshold_count: count(5, "trust count")?,
        helpful_count: count(6, "helpful count")?,
        unhelpful_count: count(7, "unhelpful count")?,
        retrieval_count_total: count(8, "retrieval total")?,
        access_count_total: count(9, "access total")?,
        retrieved_fact_count: count(10, "retrieved fact count")?,
        rated_fact_count: count(11, "rated fact count")?,
    })
}

async fn project_memory_owner_entity_count_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    read_control: &FactReadControl,
) -> FactStoreResult<u64> {
    ensure_project_memory_read_active(read_control)?;
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT COUNT(DISTINCT lower(trim(CAST(entities.value AS TEXT))))
             FROM memory_v2_current_facts AS current_facts
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = current_facts.fact_id
              AND facts.owner_kind = current_facts.owner_kind
              AND facts.project_id = current_facts.project_id
             JOIN memory_v2_assertion_payloads AS payloads
               ON payloads.assertion_id = current_facts.active_assertion_id
              AND payloads.fact_id = current_facts.fact_id
              AND payloads.owner_kind = current_facts.owner_kind
              AND payloads.project_id = current_facts.project_id
             JOIN json_each(payloads.payload_json, '$.entities') AS entities
             WHERE current_facts.owner_kind = ?1
               AND current_facts.project_id = ?2
               AND facts.owner_json = ?3
               AND current_facts.payload_access = 'eligible'
               AND current_facts.active_assertion_id IS NOT NULL
               AND trim(CAST(entities.value AS TEXT)) <> ''",
            params![key.kind, key.project_id.as_str(), key.json.as_str()],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    ensure_project_memory_read_active(read_control)?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
        .ok_or_else(|| storage_message(PROJECT_MEMORY_READ_OPERATION, "entity count is missing"))?;
    nonnegative_u64(
        row_i64(&row, 0, PROJECT_MEMORY_READ_OPERATION)?,
        "entity count",
    )
}

pub(super) async fn project_memory_status_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    read_control: &FactReadControl,
) -> FactStoreResult<ProjectMemoryMemoryStatusV1> {
    ensure_project_memory_read_active(read_control)?;
    let counts = project_memory_owner_status_counts_tx(transaction, owner, read_control).await?;
    let entity_count =
        project_memory_owner_entity_count_tx(transaction, owner, read_control).await?;
    ensure_project_memory_read_active(read_control)?;
    let feedback_total = counts.helpful_count.saturating_add(counts.unhelpful_count);

    ProjectMemoryMemoryStatusV1::new(
        owner.clone(),
        counts.fact_count,
        entity_count,
        ProjectMemoryMemoryAlgebraV1::new(
            "amari_fhrr".to_owned(),
            HolographicEncoder::DIMENSIONS as u64,
            estimated_holographic_capacity()?,
        )?,
        counts.trust[0],
        counts.trust[1],
        counts.trust[2],
        counts.trust[3],
        counts.below_default_recall_threshold_count,
        counts.helpful_count,
        counts.unhelpful_count,
        ProjectMemoryMemoryFeedbackFunnelV1::new(
            counts.retrieval_count_total,
            counts.access_count_total,
            counts.retrieved_fact_count,
            counts.rated_fact_count,
            feedback_total,
        ),
    )
}
