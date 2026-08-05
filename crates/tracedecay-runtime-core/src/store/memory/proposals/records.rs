//! Proposal request parsing, record projection, and read queries.

use super::super::crud::{payload_metadata, value_strings};
use super::super::envelope::digest;
use super::super::primitives::{
    FACT_READ_OPERATION, OwnerKey, category_label, from_json, nonnegative_u64, row_i64,
    row_optional_string, row_string, storage_error, storage_message, to_json,
};
use super::super::projection::legacy_mapping_tx;
use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use serde_json::{Value, json};
use tracedecay_domain::{
    ActorId, Confidence, FactCategoryV1, FactEventId, FactId, FactOwnerV1, ProvenanceId, UtcMicros,
};
use tracedecay_store::{
    FactAddCommand, FactLineageError, FactLineageResult, FactMapping, FactProposalEvidence,
    FactProposalPage, FactProposalRecord, FactProposalRevision, FactProposalState, FactStoreResult,
    OwnedFactId,
};
const FACT_PROPOSAL_PAGE_LIMIT: usize = 1_000;

pub(super) fn proposal_state_label(state: FactProposalState) -> &'static str {
    match state {
        FactProposalState::PendingApproval => "pending",
        FactProposalState::Applying => "applying",
        FactProposalState::Applied => "applied",
        FactProposalState::Rejected => "rejected",
        FactProposalState::Quarantined => "quarantined",
    }
}

fn proposal_state(value: &str) -> FactLineageResult<FactProposalState> {
    match value {
        "pending" => Ok(FactProposalState::PendingApproval),
        "applying" => Ok(FactProposalState::Applying),
        "applied" => Ok(FactProposalState::Applied),
        "rejected" => Ok(FactProposalState::Rejected),
        "quarantined" => Ok(FactProposalState::Quarantined),
        _ => Err(storage_message(
            FACT_READ_OPERATION,
            format!("unknown fact proposal state {value:?}"),
        )),
    }
}

pub(in crate::store::memory) fn proposal_category(
    value: &str,
) -> FactLineageResult<FactCategoryV1> {
    match value {
        "general" => Ok(FactCategoryV1::General),
        "user_pref" => Ok(FactCategoryV1::UserPref),
        "project" => Ok(FactCategoryV1::Project),
        "tool" => Ok(FactCategoryV1::Tool),
        "decision" => Ok(FactCategoryV1::Decision),
        "code_area" => Ok(FactCategoryV1::CodeArea),
        _ => Err(storage_message(
            FACT_READ_OPERATION,
            format!("unknown fact proposal category {value:?}"),
        )),
    }
}

fn proposal_required_string(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
) -> FactLineageResult<String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            storage_message(
                FACT_READ_OPERATION,
                format!("fact proposal {field} is missing or malformed"),
            )
        })
}

fn proposal_optional_string(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
) -> FactLineageResult<Option<String>> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(storage_message(
            FACT_READ_OPERATION,
            format!("fact proposal {field} is malformed"),
        )),
    }
}

pub(super) fn proposal_request_value(request: &FactAddCommand) -> Value {
    json!({
        "owner": request.owner(),
        "operation_id": request.operation_id().as_str(),
        "content": request.content(),
        "category": category_label(request.category()),
        "source": request.source(),
        "tags": request.tags(),
        "entities": request.entities(),
        "metadata": payload_metadata(request.metadata()),
        "automation_run_id": request.automation_run_id(),
        "default_trust": request.default_trust().as_f64(),
        "actor": request.actor().map(ActorId::as_str),
    })
}

fn proposal_request_from_value(
    owner: &FactOwnerV1,
    value: Value,
) -> FactLineageResult<FactAddCommand> {
    let object = value.as_object().ok_or_else(|| {
        storage_message(
            FACT_READ_OPERATION,
            "fact proposal request is not an object",
        )
    })?;
    let stored_owner = from_json::<FactOwnerV1>(
        &to_json(
            object.get("owner").ok_or_else(|| {
                storage_message(
                    FACT_READ_OPERATION,
                    "fact proposal request owner is missing",
                )
            })?,
            "serialize fact proposal request owner",
        )?,
        FACT_READ_OPERATION,
    )?;
    if &stored_owner != owner {
        return Err(FactLineageError::OwnerMismatch);
    }
    let operation_id = ProvenanceId::new(proposal_required_string(object, "operation_id")?)
        .map_err(FactLineageError::from)?;
    let content = proposal_required_string(object, "content")?;
    let category = proposal_category(&proposal_required_string(object, "category")?)?;
    let source = proposal_optional_string(object, "source")?;
    let tags = value_strings(
        object.get("tags").ok_or_else(|| {
            storage_message(
                FACT_READ_OPERATION,
                "fact proposal request tags are missing",
            )
        })?,
        "proposal tags",
    )?;
    let entities = value_strings(
        object.get("entities").ok_or_else(|| {
            storage_message(
                FACT_READ_OPERATION,
                "fact proposal request entities are missing",
            )
        })?,
        "proposal entities",
    )?;
    let metadata = payload_metadata(&object.get("metadata").cloned().ok_or_else(|| {
        storage_message(
            FACT_READ_OPERATION,
            "fact proposal request metadata is missing",
        )
    })?);
    let automation_run_id = proposal_optional_string(object, "automation_run_id")?;
    let trust = Confidence::new(
        object
            .get("default_trust")
            .and_then(Value::as_f64)
            .ok_or_else(|| {
                storage_message(
                    FACT_READ_OPERATION,
                    "fact proposal request default trust is missing",
                )
            })?,
    )
    .map_err(FactLineageError::from)?;
    let actor = proposal_optional_string(object, "actor")?
        .map(ActorId::new)
        .transpose()
        .map_err(FactLineageError::from)?;
    let request = FactAddCommand::new(
        owner.clone(),
        operation_id,
        content,
        category,
        source,
        tags,
        entities,
        metadata,
        trust,
        actor,
    )?;
    match automation_run_id {
        Some(run_id) => request.with_automation_run_id(run_id),
        None => Ok(request),
    }
}

pub(in crate::store::memory) fn proposal_action_id(
    kind: &'static str,
    material: Value,
) -> FactLineageResult<ProvenanceId> {
    let digest = digest(material)?;
    ProvenanceId::new(format!("compatibility-{kind}:{digest}")).map_err(FactLineageError::from)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn proposal_transition_json(
    proposal_id: &ProvenanceId,
    previous_state: Option<&str>,
    current_state: &str,
    reviewer: Option<&ActorId>,
    reason: Option<&str>,
    request_digest: &str,
    promoted_fact_id: Option<&FactId>,
    promoted_event_id: Option<&FactEventId>,
) -> FactLineageResult<String> {
    to_json(
        &json!({
            "proposal_id": proposal_id.as_str(),
            "previous_state": previous_state,
            "current_state": current_state,
            "reviewer": reviewer.map(ActorId::as_str),
            "reason": reason,
            "request_digest": request_digest,
            "promoted_fact_id": promoted_fact_id.map(FactId::as_str),
            "promoted_event_id": promoted_event_id.map(FactEventId::as_str),
        }),
        "serialize fact proposal transition",
    )
}

pub(in crate::store::memory) async fn proposal_record_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    proposal_id: &ProvenanceId,
) -> FactStoreResult<Option<FactProposalRecord>> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT proposals.proposal_id, proposals.owner_json, proposals.request_json,
                    current_state.state, current_state.revision,
                    transition.reviewer_json, transition.validation_json,
                    transition.promoted_fact_id, proposals.evidence_json,
                    proposals.submitted_at, current_state.updated_at
             FROM memory_v2_proposals AS proposals
             JOIN memory_v2_proposal_current AS current_state
               ON current_state.proposal_id = proposals.proposal_id
              AND current_state.owner_kind = proposals.owner_kind
              AND current_state.project_id = proposals.project_id
             JOIN memory_v2_proposal_transitions AS transition
               ON transition.transition_id = current_state.last_transition_id
              AND transition.proposal_id = current_state.proposal_id
              AND transition.owner_kind = current_state.owner_kind
              AND transition.project_id = current_state.project_id
             WHERE proposals.proposal_id = ?1
               AND proposals.owner_kind = ?2
               AND proposals.project_id = ?3",
            params![proposal_id.as_str(), key.kind, key.project_id.as_str()],
        )
        .await
        .map_err(|error| storage_error(FACT_READ_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(FACT_READ_OPERATION, error))?
    else {
        return Ok(None);
    };
    let stored_id = ProvenanceId::new(row_string(&row, 0, FACT_READ_OPERATION)?)
        .map_err(FactLineageError::from)?;
    if &stored_id != proposal_id {
        return Err(storage_message(FACT_READ_OPERATION, "fact proposal identity mismatch").into());
    }
    if row_string(&row, 1, FACT_READ_OPERATION)? != key.json {
        return Err(FactLineageError::OwnerMismatch.into());
    }
    let request = proposal_request_from_value(
        owner,
        from_json::<Value>(
            &row_string(&row, 2, FACT_READ_OPERATION)?,
            FACT_READ_OPERATION,
        )?,
    )?;
    let state = proposal_state(&row_string(&row, 3, FACT_READ_OPERATION)?)?;
    let revision = FactProposalRevision::new(
        u64::try_from(row_i64(&row, 4, FACT_READ_OPERATION)?).map_err(|_| {
            storage_message(FACT_READ_OPERATION, "fact proposal revision is negative")
        })?,
    )?;
    let reviewer = row_optional_string(&row, 5, FACT_READ_OPERATION)?
        .map(|value| from_json::<ActorId>(&value, FACT_READ_OPERATION))
        .transpose()?;
    let reason = row_optional_string(&row, 6, FACT_READ_OPERATION)?
        .map(|value| from_json::<Value>(&value, FACT_READ_OPERATION))
        .transpose()?
        .and_then(|value| {
            value
                .get("reason")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        });
    let applied_fact_id = row_optional_string(&row, 7, FACT_READ_OPERATION)?
        .map(FactId::new)
        .transpose()
        .map_err(FactLineageError::from)?;
    let applied_mapping = match (&state, &applied_fact_id) {
        (FactProposalState::Applied, Some(fact_id)) => Some(
            legacy_mapping_tx(transaction, owner, fact_id)
                .await?
                .ok_or_else(|| {
                    storage_message(
                        FACT_READ_OPERATION,
                        "applied fact proposal is missing its fact mapping",
                    )
                })?,
        ),
        (FactProposalState::Applied, None) => {
            return Err(storage_message(
                FACT_READ_OPERATION,
                "applied fact proposal is missing its promoted fact",
            )
            .into());
        }
        (_, Some(_)) => {
            return Err(storage_message(
                FACT_READ_OPERATION,
                "non-applied fact proposal has a promoted fact",
            )
            .into());
        }
        (_, None) => None,
    };
    let evidence = from_json::<FactProposalEvidence>(
        &row_string(&row, 8, FACT_READ_OPERATION)?,
        FACT_READ_OPERATION,
    )?;
    let submitted_at = UtcMicros(row_i64(&row, 9, FACT_READ_OPERATION)?);
    let updated_at = UtcMicros(row_i64(&row, 10, FACT_READ_OPERATION)?);
    let mapping = match (applied_mapping, applied_fact_id.as_ref()) {
        (Some(mapping), Some(fact_id)) => Some(FactMapping::new(
            OwnedFactId::new(owner.clone(), fact_id.clone())?,
            Some(mapping),
        )?),
        (None, None) => None,
        _ => {
            return Err(storage_message(
                FACT_READ_OPERATION,
                "fact proposal mapping and fact identity disagree",
            )
            .into());
        }
    };
    FactProposalRecord::new(
        stored_id,
        owner.clone(),
        revision,
        state,
        request,
        applied_fact_id,
        mapping,
        evidence,
        reviewer,
        reason,
        submitted_at,
        updated_at,
    )
    .map(Some)
    .map_err(Into::into)
}

pub(in crate::store::memory) async fn get_fact_proposal_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    proposal_id: &ProvenanceId,
) -> FactStoreResult<Option<FactProposalRecord>> {
    proposal_record_tx(transaction, owner, proposal_id).await
}

pub(in crate::store::memory) async fn list_fact_proposals_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    state: Option<FactProposalState>,
    after_proposal_id: Option<&ProvenanceId>,
    limit: usize,
) -> FactStoreResult<FactProposalPage> {
    if limit == 0 || limit > FACT_PROPOSAL_PAGE_LIMIT {
        return Err(FactLineageError::InvalidQueryLimit {
            limit,
            max: FACT_PROPOSAL_PAGE_LIMIT,
        }
        .into());
    }
    let key = OwnerKey::new(owner)?;
    let fetch_limit = i64::try_from(limit.saturating_add(1)).map_err(|_| {
        FactLineageError::InvalidQueryLimit {
            limit,
            max: FACT_PROPOSAL_PAGE_LIMIT,
        }
    })?;
    let state_label = state.map(proposal_state_label);
    let mut rows = match (state_label, after_proposal_id) {
        (Some(state), Some(after)) => {
            transaction
                .query(
                    "SELECT current_state.proposal_id
                 FROM memory_v2_proposal_current AS current_state
                 JOIN memory_v2_proposals AS proposals
                   ON proposals.proposal_id = current_state.proposal_id
                  AND proposals.owner_kind = current_state.owner_kind
                  AND proposals.project_id = current_state.project_id
                 WHERE current_state.owner_kind = ?1 AND current_state.project_id = ?2
                   AND proposals.owner_json = ?3 AND current_state.state = ?4
                   AND current_state.proposal_id > ?5
                 ORDER BY current_state.proposal_id ASC LIMIT ?6",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        state,
                        after.as_str(),
                        fetch_limit
                    ],
                )
                .await
        }
        (Some(state), None) => {
            transaction
                .query(
                    "SELECT current_state.proposal_id
                 FROM memory_v2_proposal_current AS current_state
                 JOIN memory_v2_proposals AS proposals
                   ON proposals.proposal_id = current_state.proposal_id
                  AND proposals.owner_kind = current_state.owner_kind
                  AND proposals.project_id = current_state.project_id
                 WHERE current_state.owner_kind = ?1 AND current_state.project_id = ?2
                   AND proposals.owner_json = ?3 AND current_state.state = ?4
                 ORDER BY current_state.proposal_id ASC LIMIT ?5",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        state,
                        fetch_limit
                    ],
                )
                .await
        }
        (None, Some(after)) => {
            transaction
                .query(
                    "SELECT current_state.proposal_id
                 FROM memory_v2_proposal_current AS current_state
                 JOIN memory_v2_proposals AS proposals
                   ON proposals.proposal_id = current_state.proposal_id
                  AND proposals.owner_kind = current_state.owner_kind
                  AND proposals.project_id = current_state.project_id
                 WHERE current_state.owner_kind = ?1 AND current_state.project_id = ?2
                   AND proposals.owner_json = ?3 AND current_state.proposal_id > ?4
                 ORDER BY current_state.proposal_id ASC LIMIT ?5",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        after.as_str(),
                        fetch_limit
                    ],
                )
                .await
        }
        (None, None) => {
            transaction
                .query(
                    "SELECT current_state.proposal_id
                 FROM memory_v2_proposal_current AS current_state
                 JOIN memory_v2_proposals AS proposals
                   ON proposals.proposal_id = current_state.proposal_id
                  AND proposals.owner_kind = current_state.owner_kind
                  AND proposals.project_id = current_state.project_id
                 WHERE current_state.owner_kind = ?1 AND current_state.project_id = ?2
                   AND proposals.owner_json = ?3
                 ORDER BY current_state.proposal_id ASC LIMIT ?4",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        fetch_limit
                    ],
                )
                .await
        }
    }
    .map_err(|error| storage_error(FACT_READ_OPERATION, error))?;
    let mut ids = Vec::with_capacity(limit.saturating_add(1));
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(FACT_READ_OPERATION, error))?
    {
        ids.push(
            ProvenanceId::new(row_string(&row, 0, FACT_READ_OPERATION)?)
                .map_err(FactLineageError::from)?,
        );
    }
    drop(rows);
    let has_more = ids.len() > limit;
    ids.truncate(limit);
    let mut proposals = Vec::with_capacity(ids.len());
    for proposal_id in &ids {
        proposals.push(
            proposal_record_tx(transaction, owner, proposal_id)
                .await?
                .ok_or_else(|| {
                    storage_message(
                        FACT_READ_OPERATION,
                        "fact proposal disappeared from its read snapshot",
                    )
                })?,
        );
    }
    FactProposalPage::new(
        owner.clone(),
        proposals,
        has_more.then(|| ids.last().cloned()).flatten(),
    )
    .map_err(Into::into)
}

pub(in crate::store::memory) async fn count_pending_fact_proposals_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
) -> FactStoreResult<u64> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT COUNT(*)
             FROM memory_v2_proposal_current AS current_state
             JOIN memory_v2_proposals AS proposals
               ON proposals.proposal_id = current_state.proposal_id
              AND proposals.owner_kind = current_state.owner_kind
              AND proposals.project_id = current_state.project_id
             WHERE current_state.owner_kind = ?1 AND current_state.project_id = ?2
               AND proposals.owner_json = ?3 AND current_state.state = 'pending'",
            params![key.kind, key.project_id.as_str(), key.json.as_str()],
        )
        .await
        .map_err(|error| storage_error(FACT_READ_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(FACT_READ_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(FACT_READ_OPERATION, "fact proposal count returned no row")
        })?;
    nonnegative_u64(
        row_i64(&row, 0, FACT_READ_OPERATION)?,
        "pending proposal count",
    )
    .map_err(Into::into)
}
