//! Compatibility fact-proposal lifecycle, transitions, and legacy imports.

use libsql::{Transaction, params};
use serde_json::{Value, json};

use tracedecay_domain::{
    ActorId, Confidence, FactAssertionId, FactCategoryV1, FactEventId, FactId, FactOwnerV1,
    ProvenanceId, SourceStoreId, UtcMicros,
};
use tracedecay_store::{
    CompatibilityFactAddCommandV1, CompatibilityFactIdV1, CompatibilityFactMappingV1,
    CompatibilityFactProposalImportReceiptV1, CompatibilityFactProposalImportV1,
    CompatibilityFactProposalPageV1, CompatibilityFactProposalRecordV1,
    CompatibilityFactProposalRevisionV1, CompatibilityFactProposalStateV1, FactCompatibilityResult,
    FactStoreError, FactStoreResult,
};

use super::crud::{
    compatibility_payload_metadata, compatibility_value_strings, proposal_transition_id,
};
use super::envelope::{
    CompatibilityOperationReceiptV1, compatibility_digest,
    compatibility_lookup_operation_receipt_tx, compatibility_record_operation_receipt_tx,
};
use super::primitives::{
    COMPATIBILITY_READ_OPERATION, COMPATIBILITY_WRITE_OPERATION, OwnerKey,
    compatibility_category_label, compatibility_now, compatibility_source_store_id, from_json,
    nonnegative_u64, row_i64, row_optional_string, row_string, storage_error, storage_message,
    to_json,
};
use super::projection::compatibility_legacy_mapping_tx;

const COMPATIBILITY_PROPOSAL_PAGE_LIMIT: usize = 1_000;

fn compatibility_proposal_state_label(state: CompatibilityFactProposalStateV1) -> &'static str {
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

pub(super) fn compatibility_proposal_category(value: &str) -> FactStoreResult<FactCategoryV1> {
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

fn compatibility_proposal_request_value(request: &CompatibilityFactAddCommandV1) -> Value {
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

pub(super) fn compatibility_proposal_action_id(
    kind: &'static str,
    material: Value,
) -> FactStoreResult<ProvenanceId> {
    let digest = compatibility_digest(material)?;
    ProvenanceId::new(format!("compatibility-{kind}:{digest}")).map_err(FactStoreError::from)
}

#[allow(clippy::too_many_arguments)]
fn compatibility_proposal_transition_json(
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

pub(super) async fn compatibility_proposal_record_tx(
    transaction: &Transaction,
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

pub(super) async fn get_compatibility_fact_proposal_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    proposal_id: &ProvenanceId,
) -> FactCompatibilityResult<Option<CompatibilityFactProposalRecordV1>> {
    compatibility_proposal_record_tx(transaction, owner, proposal_id).await
}

pub(super) async fn list_compatibility_fact_proposals_tx(
    transaction: &Transaction,
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

pub(super) async fn count_pending_compatibility_fact_proposals_tx(
    transaction: &Transaction,
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

fn compatibility_proposal_request_digest(
    request: &CompatibilityFactAddCommandV1,
) -> FactStoreResult<String> {
    compatibility_digest(json!({
        "owner": request.owner(),
        "content": request.content(),
        "category": compatibility_category_label(request.category()),
        "source": request.source(),
        "tags": request.tags(),
        "entities": request.entities(),
        "metadata": compatibility_payload_metadata(request.metadata()),
        "automation_run_id": request.automation_run_id(),
        "default_trust": request.default_trust().as_f64(),
        "actor": request.actor().map(ActorId::as_str),
    }))
}

async fn compatibility_proposal_digest_tx(
    transaction: &Transaction,
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
    transaction: &Transaction,
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

pub(super) async fn compatibility_replay_proposal_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    receipt: &CompatibilityOperationReceiptV1,
) -> FactCompatibilityResult<CompatibilityFactProposalRecordV1> {
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
    transaction: &Transaction,
    proposal_id: &ProvenanceId,
    request: &CompatibilityFactAddCommandV1,
    idempotency_key: &ProvenanceId,
    request_digest: &str,
    evidence: &Value,
    state: CompatibilityFactProposalStateV1,
    reviewer: Option<&ActorId>,
    reason: Option<&str>,
    origin: &'static str,
    occurred_at: UtcMicros,
) -> FactStoreResult<()> {
    let key = OwnerKey::new(request.owner())?;
    let state_label = compatibility_proposal_state_label(state);
    if matches!(
        state,
        CompatibilityFactProposalStateV1::Applying | CompatibilityFactProposalStateV1::Applied
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
pub(super) async fn compatibility_advance_proposal_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    proposal_id: &ProvenanceId,
    expected_state: CompatibilityFactProposalStateV1,
    expected_revision: CompatibilityFactProposalRevisionV1,
    state: CompatibilityFactProposalStateV1,
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
    let applied = state == CompatibilityFactProposalStateV1::Applied;
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

pub(super) async fn submit_compatibility_fact_proposal_tx(
    transaction: &Transaction,
    proposal_id: ProvenanceId,
    request: &CompatibilityFactAddCommandV1,
    submitter: Option<&ActorId>,
) -> FactCompatibilityResult<CompatibilityFactProposalRecordV1> {
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
        CompatibilityFactProposalStateV1::PendingApproval,
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

pub(super) async fn reject_compatibility_fact_proposal_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    proposal_id: &ProvenanceId,
    expected_revision: CompatibilityFactProposalRevisionV1,
    reviewer: &ActorId,
    reason: &str,
) -> FactCompatibilityResult<CompatibilityFactProposalRecordV1> {
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
    if proposal.state() != CompatibilityFactProposalStateV1::PendingApproval
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
        CompatibilityFactProposalStateV1::PendingApproval,
        expected_revision,
        CompatibilityFactProposalStateV1::Rejected,
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
    transaction: &Transaction,
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
    request: &CompatibilityFactProposalImportV1,
    receipt: &Value,
) -> FactStoreResult<CompatibilityFactProposalImportReceiptV1> {
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
    CompatibilityFactProposalImportReceiptV1::new(
        request.owner().clone(),
        request.source_store_id().clone(),
        request.sidecar_digest().clone(),
        imported_count,
        quarantined_count,
    )
}

fn compatibility_import_initial_state(
    state: CompatibilityFactProposalStateV1,
) -> (CompatibilityFactProposalStateV1, Option<&'static str>) {
    match state {
        CompatibilityFactProposalStateV1::PendingApproval => {
            (CompatibilityFactProposalStateV1::PendingApproval, None)
        }
        CompatibilityFactProposalStateV1::Applying => (
            CompatibilityFactProposalStateV1::PendingApproval,
            Some("legacy applying state normalized to pending"),
        ),
        CompatibilityFactProposalStateV1::Rejected => {
            (CompatibilityFactProposalStateV1::Rejected, None)
        }
        CompatibilityFactProposalStateV1::Quarantined => (
            CompatibilityFactProposalStateV1::Quarantined,
            Some("legacy proposal was quarantined"),
        ),
        CompatibilityFactProposalStateV1::Applied => (
            CompatibilityFactProposalStateV1::Quarantined,
            Some("legacy applied proposal lacks a verifiable canonical promotion"),
        ),
    }
}

pub(super) async fn import_legacy_compatibility_fact_proposals_tx(
    transaction: &Transaction,
    request: &CompatibilityFactProposalImportV1,
) -> FactCompatibilityResult<CompatibilityFactProposalImportReceiptV1> {
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
        let resolved_id = match compatibility_legacy_proposal_mapping_tx(
            transaction,
            request.owner(),
            request.source_store_id(),
            legacy_proposal_id,
        )
        .await?
        {
            Some((existing_id, import_receipt)) => {
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
            }
            None => {
                if let Some(existing_id) = compatibility_proposal_for_digest_tx(
                    transaction,
                    request.owner(),
                    &record_digest,
                )
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
            }
        };
        let proposal = compatibility_proposal_record_tx(transaction, request.owner(), &resolved_id)
            .await?
            .ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility proposal is missing after legacy import",
                )
            })?;
        if proposal.state() == CompatibilityFactProposalStateV1::Quarantined {
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
    CompatibilityFactProposalImportReceiptV1::new(
        request.owner().clone(),
        request.source_store_id().clone(),
        request.sidecar_digest().clone(),
        imported_count,
        quarantined_count,
    )
    .map_err(Into::into)
}
