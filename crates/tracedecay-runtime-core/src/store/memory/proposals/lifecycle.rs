//! Proposal digests, submit/advance/reject/replay transitions, and legacy import.

use super::super::crud::{payload_metadata, proposal_transition_id};
use super::super::envelope::{
    OperationReceipt, digest, lookup_operation_receipt_tx, record_operation_receipt_tx,
};
use super::super::primitives::{
    FACT_WRITE_OPERATION, OwnerKey, category_label, now, row_string, storage_error,
    storage_message, to_json,
};
use super::{
    proposal_action_id, proposal_record_tx, proposal_request_value, proposal_state_label,
    proposal_transition_json,
};
use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use serde_json::{Value, json};
use tracedecay_domain::{
    ActorId, FactAssertionId, FactEventId, FactId, FactOwnerV1, ProvenanceId, UtcMicros,
};
use tracedecay_store::{
    FactAddCommand, FactLineageError, FactLineageResult, FactProposalEvidence, FactProposalRecord,
    FactProposalRevision, FactProposalState, FactStoreResult,
};
fn proposal_request_digest(request: &FactAddCommand) -> FactLineageResult<String> {
    digest(json!({
        "owner": request.owner(),
        "content": request.content(),
        "category": category_label(request.category()),
        "source": request.source(),
        "tags": request.tags(),
        "entities": request.entities(),
        "metadata": payload_metadata(request.metadata()),
        "automation_run_id": request.automation_run_id(),
        "default_trust": request.default_trust().as_f64(),
        "actor": request.actor().map(ActorId::as_str),
    }))
}

async fn proposal_digest_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    proposal_id: &ProvenanceId,
) -> FactLineageResult<Option<String>> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT owner_json, request_digest FROM memory_v2_proposals
             WHERE proposal_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
            params![proposal_id.as_str(), key.kind, key.project_id.as_str()],
        )
        .await
        .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?
    else {
        return Ok(None);
    };
    if row_string(&row, 0, FACT_WRITE_OPERATION)? != key.json {
        return Err(FactLineageError::OwnerMismatch);
    }
    Ok(Some(row_string(&row, 1, FACT_WRITE_OPERATION)?))
}

async fn proposal_for_digest_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    request_digest: &str,
) -> FactLineageResult<Option<ProvenanceId>> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT proposal_id, owner_json FROM memory_v2_proposals
             WHERE owner_kind = ?1 AND project_id = ?2 AND request_digest = ?3",
            params![key.kind, key.project_id.as_str(), request_digest],
        )
        .await
        .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?
    else {
        return Ok(None);
    };
    if row_string(&row, 1, FACT_WRITE_OPERATION)? != key.json {
        return Err(FactLineageError::OwnerMismatch);
    }
    ProvenanceId::new(row_string(&row, 0, FACT_WRITE_OPERATION)?)
        .map(Some)
        .map_err(FactLineageError::from)
}

fn proposal_receipt_proposal_id(receipt: &OperationReceipt) -> FactLineageResult<ProvenanceId> {
    let proposal_id = receipt
        .receipt
        .get("proposal_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            storage_message(
                FACT_WRITE_OPERATION,
                "fact proposal receipt is missing its proposal identity",
            )
        })?;
    ProvenanceId::new(proposal_id.to_owned()).map_err(FactLineageError::from)
}

pub(in crate::store::memory) async fn replay_proposal_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    receipt: &OperationReceipt,
) -> FactStoreResult<FactProposalRecord> {
    let proposal_id = proposal_receipt_proposal_id(receipt)?;
    proposal_record_tx(transaction, owner, &proposal_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                FACT_WRITE_OPERATION,
                "fact proposal replay target is missing",
            )
            .into()
        })
}

#[allow(clippy::too_many_arguments)]
async fn insert_proposal_tx(
    transaction: &Transaction<'_>,
    proposal_id: &ProvenanceId,
    request: &FactAddCommand,
    idempotency_key: &ProvenanceId,
    request_digest: &str,
    evidence: &FactProposalEvidence,
    state: FactProposalState,
    reviewer: Option<&ActorId>,
    reason: Option<&str>,
    origin: &'static str,
    occurred_at: UtcMicros,
) -> FactLineageResult<()> {
    let key = OwnerKey::new(request.owner())?;
    let state_label = proposal_state_label(state);
    if matches!(
        state,
        FactProposalState::Applying | FactProposalState::Applied
    ) {
        return Err(storage_message(
            FACT_WRITE_OPERATION,
            "fact proposal initial state is not durable",
        ));
    }
    let transition_json = proposal_transition_json(
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
        .map(|value| to_json(value, "serialize fact proposal reviewer"))
        .transpose()?;
    let validation_json = reason
        .map(|value| {
            to_json(
                &json!({ "reason": value }),
                "serialize fact proposal validation",
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
                    &proposal_request_value(request),
                    "serialize fact proposal request",
                )?,
                to_json(evidence, "serialize fact proposal evidence")?,
                occurred_at.0,
            ],
        )
        .await
        .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?;
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
        .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?;
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
        .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(in crate::store::memory) async fn advance_proposal_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    proposal_id: &ProvenanceId,
    expected_state: FactProposalState,
    expected_revision: FactProposalRevision,
    state: FactProposalState,
    reviewer: Option<&ActorId>,
    reason: Option<&str>,
    request_digest: &str,
    promoted_fact_id: Option<&FactId>,
    promoted_assertion_id: Option<&FactAssertionId>,
    promoted_event_id: Option<&FactEventId>,
    occurred_at: UtcMicros,
) -> FactLineageResult<()> {
    let key = OwnerKey::new(owner)?;
    let expected_label = proposal_state_label(expected_state);
    let state_label = proposal_state_label(state);
    let applied = state == FactProposalState::Applied;
    if applied != (promoted_fact_id.is_some() && promoted_event_id.is_some())
        || (!applied
            && (promoted_fact_id.is_some()
                || promoted_assertion_id.is_some()
                || promoted_event_id.is_some()))
    {
        return Err(storage_message(
            FACT_WRITE_OPERATION,
            "fact proposal transition has inconsistent promoted identities",
        ));
    }
    let transition_json = proposal_transition_json(
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
        .map(|value| to_json(value, "serialize fact proposal reviewer"))
        .transpose()?;
    let validation_json = reason
        .map(|value| {
            to_json(
                &json!({ "reason": value }),
                "serialize fact proposal validation",
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
        .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?;
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
                        FACT_WRITE_OPERATION,
                        "fact proposal revision exceeds storage range",
                    )
                })?,
            ],
        )
        .await
        .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?;
    if changed != 1 {
        return Err(storage_message(
            FACT_WRITE_OPERATION,
            "fact proposal revision or state changed before transition",
        ));
    }
    Ok(())
}

pub(in crate::store::memory) async fn submit_fact_proposal_tx(
    transaction: &Transaction<'_>,
    proposal_id: ProvenanceId,
    request: &FactAddCommand,
    submitter: Option<&ActorId>,
    evidence: &FactProposalEvidence,
) -> FactStoreResult<FactProposalRecord> {
    let request_digest = proposal_request_digest(request)?;
    if let Some(receipt) = lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "proposal_submit",
        &request_digest,
    )
    .await?
    {
        return replay_proposal_tx(transaction, request.owner(), &receipt).await;
    }
    if let Some(existing_digest) =
        proposal_digest_tx(transaction, request.owner(), &proposal_id).await?
    {
        if existing_digest != request_digest {
            return Err(storage_message(
                FACT_WRITE_OPERATION,
                "fact proposal id was reused with a different request",
            )
            .into());
        }
        let proposal = proposal_record_tx(transaction, request.owner(), &proposal_id)
            .await?
            .ok_or_else(|| {
                storage_message(
                    FACT_WRITE_OPERATION,
                    "fact proposal record is missing after identity lookup",
                )
            })?;
        let receipt = json!({
            "proposal_id": proposal.proposal_id().as_str(),
            "state": proposal_state_label(proposal.state()),
        });
        record_operation_receipt_tx(
            transaction,
            request.owner(),
            request.operation_id(),
            "proposal_submit",
            &request_digest,
            proposal.applied_fact_id(),
            None,
            &receipt,
            now()?,
        )
        .await?;
        return Ok(proposal);
    }
    if let Some(existing_id) =
        proposal_for_digest_tx(transaction, request.owner(), &request_digest).await?
    {
        let proposal = proposal_record_tx(transaction, request.owner(), &existing_id)
            .await?
            .ok_or_else(|| {
                storage_message(
                    FACT_WRITE_OPERATION,
                    "fact proposal record is missing after digest lookup",
                )
            })?;
        let receipt = json!({
            "proposal_id": proposal.proposal_id().as_str(),
            "state": proposal_state_label(proposal.state()),
        });
        record_operation_receipt_tx(
            transaction,
            request.owner(),
            request.operation_id(),
            "proposal_submit",
            &request_digest,
            proposal.applied_fact_id(),
            None,
            &receipt,
            now()?,
        )
        .await?;
        return Ok(proposal);
    }
    let now = now()?;
    insert_proposal_tx(
        transaction,
        &proposal_id,
        request,
        request.operation_id(),
        &request_digest,
        evidence,
        FactProposalState::PendingApproval,
        submitter,
        None,
        "runtime",
        now,
    )
    .await?;
    let receipt = json!({ "proposal_id": proposal_id.as_str(), "state": "pending" });
    record_operation_receipt_tx(
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
    replay_proposal_tx(
        transaction,
        request.owner(),
        &OperationReceipt {
            fact_id: None,
            event_id: None,
            receipt,
        },
    )
    .await
}

pub(in crate::store::memory) async fn reject_fact_proposal_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    proposal_id: &ProvenanceId,
    expected_revision: FactProposalRevision,
    reviewer: &ActorId,
    reason: &str,
) -> FactStoreResult<FactProposalRecord> {
    if reason.trim().is_empty() || reason.len() > 4_096 {
        return Err(
            FactLineageError::Contract(tracedecay_domain::DomainError::NonCanonical {
                field: "fact proposal reason",
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
    let request_digest = digest(material.clone())?;
    let operation_id = proposal_action_id("proposal-reject", material)?;
    if let Some(receipt) = lookup_operation_receipt_tx(
        transaction,
        owner,
        &operation_id,
        "proposal_reject",
        &request_digest,
    )
    .await?
    {
        return replay_proposal_tx(transaction, owner, &receipt).await;
    }
    let proposal = proposal_record_tx(transaction, owner, proposal_id)
        .await?
        .ok_or_else(|| storage_message(FACT_WRITE_OPERATION, "fact proposal is missing"))?;
    if proposal.state() != FactProposalState::PendingApproval
        || proposal.revision() != expected_revision
    {
        return Err(storage_message(
            FACT_WRITE_OPERATION,
            "fact proposal revision or state changed before rejection",
        )
        .into());
    }
    let now = now()?;
    advance_proposal_tx(
        transaction,
        owner,
        proposal_id,
        FactProposalState::PendingApproval,
        expected_revision,
        FactProposalState::Rejected,
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
    record_operation_receipt_tx(
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
    replay_proposal_tx(
        transaction,
        owner,
        &OperationReceipt {
            fact_id: None,
            event_id: None,
            receipt,
        },
    )
    .await
}
