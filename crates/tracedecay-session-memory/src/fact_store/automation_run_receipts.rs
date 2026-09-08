//! Exact owner/run recovery reads over canonical memory receipts.

use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use tracedecay_domain::{FactEventId, FactId, FactOwnerV1, ProvenanceId, RunId};
use tracedecay_store::{
    FactReadControl, FactStoreError, FactStoreResult, MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS,
    ProjectMemoryAutomationRunReceiptsV1, ProjectMemoryFactCurationReceiptV1,
};

use super::automatic_facts::{
    project_memory_automatic_fact_receipt_record_tx, project_memory_automatic_fact_state_label,
};
use super::primitives::{
    OwnerKey, PROJECT_MEMORY_READ_OPERATION, ensure_project_memory_read_active, from_json,
    row_optional_string, row_string, storage_error, storage_message,
};

struct AutomaticReceiptEnvelopeRow {
    apply_id: ProvenanceId,
    request_digest: String,
    idempotency_key: String,
    state: String,
    fact_id: Option<String>,
    event_id: Option<String>,
    envelope_operation_id: Option<String>,
    envelope_kind: Option<String>,
    envelope_digest: Option<String>,
    envelope_fact_id: Option<String>,
    envelope_event_id: Option<String>,
    envelope_receipt_json: Option<String>,
}

pub(super) async fn project_memory_automation_run_receipts_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    run_id: &RunId,
    read_control: &FactReadControl,
) -> FactStoreResult<ProjectMemoryAutomationRunReceiptsV1> {
    ensure_project_memory_read_active(read_control)?;
    let key = OwnerKey::new(owner)?;

    let mut curation_rows = transaction
        .query(
            "SELECT operation_id, request_digest, fact_id, event_id, receipt_json
             FROM memory_v2_operation_receipts
             WHERE owner_kind = ?1
               AND project_id = ?2
               AND operation_kind = 'curation'
               AND json_extract(receipt_json, '$.automation_run_id') = ?3
             ORDER BY recorded_at ASC, operation_id ASC
             LIMIT 2",
            params![key.kind, key.project_id.as_str(), run_id.as_str()],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let mut curation_values = Vec::with_capacity(2);
    loop {
        ensure_project_memory_read_active(read_control)?;
        let row = curation_rows
            .next()
            .await
            .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
        let Some(row) = row else {
            break;
        };
        let operation_id = ProvenanceId::new(row_string(&row, 0, PROJECT_MEMORY_READ_OPERATION)?)
            .map_err(FactStoreError::from)?;
        let input_digest = row_string(&row, 1, PROJECT_MEMORY_READ_OPERATION)?;
        let fact_id = row_optional_string(&row, 2, PROJECT_MEMORY_READ_OPERATION)?
            .map(FactId::new)
            .transpose()
            .map_err(FactStoreError::from)?;
        let event_id = row_optional_string(&row, 3, PROJECT_MEMORY_READ_OPERATION)?
            .map(FactEventId::new)
            .transpose()
            .map_err(FactStoreError::from)?;
        let receipt = serde_json::from_value::<ProjectMemoryFactCurationReceiptV1>(from_json(
            &row_string(&row, 4, PROJECT_MEMORY_READ_OPERATION)?,
            PROJECT_MEMORY_READ_OPERATION,
        )?)
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
        if receipt.owner() != owner
            || receipt.automation_run_id() != Some(run_id)
            || receipt.operation_id() != &operation_id
            || receipt.input_digest() != input_digest
            || fact_id.as_ref() != receipt.replay_fact_id()
            || event_id.as_ref() != receipt.replay_event_id()
        {
            return Err(storage_message(
                PROJECT_MEMORY_READ_OPERATION,
                "memory automation curation receipt does not match its immutable envelope",
            ));
        }
        curation_values.push(receipt);
    }
    drop(curation_rows);
    ensure_project_memory_read_active(read_control)?;
    if curation_values.len() > 1 {
        return Err(FactStoreError::BatchLimitExceeded {
            field: "memory automation curation receipts",
            count: curation_values.len(),
            max: 1,
        });
    }
    let curation_receipt = curation_values.pop();

    let fetch_limit =
        i64::try_from(MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS + 1).map_err(|_| {
            FactStoreError::InvalidQueryLimit {
                limit: MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS + 1,
                max: MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS,
            }
        })?;
    let mut automatic_rows = transaction
        .query(
            "SELECT automatic.apply_id, automatic.request_digest,
                    automatic.idempotency_key, automatic.state,
                    automatic.applied_fact_id, automatic.applied_event_id,
                    envelope.operation_id, envelope.operation_kind,
                    envelope.request_digest, envelope.fact_id, envelope.event_id,
                    envelope.receipt_json
             FROM memory_v2_automatic_fact_receipts AS automatic
             LEFT JOIN memory_v2_operation_receipts AS envelope
               ON envelope.owner_kind = automatic.owner_kind
              AND envelope.project_id = automatic.project_id
              AND envelope.operation_id = automatic.idempotency_key
             WHERE automatic.owner_kind = ?1
               AND automatic.project_id = ?2
               AND automatic.owner_json = ?3
               AND json_extract(automatic.request_json, '$.automation_run_id') = ?4
             ORDER BY automatic.recorded_at ASC, automatic.apply_id ASC
             LIMIT ?5",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                run_id.as_str(),
                fetch_limit,
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let mut automatic_ids = Vec::with_capacity(MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS + 1);
    loop {
        ensure_project_memory_read_active(read_control)?;
        let row = automatic_rows
            .next()
            .await
            .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
        let Some(row) = row else {
            break;
        };
        automatic_ids.push(AutomaticReceiptEnvelopeRow {
            apply_id: ProvenanceId::new(row_string(&row, 0, PROJECT_MEMORY_READ_OPERATION)?)
                .map_err(FactStoreError::from)?,
            request_digest: row_string(&row, 1, PROJECT_MEMORY_READ_OPERATION)?,
            idempotency_key: row_string(&row, 2, PROJECT_MEMORY_READ_OPERATION)?,
            state: row_string(&row, 3, PROJECT_MEMORY_READ_OPERATION)?,
            fact_id: row_optional_string(&row, 4, PROJECT_MEMORY_READ_OPERATION)?,
            event_id: row_optional_string(&row, 5, PROJECT_MEMORY_READ_OPERATION)?,
            envelope_operation_id: row_optional_string(&row, 6, PROJECT_MEMORY_READ_OPERATION)?,
            envelope_kind: row_optional_string(&row, 7, PROJECT_MEMORY_READ_OPERATION)?,
            envelope_digest: row_optional_string(&row, 8, PROJECT_MEMORY_READ_OPERATION)?,
            envelope_fact_id: row_optional_string(&row, 9, PROJECT_MEMORY_READ_OPERATION)?,
            envelope_event_id: row_optional_string(&row, 10, PROJECT_MEMORY_READ_OPERATION)?,
            envelope_receipt_json: row_optional_string(&row, 11, PROJECT_MEMORY_READ_OPERATION)?,
        });
    }
    drop(automatic_rows);
    ensure_project_memory_read_active(read_control)?;
    if automatic_ids.len() > MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS {
        return Err(FactStoreError::BatchLimitExceeded {
            field: "memory automation automatic fact receipts",
            count: automatic_ids.len(),
            max: MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS,
        });
    }
    let mut automatic_fact_receipts = Vec::with_capacity(automatic_ids.len());
    for stored in automatic_ids {
        ensure_project_memory_read_active(read_control)?;
        let receipt = project_memory_automatic_fact_receipt_record_tx(
            transaction,
            owner,
            &stored.apply_id,
        )
        .await?
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_READ_OPERATION,
                "memory automation automatic fact receipt disappeared from its read snapshot",
            )
        })?;
        if receipt.request().input_digest() != stored.request_digest {
            return Err(storage_message(
                PROJECT_MEMORY_READ_OPERATION,
                "memory automation automatic fact receipt does not match its immutable request digest",
            ));
        }
        let envelope_receipt = stored
            .envelope_receipt_json
            .as_deref()
            .map(|value| from_json::<serde_json::Value>(value, PROJECT_MEMORY_READ_OPERATION))
            .transpose()?;
        let envelope_apply_id = envelope_receipt
            .as_ref()
            .and_then(|value| value.get("apply_id"))
            .and_then(serde_json::Value::as_str);
        let envelope_state = envelope_receipt
            .as_ref()
            .and_then(|value| value.get("state"))
            .and_then(serde_json::Value::as_str);
        let canonical_state = project_memory_automatic_fact_state_label(receipt.state());
        if stored.idempotency_key != receipt.request().operation_id().as_str()
            || stored.state != canonical_state
            || stored.envelope_operation_id.as_deref() != Some(stored.idempotency_key.as_str())
            || stored.envelope_kind.as_deref() != Some("automatic_fact_apply")
            || stored.envelope_digest.as_deref() != Some(stored.request_digest.as_str())
            || stored.envelope_fact_id != stored.fact_id
            || stored.envelope_event_id != stored.event_id
            || stored.fact_id.as_deref() != receipt.applied_fact_id().map(FactId::as_str)
            || stored.event_id.as_deref() != receipt.applied_event_id().map(FactEventId::as_str)
            || envelope_apply_id != Some(stored.apply_id.as_str())
            || envelope_state != Some(canonical_state)
        {
            return Err(storage_message(
                PROJECT_MEMORY_READ_OPERATION,
                "memory automation automatic fact receipt does not match its immutable operation envelope",
            ));
        }
        automatic_fact_receipts.push(receipt);
    }
    ensure_project_memory_read_active(read_control)?;
    ProjectMemoryAutomationRunReceiptsV1::new(
        owner.clone(),
        run_id.clone(),
        curation_receipt,
        automatic_fact_receipts,
    )
}

#[cfg(test)]
mod tests;
