//! Proposal request parsing, record projection, and read queries.

use super::super::crud::{compatibility_payload_metadata, compatibility_value_strings};
use super::super::envelope::compatibility_digest;
use super::super::primitives::{
    COMPATIBILITY_READ_OPERATION, OwnerKey, compatibility_category_label, from_json,
    nonnegative_u64, row_i64, row_optional_string, row_string, storage_error, storage_message,
    to_json,
};
use super::super::projection::compatibility_legacy_mapping_tx;
use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use serde_json::{Value, json};
use tracedecay_domain::{
    ActorId, Confidence, FactCategoryV1, FactEventId, FactId, FactOwnerV1, ProvenanceId,
};
use tracedecay_store::{
    CompatibilityFactAddCommandV1, CompatibilityFactIdV1, CompatibilityFactMappingV1,
    CompatibilityFactProposalPageV1, CompatibilityFactProposalRecordV1,
    CompatibilityFactProposalRevisionV1, CompatibilityFactProposalStateV1, FactCompatibilityResult,
    FactStoreError, FactStoreResult,
};
const COMPATIBILITY_PROPOSAL_PAGE_LIMIT: usize = 1_000;

pub(super) fn compatibility_proposal_state_label(
    state: CompatibilityFactProposalStateV1,
) -> &'static str {
    match state {
        CompatibilityFactProposalStateV1::PendingApproval => "pending",
        CompatibilityFactProposalStateV1::Applying => "applying",
        CompatibilityFactProposalStateV1::Applied => "applied",
        CompatibilityFactProposalStateV1::Rejected => "rejected",
        CompatibilityFactProposalStateV1::Quarantined => "quarantined",
    }
}

fn compatibility_proposal_state(value: &str) -> FactStoreResult<CompatibilityFactProposalStateV1> {
    match value {
        "pending" => Ok(CompatibilityFactProposalStateV1::PendingApproval),
        "applying" => Ok(CompatibilityFactProposalStateV1::Applying),
        "applied" => Ok(CompatibilityFactProposalStateV1::Applied),
        "rejected" => Ok(CompatibilityFactProposalStateV1::Rejected),
        "quarantined" => Ok(CompatibilityFactProposalStateV1::Quarantined),
        _ => Err(storage_message(
            COMPATIBILITY_READ_OPERATION,
            format!("unknown compatibility proposal state {value:?}"),
        )),
    }
}

pub(in crate::store::memory) fn compatibility_proposal_category(
    value: &str,
) -> FactStoreResult<FactCategoryV1> {
    match value {
        "general" => Ok(FactCategoryV1::General),
        "user_pref" => Ok(FactCategoryV1::UserPref),
        "project" => Ok(FactCategoryV1::Project),
        "tool" => Ok(FactCategoryV1::Tool),
        "decision" => Ok(FactCategoryV1::Decision),
        "code_area" => Ok(FactCategoryV1::CodeArea),
        _ => Err(storage_message(
            COMPATIBILITY_READ_OPERATION,
            format!("unknown compatibility proposal category {value:?}"),
        )),
    }
}

fn compatibility_proposal_required_string(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
) -> FactStoreResult<String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                format!("compatibility proposal {field} is missing or malformed"),
            )
        })
}

fn compatibility_proposal_optional_string(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
) -> FactStoreResult<Option<String>> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(storage_message(
            COMPATIBILITY_READ_OPERATION,
            format!("compatibility proposal {field} is malformed"),
        )),
    }
}

pub(super) fn compatibility_proposal_request_value(
    request: &CompatibilityFactAddCommandV1,
) -> Value {
    json!({
        "owner": request.owner(),
        "operation_id": request.operation_id().as_str(),
        "content": request.content(),
        "category": compatibility_category_label(request.category()),
        "source": request.source(),
        "tags": request.tags(),
        "entities": request.entities(),
        "metadata": compatibility_payload_metadata(request.metadata()),
        "automation_run_id": request.automation_run_id(),
        "default_trust": request.default_trust().as_f64(),
        "actor": request.actor().map(ActorId::as_str),
    })
}

fn compatibility_proposal_request_from_value(
    owner: &FactOwnerV1,
    value: Value,
) -> FactStoreResult<CompatibilityFactAddCommandV1> {
    let object = value.as_object().ok_or_else(|| {
        storage_message(
            COMPATIBILITY_READ_OPERATION,
            "compatibility proposal request is not an object",
        )
    })?;
    let stored_owner = from_json::<FactOwnerV1>(
        &to_json(
            object.get("owner").ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_READ_OPERATION,
                    "compatibility proposal request owner is missing",
                )
            })?,
            "serialize compatibility proposal request owner",
        )?,
        COMPATIBILITY_READ_OPERATION,
    )?;
    if &stored_owner != owner {
        return Err(FactStoreError::OwnerMismatch);
    }
    let operation_id = ProvenanceId::new(compatibility_proposal_required_string(
        object,
        "operation_id",
    )?)
    .map_err(FactStoreError::from)?;
    let content = compatibility_proposal_required_string(object, "content")?;
    let category = compatibility_proposal_category(&compatibility_proposal_required_string(
        object, "category",
    )?)?;
    let source = compatibility_proposal_optional_string(object, "source")?;
    let tags = compatibility_value_strings(
        object.get("tags").ok_or_else(|| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility proposal request tags are missing",
            )
        })?,
        "proposal tags",
    )?;
    let entities = compatibility_value_strings(
        object.get("entities").ok_or_else(|| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility proposal request entities are missing",
            )
        })?,
        "proposal entities",
    )?;
    let metadata =
        compatibility_payload_metadata(&object.get("metadata").cloned().ok_or_else(|| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility proposal request metadata is missing",
            )
        })?);
    let automation_run_id = compatibility_proposal_optional_string(object, "automation_run_id")?;
    let trust = Confidence::new(
        object
            .get("default_trust")
            .and_then(Value::as_f64)
            .ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_READ_OPERATION,
                    "compatibility proposal request default trust is missing",
                )
            })?,
    )
    .map_err(FactStoreError::from)?;
    let actor = compatibility_proposal_optional_string(object, "actor")?
        .map(ActorId::new)
        .transpose()
        .map_err(FactStoreError::from)?;
    let request = CompatibilityFactAddCommandV1::new(
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

pub(in crate::store::memory) fn compatibility_proposal_action_id(
    kind: &'static str,
    material: Value,
) -> FactStoreResult<ProvenanceId> {
    let digest = compatibility_digest(material)?;
    ProvenanceId::new(format!("compatibility-{kind}:{digest}")).map_err(FactStoreError::from)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn compatibility_proposal_transition_json(
    proposal_id: &ProvenanceId,
    previous_state: Option<&str>,
    current_state: &str,
    reviewer: Option<&ActorId>,
    reason: Option<&str>,
    request_digest: &str,
    promoted_fact_id: Option<&FactId>,
    promoted_event_id: Option<&FactEventId>,
) -> FactStoreResult<String> {
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
        "serialize compatibility proposal transition",
    )
}

pub(in crate::store::memory) async fn compatibility_proposal_record_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    proposal_id: &ProvenanceId,
) -> FactCompatibilityResult<Option<CompatibilityFactProposalRecordV1>> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT proposals.proposal_id, proposals.owner_json, proposals.request_json,
                    current_state.state, current_state.revision,
                    transition.reviewer_json, transition.validation_json,
                    transition.promoted_fact_id
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
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    else {
        return Ok(None);
    };
    let stored_id = ProvenanceId::new(row_string(&row, 0, COMPATIBILITY_READ_OPERATION)?)
        .map_err(FactStoreError::from)?;
    if &stored_id != proposal_id {
        return Err(storage_message(
            COMPATIBILITY_READ_OPERATION,
            "compatibility proposal identity mismatch",
        )
        .into());
    }
    if row_string(&row, 1, COMPATIBILITY_READ_OPERATION)? != key.json {
        return Err(FactStoreError::OwnerMismatch.into());
    }
    let request = compatibility_proposal_request_from_value(
        owner,
        from_json::<Value>(
            &row_string(&row, 2, COMPATIBILITY_READ_OPERATION)?,
            COMPATIBILITY_READ_OPERATION,
        )?,
    )?;
    let state = compatibility_proposal_state(&row_string(&row, 3, COMPATIBILITY_READ_OPERATION)?)?;
    let revision = CompatibilityFactProposalRevisionV1::new(
        u64::try_from(row_i64(&row, 4, COMPATIBILITY_READ_OPERATION)?).map_err(|_| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility proposal revision is negative",
            )
        })?,
    )?;
    let reviewer = row_optional_string(&row, 5, COMPATIBILITY_READ_OPERATION)?
        .map(|value| from_json::<ActorId>(&value, COMPATIBILITY_READ_OPERATION))
        .transpose()?;
    let reason = row_optional_string(&row, 6, COMPATIBILITY_READ_OPERATION)?
        .map(|value| from_json::<Value>(&value, COMPATIBILITY_READ_OPERATION))
        .transpose()?
        .and_then(|value| {
            value
                .get("reason")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        });
    let applied_fact_id = row_optional_string(&row, 7, COMPATIBILITY_READ_OPERATION)?
        .map(FactId::new)
        .transpose()
        .map_err(FactStoreError::from)?;
    let applied_mapping = match (&state, &applied_fact_id) {
        (CompatibilityFactProposalStateV1::Applied, Some(fact_id)) => Some(
            compatibility_legacy_mapping_tx(transaction, owner, fact_id)
                .await?
                .ok_or_else(|| {
                    storage_message(
                        COMPATIBILITY_READ_OPERATION,
                        "applied compatibility proposal is missing its fixed legacy mapping",
                    )
                })?,
        ),
        (CompatibilityFactProposalStateV1::Applied, None) => {
            return Err(storage_message(
                COMPATIBILITY_READ_OPERATION,
                "applied compatibility proposal is missing its promoted fact",
            )
            .into());
        }
        (_, Some(_)) => {
            return Err(storage_message(
                COMPATIBILITY_READ_OPERATION,
                "non-applied compatibility proposal has a promoted fact",
            )
            .into());
        }
        (_, None) => None,
    };
    let mapping = match (applied_mapping, applied_fact_id.as_ref()) {
        (Some(mapping), Some(fact_id)) => Some(CompatibilityFactMappingV1::new(
            CompatibilityFactIdV1::new(owner.clone(), fact_id.clone())?,
            Some(mapping),
        )?),
        (None, None) => None,
        _ => {
            return Err(storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility proposal mapping and fact identity disagree",
            )
            .into());
        }
    };
    CompatibilityFactProposalRecordV1::new(
        stored_id,
        owner.clone(),
        revision,
        state,
        request,
        applied_fact_id,
        mapping,
        reviewer,
        reason,
    )
    .map(Some)
    .map_err(Into::into)
}

pub(in crate::store::memory) async fn get_compatibility_fact_proposal_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    proposal_id: &ProvenanceId,
) -> FactCompatibilityResult<Option<CompatibilityFactProposalRecordV1>> {
    compatibility_proposal_record_tx(transaction, owner, proposal_id).await
}

pub(in crate::store::memory) async fn list_compatibility_fact_proposals_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    state: Option<CompatibilityFactProposalStateV1>,
    after_proposal_id: Option<&ProvenanceId>,
    limit: usize,
) -> FactCompatibilityResult<CompatibilityFactProposalPageV1> {
    if limit == 0 || limit > COMPATIBILITY_PROPOSAL_PAGE_LIMIT {
        return Err(FactStoreError::InvalidQueryLimit {
            limit,
            max: COMPATIBILITY_PROPOSAL_PAGE_LIMIT,
        }
        .into());
    }
    let key = OwnerKey::new(owner)?;
    let fetch_limit =
        i64::try_from(limit.saturating_add(1)).map_err(|_| FactStoreError::InvalidQueryLimit {
            limit,
            max: COMPATIBILITY_PROPOSAL_PAGE_LIMIT,
        })?;
    let state_label = state.map(compatibility_proposal_state_label);
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
    .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut ids = Vec::with_capacity(limit.saturating_add(1));
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        ids.push(
            ProvenanceId::new(row_string(&row, 0, COMPATIBILITY_READ_OPERATION)?)
                .map_err(FactStoreError::from)?,
        );
    }
    drop(rows);
    let has_more = ids.len() > limit;
    ids.truncate(limit);
    let mut proposals = Vec::with_capacity(ids.len());
    for proposal_id in &ids {
        proposals.push(
            compatibility_proposal_record_tx(transaction, owner, proposal_id)
                .await?
                .ok_or_else(|| {
                    storage_message(
                        COMPATIBILITY_READ_OPERATION,
                        "compatibility proposal disappeared from its read snapshot",
                    )
                })?,
        );
    }
    CompatibilityFactProposalPageV1::new(
        owner.clone(),
        proposals,
        has_more.then(|| ids.last().cloned()).flatten(),
    )
    .map_err(Into::into)
}

pub(in crate::store::memory) async fn count_pending_compatibility_fact_proposals_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
) -> FactCompatibilityResult<u64> {
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
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility proposal count returned no row",
            )
        })?;
    nonnegative_u64(
        row_i64(&row, 0, COMPATIBILITY_READ_OPERATION)?,
        "pending proposal count",
    )
    .map_err(Into::into)
}
