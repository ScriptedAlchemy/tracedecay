//! Compatibility missing-vector repair.
//!
//! Plan 39 Task 7 (owner decision 2026-08-07, second): the derived holographic
//! bank projection is deleted, so repair no longer marks, rebuilds, or clears
//! bank rows. Recall re-encodes candidate vectors from canonical fact content
//! at query time.

use crate::memory::encoding::HolographicEncoder;

use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use serde_json::json;

use tracedecay_domain::{ActorId, FactId, FactOwnerV1, UtcMicros};
use tracedecay_store::{
    FactStoreError, FactStoreResult, ProjectMemoryFactRepairVectorV1,
    ProjectMemoryMemoryRepairCommandV1, ProjectMemoryMemoryRepairStatsV1, ProjectMemoryResult,
};

use super::crud::{compatibility_mirror_vector, load_current_fact_tx};
use super::curation::{
    project_memory_available_curation_fact_tx, project_memory_curation_evidence_ids_tx,
};
use super::envelope::{
    project_memory_digest, project_memory_lookup_operation_receipt_tx, project_memory_receipt_u64,
    project_memory_record_operation_receipt_tx,
};
use super::primitives::{
    OwnerKey, PROJECT_MEMORY_WRITE_OPERATION, compatibility_legacy_timestamp,
    compatibility_source_store_id, project_memory_now, row_string, storage_error, storage_message,
};
use super::projection::project_memory_required_mapping_tx;

/// Per-repair-pass batch caps. The daemon scheduler treats a pass that hits
/// either cap as incomplete and keeps ticking rather than going idle with a
/// converging backlog.
pub(crate) const COMPATIBILITY_REPAIR_VECTOR_BATCH: i64 = 512;

/// True when a repair pass filled the per-pass batch cap, so backlog may
/// remain behind the cap. Only the store computes this — it owns the cap — so
/// the daemon scheduler can consume [`ProjectMemoryMemoryRepairStatsV1::saturated`]
/// without depending on this store-internal constant.
fn compatibility_repair_batches_saturated(missing_vectors_repaired: u64) -> bool {
    missing_vectors_repaired >= COMPATIBILITY_REPAIR_VECTOR_BATCH as u64
}

pub(super) async fn compatibility_repair_vector_for_fact_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    operation: &ProjectMemoryFactRepairVectorV1,
    now: UtcMicros,
) -> FactStoreResult<FactId> {
    let _evidence =
        project_memory_curation_evidence_ids_tx(transaction, owner, operation.evidence_facts())
            .await?;
    let (fact_id, fact, mapping) =
        project_memory_available_curation_fact_tx(transaction, operation.fact()).await?;
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
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    if changed != 1 {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "compatibility vector target is missing from the legacy mirror",
        ));
    }
    Ok(fact_id)
}

pub(super) fn project_memory_repair_request_digest(
    request: &ProjectMemoryMemoryRepairCommandV1,
) -> FactStoreResult<String> {
    project_memory_digest(json!({
        "owner": request.owner(),
        "actor": request.actor().map(ActorId::as_str),
    }))
}

pub(super) async fn repair_project_memory_tx(
    transaction: &Transaction<'_>,
    request: &ProjectMemoryMemoryRepairCommandV1,
) -> ProjectMemoryResult<ProjectMemoryMemoryRepairStatsV1> {
    let request_digest = project_memory_repair_request_digest(request)?;
    if let Some(receipt) = project_memory_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "repair",
        &request_digest,
    )
    .await?
    {
        let missing_vectors_repaired =
            project_memory_receipt_u64(&receipt.receipt, "missing_vectors_repaired")?;
        return Ok(
            ProjectMemoryMemoryRepairStatsV1::new(missing_vectors_repaired, 0).with_saturated(
                compatibility_repair_batches_saturated(missing_vectors_repaired),
            ),
        );
    }
    let now = project_memory_now()?;
    let missing_vectors_repaired = compatibility_repair_missing_vectors_tx(
        transaction,
        request.owner(),
        COMPATIBILITY_REPAIR_VECTOR_BATCH,
    )
    .await?;
    let receipt = json!({
        "missing_vectors_repaired": missing_vectors_repaired,
    });
    project_memory_record_operation_receipt_tx(
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
        ProjectMemoryMemoryRepairStatsV1::new(missing_vectors_repaired, 0).with_saturated(
            compatibility_repair_batches_saturated(missing_vectors_repaired),
        ),
    )
}

pub(super) async fn compatibility_repair_missing_vectors_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    limit: i64,
) -> FactStoreResult<u64> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut rows = transaction
        .query(
            "SELECT mappings.fact_id
             FROM memory_facts AS legacy_facts
             JOIN memory_v2_facts AS mappings
               ON mappings.fact_id = legacy_facts.canonical_fact_id
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
               AND ?4 = 'persisted-numeric-fact-id'
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
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let mut fact_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
    {
        fact_ids.push(
            FactId::new(row_string(&row, 0, PROJECT_MEMORY_WRITE_OPERATION)?)
                .map_err(FactStoreError::from)?,
        );
    }
    drop(rows);
    let mut repaired = 0_u64;
    for fact_id in fact_ids {
        let Some(fact) = load_current_fact_tx(transaction, &key, owner, &fact_id).await? else {
            continue;
        };
        let Some(payload) = fact.payload() else {
            continue;
        };
        let mapping = project_memory_required_mapping_tx(transaction, owner, &fact_id).await?;
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
            .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
        if changed != 1 {
            return Err(storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "compatibility vector target is missing from the legacy mirror",
            ));
        }
        repaired = repaired.saturating_add(1);
    }
    Ok(repaired)
}
