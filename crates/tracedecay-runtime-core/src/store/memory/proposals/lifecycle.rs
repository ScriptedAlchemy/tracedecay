//! Proposal digests, submit/advance/reject/replay transitions, and legacy import.

use super::super::crud::{compatibility_payload_metadata, proposal_transition_id};
use super::super::envelope::{
    CompatibilityOperationReceiptV1, compatibility_digest,
    compatibility_lookup_operation_receipt_tx, compatibility_record_operation_receipt_tx,
};
use super::super::primitives::{
    COMPATIBILITY_WRITE_OPERATION, OwnerKey, compatibility_category_label, compatibility_now,
    compatibility_source_store_id, from_json, row_string, storage_error, storage_message, to_json,
};
use super::{
    compatibility_proposal_action_id, compatibility_proposal_record_tx,
    compatibility_proposal_request_value, compatibility_proposal_state_label,
    compatibility_proposal_transition_json,
};
use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use serde_json::{Value, json};
use tracedecay_domain::{
    ActorId, FactAssertionId, FactEventId, FactId, FactOwnerV1, ProvenanceId, SourceStoreId,
    UtcMicros,
};
use tracedecay_store::{
    ProjectMemoryFactAddCommandV1, ProjectMemoryFactProposalImportReceiptV1,
    ProjectMemoryFactProposalImportV1, ProjectMemoryFactProposalRecordV1,
    ProjectMemoryFactProposalRevisionV1, ProjectMemoryFactProposalStateV1, FactCompatibilityResult,
    FactStoreError, FactStoreResult,
};
fn compatibility_proposal_request_digest(
    request: &ProjectMemoryFactAddCommandV1,
) -> FactStoreResult<String> {
    compatibility_digest(json!({
        "owner": request.owner(),
        "content": request.content(),
        "category": compatibility_category_label(request.category()),
        "source": request.source(),
        "tags": request.tags(),
        "entities": request.entities(),
        "metadata": compatibility_payload_metadata(request.metadata()),
        "sanitization_receipt": request.sanitization_receipt(),
        "automation_run_id": request.automation_run_id(),
        "default_trust": request.default_trust().as_f64(),
        "actor": request.actor().map(ActorId::as_str),
    }))
}

async fn compatibility_proposal_digest_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    proposal_id: &ProvenanceId,
) -> FactStoreResult<Option<String>> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT owner_json, request_digest FROM memory_v2_proposals
             WHERE proposal_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
            params![proposal_id.as_str(), key.kind, key.project_id.as_str()],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    else {
        return Ok(None);
    };
    if row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)? != key.json {
        return Err(FactStoreError::OwnerMismatch);
    }
    Ok(Some(row_string(&row, 1, COMPATIBILITY_WRITE_OPERATION)?))
}

async fn compatibility_proposal_for_digest_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    request_digest: &str,
) -> FactStoreResult<Option<ProvenanceId>> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT proposal_id, owner_json FROM memory_v2_proposals
             WHERE owner_kind = ?1 AND project_id = ?2 AND request_digest = ?3",
            params![key.kind, key.project_id.as_str(), request_digest],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    else {
        return Ok(None);
    };
    if row_string(&row, 1, COMPATIBILITY_WRITE_OPERATION)? != key.json {
        return Err(FactStoreError::OwnerMismatch);
    }
    ProvenanceId::new(row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?)
        .map(Some)
        .map_err(FactStoreError::from)
}

fn compatibility_proposal_receipt_proposal_id(
    receipt: &CompatibilityOperationReceiptV1,
) -> FactStoreResult<ProvenanceId> {
    let proposal_id = receipt
        .receipt
        .get("proposal_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility proposal receipt is missing its proposal identity",
            )
        })?;
    ProvenanceId::new(proposal_id.to_owned()).map_err(FactStoreError::from)
}

pub(in crate::store::memory) async fn compatibility_replay_proposal_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    receipt: &CompatibilityOperationReceiptV1,
) -> FactCompatibilityResult<ProjectMemoryFactProposalRecordV1> {
    let proposal_id = compatibility_proposal_receipt_proposal_id(receipt)?;
    compatibility_proposal_record_tx(transaction, owner, &proposal_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility proposal replay target is missing",
            )
            .into()
        })
}

#[allow(clippy::too_many_arguments)]
async fn compatibility_insert_proposal_tx(
    transaction: &Transaction<'_>,
    proposal_id: &ProvenanceId,
    request: &ProjectMemoryFactAddCommandV1,
    idempotency_key: &ProvenanceId,
    request_digest: &str,
    evidence: &Value,
    state: ProjectMemoryFactProposalStateV1,
    reviewer: Option<&ActorId>,
    reason: Option<&str>,
    origin: &'static str,
    occurred_at: UtcMicros,
) -> FactStoreResult<()> {
    let key = OwnerKey::new(request.owner())?;
    let state_label = compatibility_proposal_state_label(state);
    if matches!(
        state,
        ProjectMemoryFactProposalStateV1::Applying | ProjectMemoryFactProposalStateV1::Applied
    ) {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility proposal initial state is not durable in V22",
        ));
    }
    let transition_json = compatibility_proposal_transition_json(
        proposal_id,
        None,
        state_label,
        reviewer,
        reason,
        request_digest,
        None,
        None,
    )?;
    let transition_id = proposal_transition_id(&transition_json);
    let reviewer_json = reviewer
        .map(|value| to_json(value, "serialize compatibility proposal reviewer"))
        .transpose()?;
    let validation_json = reason
        .map(|value| {
            to_json(
                &json!({ "reason": value }),
                "serialize compatibility proposal validation",
            )
        })
        .transpose()?;
    transaction
        .execute(
            "INSERT INTO memory_v2_proposals(
                proposal_id, owner_kind, project_id, owner_json, idempotency_key,
                request_digest, request_json, evidence_json, submitted_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                proposal_id.as_str(),
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                idempotency_key.as_str(),
                request_digest,
                to_json(
                    &compatibility_proposal_request_value(request),
                    "serialize compatibility proposal request",
                )?,
                to_json(evidence, "serialize compatibility proposal evidence")?,
                occurred_at.0,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    transaction
        .execute(
            "INSERT INTO memory_v2_proposal_transitions(
                transition_id, proposal_id, owner_kind, project_id, previous_state,
                current_state, reviewer_json, validation_json, origin,
                promoted_fact_id, promoted_assertion_id, promoted_event_id,
                transition_json, occurred_at
             ) VALUES(?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?8,
                      NULL, NULL, NULL, ?9, ?10)",
            params![
                transition_id.as_str(),
                proposal_id.as_str(),
                key.kind,
                key.project_id.as_str(),
                state_label,
                reviewer_json,
                validation_json,
                origin,
                transition_json,
                occurred_at.0,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    transaction
        .execute(
            "INSERT INTO memory_v2_proposal_current(
                proposal_id, owner_kind, project_id, state, revision,
                last_transition_id, updated_at
             ) VALUES(?1, ?2, ?3, ?4, 1, ?5, ?6)",
            params![
                proposal_id.as_str(),
                key.kind,
                key.project_id.as_str(),
                state_label,
                transition_id.as_str(),
                occurred_at.0,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(in crate::store::memory) async fn compatibility_advance_proposal_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    proposal_id: &ProvenanceId,
    expected_state: ProjectMemoryFactProposalStateV1,
    expected_revision: ProjectMemoryFactProposalRevisionV1,
    state: ProjectMemoryFactProposalStateV1,
    reviewer: Option<&ActorId>,
    reason: Option<&str>,
    request_digest: &str,
    promoted_fact_id: Option<&FactId>,
    promoted_assertion_id: Option<&FactAssertionId>,
    promoted_event_id: Option<&FactEventId>,
    occurred_at: UtcMicros,
) -> FactStoreResult<()> {
    let key = OwnerKey::new(owner)?;
    let expected_label = compatibility_proposal_state_label(expected_state);
    let state_label = compatibility_proposal_state_label(state);
    let applied = state == ProjectMemoryFactProposalStateV1::Applied;
    if applied != (promoted_fact_id.is_some() && promoted_event_id.is_some())
        || (!applied
            && (promoted_fact_id.is_some()
                || promoted_assertion_id.is_some()
                || promoted_event_id.is_some()))
    {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility proposal transition has inconsistent promoted identities",
        ));
    }
    let transition_json = compatibility_proposal_transition_json(
        proposal_id,
        Some(expected_label),
        state_label,
        reviewer,
        reason,
        request_digest,
        promoted_fact_id,
        promoted_event_id,
    )?;
    let transition_id = proposal_transition_id(&transition_json);
    let reviewer_json = reviewer
        .map(|value| to_json(value, "serialize compatibility proposal reviewer"))
        .transpose()?;
    let validation_json = reason
        .map(|value| {
            to_json(
                &json!({ "reason": value }),
                "serialize compatibility proposal validation",
            )
        })
        .transpose()?;
    transaction
        .execute(
            "INSERT INTO memory_v2_proposal_transitions(
                transition_id, proposal_id, owner_kind, project_id, previous_state,
                current_state, reviewer_json, validation_json, origin,
                promoted_fact_id, promoted_assertion_id, promoted_event_id,
                transition_json, occurred_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'runtime',
                      ?9, ?10, ?11, ?12, ?13)",
            params![
                transition_id.as_str(),
                proposal_id.as_str(),
                key.kind,
                key.project_id.as_str(),
                expected_label,
                state_label,
                reviewer_json,
                validation_json,
                promoted_fact_id.map(FactId::as_str),
                promoted_assertion_id.map(FactAssertionId::as_str),
                promoted_event_id.map(FactEventId::as_str),
                transition_json,
                occurred_at.0,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let changed = transaction
        .execute(
            "UPDATE memory_v2_proposal_current
             SET state = ?1, revision = revision + 1,
                 last_transition_id = ?2, updated_at = ?3
             WHERE proposal_id = ?4 AND owner_kind = ?5 AND project_id = ?6
               AND state = ?7 AND revision = ?8",
            params![
                state_label,
                transition_id.as_str(),
                occurred_at.0,
                proposal_id.as_str(),
                key.kind,
                key.project_id.as_str(),
                expected_label,
                i64::try_from(expected_revision.get()).map_err(|_| {
                    storage_message(
                        COMPATIBILITY_WRITE_OPERATION,
                        "compatibility proposal revision exceeds storage range",
                    )
                })?,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    if changed != 1 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility proposal revision or state changed before transition",
        ));
    }
    Ok(())
}

pub(in crate::store::memory) async fn submit_compatibility_fact_proposal_tx(
    transaction: &Transaction<'_>,
    proposal_id: ProvenanceId,
    request: &ProjectMemoryFactAddCommandV1,
    submitter: Option<&ActorId>,
) -> FactCompatibilityResult<ProjectMemoryFactProposalRecordV1> {
    let request_digest = compatibility_proposal_request_digest(request)?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "proposal_submit",
        &request_digest,
    )
    .await?
    {
        return compatibility_replay_proposal_tx(transaction, request.owner(), &receipt).await;
    }
    if let Some(existing_digest) =
        compatibility_proposal_digest_tx(transaction, request.owner(), &proposal_id).await?
    {
        if existing_digest != request_digest {
            return Err(storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility proposal id was reused with a different request",
            )
            .into());
        }
        let proposal = compatibility_proposal_record_tx(transaction, request.owner(), &proposal_id)
            .await?
            .ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility proposal record is missing after identity lookup",
                )
            })?;
        let receipt = json!({
            "proposal_id": proposal.proposal_id().as_str(),
            "state": compatibility_proposal_state_label(proposal.state()),
        });
        compatibility_record_operation_receipt_tx(
            transaction,
            request.owner(),
            request.operation_id(),
            "proposal_submit",
            &request_digest,
            proposal.applied_fact_id(),
            None,
            &receipt,
            compatibility_now()?,
        )
        .await?;
        return Ok(proposal);
    }
    if let Some(existing_id) =
        compatibility_proposal_for_digest_tx(transaction, request.owner(), &request_digest).await?
    {
        let proposal = compatibility_proposal_record_tx(transaction, request.owner(), &existing_id)
            .await?
            .ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility proposal record is missing after digest lookup",
                )
            })?;
        let receipt = json!({
            "proposal_id": proposal.proposal_id().as_str(),
            "state": compatibility_proposal_state_label(proposal.state()),
        });
        compatibility_record_operation_receipt_tx(
            transaction,
            request.owner(),
            request.operation_id(),
            "proposal_submit",
            &request_digest,
            proposal.applied_fact_id(),
            None,
            &receipt,
            compatibility_now()?,
        )
        .await?;
        return Ok(proposal);
    }
    let now = compatibility_now()?;
    compatibility_insert_proposal_tx(
        transaction,
        &proposal_id,
        request,
        request.operation_id(),
        &request_digest,
        &json!({ "kind": "compatibility-proposal-v1" }),
        ProjectMemoryFactProposalStateV1::PendingApproval,
        submitter,
        None,
        "runtime",
        now,
    )
    .await?;
    let receipt = json!({ "proposal_id": proposal_id.as_str(), "state": "pending" });
    compatibility_record_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "proposal_submit",
        &request_digest,
        None,
        None,
        &receipt,
        now,
    )
    .await?;
    compatibility_replay_proposal_tx(
        transaction,
        request.owner(),
        &CompatibilityOperationReceiptV1 {
            fact_id: None,
            event_id: None,
            receipt,
        },
    )
    .await
}

pub(in crate::store::memory) async fn reject_compatibility_fact_proposal_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    proposal_id: &ProvenanceId,
    expected_revision: ProjectMemoryFactProposalRevisionV1,
    reviewer: &ActorId,
    reason: &str,
) -> FactCompatibilityResult<ProjectMemoryFactProposalRecordV1> {
    if reason.trim().is_empty() || reason.len() > 4_096 {
        return Err(
            FactStoreError::Contract(tracedecay_domain::DomainError::NonCanonical {
                field: "compatibility fact proposal reason",
            })
            .into(),
        );
    }
    let material = json!({
        "proposal_id": proposal_id.as_str(),
        "expected_revision": expected_revision.get(),
        "reviewer": reviewer.as_str(),
        "reason": reason,
    });
    let request_digest = compatibility_digest(material.clone())?;
    let operation_id = compatibility_proposal_action_id("proposal-reject", material)?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        owner,
        &operation_id,
        "proposal_reject",
        &request_digest,
    )
    .await?
    {
        return compatibility_replay_proposal_tx(transaction, owner, &receipt).await;
    }
    let proposal = compatibility_proposal_record_tx(transaction, owner, proposal_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility proposal is missing",
            )
        })?;
    if proposal.state() != ProjectMemoryFactProposalStateV1::PendingApproval
        || proposal.revision() != expected_revision
    {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility proposal revision or state changed before rejection",
        )
        .into());
    }
    let now = compatibility_now()?;
    compatibility_advance_proposal_tx(
        transaction,
        owner,
        proposal_id,
        ProjectMemoryFactProposalStateV1::PendingApproval,
        expected_revision,
        ProjectMemoryFactProposalStateV1::Rejected,
        Some(reviewer),
        Some(reason),
        &request_digest,
        None,
        None,
        None,
        now,
    )
    .await?;
    let receipt = json!({
        "proposal_id": proposal_id.as_str(),
        "state": "rejected",
        "revision": expected_revision.get().saturating_add(1),
    });
    compatibility_record_operation_receipt_tx(
        transaction,
        owner,
        &operation_id,
        "proposal_reject",
        &request_digest,
        None,
        None,
        &receipt,
        now,
    )
    .await?;
    compatibility_replay_proposal_tx(
        transaction,
        owner,
        &CompatibilityOperationReceiptV1 {
            fact_id: None,
            event_id: None,
            receipt,
        },
    )
    .await
}

async fn compatibility_legacy_proposal_mapping_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    legacy_proposal_id: i64,
) -> FactStoreResult<Option<(ProvenanceId, Value)>> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT mappings.proposal_id, proposals.owner_json, mappings.import_receipt_json
             FROM memory_v2_legacy_proposal_map AS mappings
             JOIN memory_v2_proposals AS proposals
               ON proposals.proposal_id = mappings.proposal_id
              AND proposals.owner_kind = mappings.owner_kind
              AND proposals.project_id = mappings.project_id
             WHERE mappings.owner_kind = ?1 AND mappings.project_id = ?2
               AND mappings.source_store_id = ?3 AND mappings.legacy_proposal_id = ?4",
            params![
                key.kind,
                key.project_id.as_str(),
                source_store_id.as_str(),
                legacy_proposal_id.to_string(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    else {
        return Ok(None);
    };
    if row_string(&row, 1, COMPATIBILITY_WRITE_OPERATION)? != key.json {
        return Err(FactStoreError::OwnerMismatch);
    }
    let proposal_id = ProvenanceId::new(row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?)
        .map_err(FactStoreError::from)?;
    let import_receipt = from_json::<Value>(
        &row_string(&row, 2, COMPATIBILITY_WRITE_OPERATION)?,
        COMPATIBILITY_WRITE_OPERATION,
    )?;
    Ok(Some((proposal_id, import_receipt)))
}

fn compatibility_import_receipt_from_value(
    request: &ProjectMemoryFactProposalImportV1,
    receipt: &Value,
) -> FactStoreResult<ProjectMemoryFactProposalImportReceiptV1> {
    let imported_count = receipt
        .get("imported_count")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility proposal import receipt is malformed",
            )
        })?;
    let quarantined_count = receipt
        .get("quarantined_count")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility proposal import receipt is malformed",
            )
        })?;
    ProjectMemoryFactProposalImportReceiptV1::new(
        request.owner().clone(),
        request.source_store_id().clone(),
        request.sidecar_digest().clone(),
        imported_count,
        quarantined_count,
    )
}

fn compatibility_import_initial_state(
    state: ProjectMemoryFactProposalStateV1,
) -> (ProjectMemoryFactProposalStateV1, Option<&'static str>) {
    match state {
        ProjectMemoryFactProposalStateV1::PendingApproval => {
            (ProjectMemoryFactProposalStateV1::PendingApproval, None)
        }
        ProjectMemoryFactProposalStateV1::Applying => (
            ProjectMemoryFactProposalStateV1::PendingApproval,
            Some("legacy applying state normalized to pending"),
        ),
        ProjectMemoryFactProposalStateV1::Rejected => {
            (ProjectMemoryFactProposalStateV1::Rejected, None)
        }
        ProjectMemoryFactProposalStateV1::Quarantined => (
            ProjectMemoryFactProposalStateV1::Quarantined,
            Some("legacy proposal was quarantined"),
        ),
        ProjectMemoryFactProposalStateV1::Applied => (
            ProjectMemoryFactProposalStateV1::Quarantined,
            Some("legacy applied proposal lacks a verifiable canonical promotion"),
        ),
    }
}

pub(in crate::store::memory) async fn import_legacy_compatibility_fact_proposals_tx(
    transaction: &Transaction<'_>,
    request: &ProjectMemoryFactProposalImportV1,
) -> FactCompatibilityResult<ProjectMemoryFactProposalImportReceiptV1> {
    let fixed_source_store_id = compatibility_source_store_id()?;
    if request.source_store_id() != &fixed_source_store_id {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility proposal imports require the fixed legacy-memory-v1 source store",
        )
        .into());
    }
    let records = request
        .records()
        .iter()
        .map(|record| {
            Ok::<_, FactStoreError>(json!({
                "legacy_proposal_id": record.legacy_proposal_id(),
                "state": compatibility_proposal_state_label(record.state()),
                "request_digest": compatibility_proposal_request_digest(record.request())?,
            }))
        })
        .collect::<FactStoreResult<Vec<_>>>()?;
    let material = json!({
        "owner": request.owner(),
        "source_store_id": request.source_store_id().as_str(),
        "sidecar_digest": request.sidecar_digest().as_str(),
        "records": records,
    });
    let request_digest = compatibility_digest(material.clone())?;
    let operation_id = compatibility_proposal_action_id("proposal-import", material)?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        &operation_id,
        "proposal_import",
        &request_digest,
    )
    .await?
    {
        return compatibility_import_receipt_from_value(request, &receipt.receipt)
            .map_err(Into::into);
    }
    let now = compatibility_now()?;
    let mut imported_count = 0_usize;
    let mut quarantined_count = 0_usize;
    for record in request.records() {
        let legacy_proposal_id = record.legacy_proposal_id();
        let record_digest = compatibility_proposal_request_digest(record.request())?;
        let (state, reason) = compatibility_import_initial_state(record.state());
        let proposal_id = compatibility_proposal_action_id(
            "legacy-proposal",
            json!({
                "source_store_id": request.source_store_id().as_str(),
                "legacy_proposal_id": legacy_proposal_id,
            }),
        )?;
        let resolved_id = if let Some((existing_id, import_receipt)) =
            compatibility_legacy_proposal_mapping_tx(
                transaction,
                request.owner(),
                request.source_store_id(),
                legacy_proposal_id,
            )
            .await?
        {
            if import_receipt.get("sidecar_digest").and_then(Value::as_str)
                != Some(request.sidecar_digest().as_str())
            {
                return Err(storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "legacy proposal id was reused with a different sidecar digest",
                )
                .into());
            }
            let stored_digest =
                compatibility_proposal_digest_tx(transaction, request.owner(), &existing_id)
                    .await?
                    .ok_or_else(|| {
                        storage_message(
                            COMPATIBILITY_WRITE_OPERATION,
                            "legacy proposal map references a missing proposal",
                        )
                    })?;
            if stored_digest != record_digest {
                return Err(storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "legacy proposal id was reused with a different request",
                )
                .into());
            }
            existing_id
        } else {
            if let Some(existing_id) =
                compatibility_proposal_for_digest_tx(transaction, request.owner(), &record_digest)
                    .await?
            {
                return Err(storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    format!(
                        "legacy proposal request is already bound to proposal {}",
                        existing_id.as_str()
                    ),
                )
                .into());
            }
            compatibility_insert_proposal_tx(
                transaction,
                &proposal_id,
                record.request(),
                &proposal_id,
                &record_digest,
                &json!({
                    "source_store_id": request.source_store_id().as_str(),
                    "sidecar_digest": request.sidecar_digest().as_str(),
                    "legacy_proposal_id": legacy_proposal_id,
                }),
                state,
                None,
                reason,
                "legacy_import",
                now,
            )
            .await?;
            transaction
                .execute(
                    "INSERT INTO memory_v2_legacy_proposal_map(
                        owner_kind, project_id, source_store_id, legacy_proposal_id,
                        proposal_id, history_coverage, import_receipt_json, imported_at
                     ) VALUES(?1, ?2, ?3, ?4, ?5, 'unknown', ?6, ?7)",
                    params![
                        OwnerKey::new(request.owner())?.kind,
                        OwnerKey::new(request.owner())?.project_id.as_str(),
                        request.source_store_id().as_str(),
                        legacy_proposal_id.to_string(),
                        proposal_id.as_str(),
                        to_json(
                            &json!({
                                "source_store_id": request.source_store_id().as_str(),
                                "sidecar_digest": request.sidecar_digest().as_str(),
                                "request_digest": record_digest,
                            }),
                            "serialize compatibility legacy proposal import receipt",
                        )?,
                        now.0,
                    ],
                )
                .await
                .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
            proposal_id
        };
        let proposal = compatibility_proposal_record_tx(transaction, request.owner(), &resolved_id)
            .await?
            .ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility proposal is missing after legacy import",
                )
            })?;
        if proposal.state() == ProjectMemoryFactProposalStateV1::Quarantined {
            quarantined_count = quarantined_count.saturating_add(1);
        } else {
            imported_count = imported_count.saturating_add(1);
        }
    }
    let receipt = json!({
        "imported_count": imported_count,
        "quarantined_count": quarantined_count,
    });
    compatibility_record_operation_receipt_tx(
        transaction,
        request.owner(),
        &operation_id,
        "proposal_import",
        &request_digest,
        None,
        None,
        &receipt,
        now,
    )
    .await?;
    ProjectMemoryFactProposalImportReceiptV1::new(
        request.owner().clone(),
        request.source_store_id().clone(),
        request.sidecar_digest().clone(),
        imported_count,
        quarantined_count,
    )
    .map_err(Into::into)
}
