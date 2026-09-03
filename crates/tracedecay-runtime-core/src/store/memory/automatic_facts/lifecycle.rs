//! Terminal automatic fact-receipt persistence and replay.

use super::super::envelope::{
    ProjectMemoryOperationReceiptV1, project_memory_lookup_operation_receipt_tx,
    project_memory_record_operation_receipt_tx,
};
use super::super::primitives::{
    OwnerKey, PROJECT_MEMORY_WRITE_OPERATION, project_memory_now, row_string, storage_error,
    storage_message, to_json,
};
use super::{
    project_memory_automatic_fact_receipt_record_tx, project_memory_automatic_fact_request_value,
    project_memory_automatic_fact_state_label,
};
use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use serde_json::{Value, json};
use tracedecay_domain::{
    FactAssertionId, FactEventId, FactId, FactOwnerV1, ProvenanceId, UtcMicros,
};
use tracedecay_store::{
    FactStoreError, FactStoreResult, ProjectMemoryAutomaticFactEffectV1,
    ProjectMemoryAutomaticFactEvidenceV1, ProjectMemoryAutomaticFactReceiptV1,
    ProjectMemoryFactAddCommandV1,
};

async fn automatic_fact_receipt_digest_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    apply_id: &ProvenanceId,
) -> FactStoreResult<Option<String>> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT owner_json, request_digest
             FROM memory_v2_automatic_fact_receipts
             WHERE apply_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
            params![apply_id.as_str(), key.kind, key.project_id.as_str()],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
    else {
        return Ok(None);
    };
    if row_string(&row, 0, PROJECT_MEMORY_WRITE_OPERATION)? != key.json {
        return Err(FactStoreError::OwnerMismatch);
    }
    Ok(Some(row_string(&row, 1, PROJECT_MEMORY_WRITE_OPERATION)?))
}

async fn automatic_fact_receipt_for_digest_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    request_digest: &str,
) -> FactStoreResult<Option<ProvenanceId>> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT apply_id, owner_json
             FROM memory_v2_automatic_fact_receipts
             WHERE owner_kind = ?1 AND project_id = ?2 AND request_digest = ?3",
            params![key.kind, key.project_id.as_str(), request_digest],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
    else {
        return Ok(None);
    };
    if row_string(&row, 1, PROJECT_MEMORY_WRITE_OPERATION)? != key.json {
        return Err(FactStoreError::OwnerMismatch);
    }
    ProvenanceId::new(row_string(&row, 0, PROJECT_MEMORY_WRITE_OPERATION)?)
        .map(Some)
        .map_err(FactStoreError::from)
}

fn automatic_fact_receipt_apply_id(
    receipt: &ProjectMemoryOperationReceiptV1,
) -> FactStoreResult<ProvenanceId> {
    let apply_id = receipt
        .receipt
        .get("apply_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "automatic fact operation receipt is missing its apply identity",
            )
        })?;
    ProvenanceId::new(apply_id.to_owned()).map_err(FactStoreError::from)
}

pub(in crate::store::memory) async fn project_memory_replay_automatic_fact_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    receipt: &ProjectMemoryOperationReceiptV1,
) -> FactStoreResult<ProjectMemoryAutomaticFactReceiptV1> {
    let apply_id = automatic_fact_receipt_apply_id(receipt)?;
    project_memory_automatic_fact_receipt_record_tx(transaction, owner, &apply_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "automatic fact replay target is missing",
            )
        })
}

#[allow(clippy::too_many_arguments)]
pub(in crate::store::memory) async fn project_memory_record_automatic_fact_receipt_tx(
    transaction: &Transaction<'_>,
    apply_id: &ProvenanceId,
    request: &ProjectMemoryFactAddCommandV1,
    request_digest: &str,
    evidence: &ProjectMemoryAutomaticFactEvidenceV1,
    effect: &ProjectMemoryAutomaticFactEffectV1,
    occurred_at: UtcMicros,
) -> FactStoreResult<()> {
    let key = OwnerKey::new(request.owner())?;
    let state = effect.state();
    let state_label = project_memory_automatic_fact_state_label(state);
    let (quarantine_reason, fact_id, assertion_id, event_id) = match effect {
        ProjectMemoryAutomaticFactEffectV1::Applied {
            fact_id,
            assertion_id,
            event_id,
            ..
        } => (None, Some(fact_id), Some(assertion_id), Some(event_id)),
        ProjectMemoryAutomaticFactEffectV1::Quarantined { reason } => {
            (Some(reason.as_str()), None, None, None)
        }
    };
    transaction
        .execute(
            "INSERT INTO memory_v2_automatic_fact_receipts(
                apply_id, owner_kind, project_id, owner_json, idempotency_key,
                request_digest, request_json, evidence_json, state, quarantine_reason,
                applied_fact_id, applied_assertion_id, applied_event_id, recorded_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                apply_id.as_str(),
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                request.operation_id().as_str(),
                request_digest,
                to_json(
                    &project_memory_automatic_fact_request_value(request),
                    "serialize automatic fact request",
                )?,
                to_json(evidence, "serialize automatic fact evidence")?,
                state_label,
                quarantine_reason,
                fact_id.map(FactId::as_str),
                assertion_id.map(FactAssertionId::as_str),
                event_id.map(FactEventId::as_str),
                occurred_at.0,
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    Ok(())
}

pub(in crate::store::memory) async fn project_memory_existing_automatic_fact_receipt_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    apply_id: &ProvenanceId,
    request_digest: &str,
) -> FactStoreResult<Option<ProjectMemoryAutomaticFactReceiptV1>> {
    if let Some(existing_digest) =
        automatic_fact_receipt_digest_tx(transaction, owner, apply_id).await?
    {
        if existing_digest != request_digest {
            return Err(storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "automatic fact apply identity was reused with a different request",
            ));
        }
        return project_memory_automatic_fact_receipt_record_tx(transaction, owner, apply_id).await;
    }
    let Some(existing_id) =
        automatic_fact_receipt_for_digest_tx(transaction, owner, request_digest).await?
    else {
        return Ok(None);
    };
    project_memory_automatic_fact_receipt_record_tx(transaction, owner, &existing_id).await
}

pub(in crate::store::memory) async fn project_memory_lookup_automatic_fact_operation_tx(
    transaction: &Transaction<'_>,
    request: &ProjectMemoryFactAddCommandV1,
    request_digest: &str,
) -> FactStoreResult<Option<ProjectMemoryAutomaticFactReceiptV1>> {
    let Some(receipt) = project_memory_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "automatic_fact_apply",
        request_digest,
    )
    .await?
    else {
        return Ok(None);
    };
    project_memory_replay_automatic_fact_tx(transaction, request.owner(), &receipt)
        .await
        .map(Some)
}

pub(in crate::store::memory) async fn project_memory_record_automatic_fact_operation_tx(
    transaction: &Transaction<'_>,
    receipt: &ProjectMemoryAutomaticFactReceiptV1,
    request_digest: &str,
) -> FactStoreResult<()> {
    let receipt_value = json!({
        "apply_id": receipt.apply_id().as_str(),
        "state": project_memory_automatic_fact_state_label(receipt.state()),
    });
    project_memory_record_operation_receipt_tx(
        transaction,
        receipt.owner(),
        receipt.request().operation_id(),
        "automatic_fact_apply",
        request_digest,
        receipt.applied_fact_id(),
        receipt.applied_event_id(),
        &receipt_value,
        project_memory_now()?,
    )
    .await
}
