//! Canonical project-memory add, update, and remove commands.

use super::super::envelope::{
    ProjectMemoryOperationReceiptV1, project_memory_digest,
    project_memory_lookup_operation_receipt_tx, project_memory_receipt_u64,
    project_memory_record_operation_receipt_tx,
};
use super::super::primitives::{
    OwnerKey, PROJECT_MEMORY_WRITE_OPERATION, from_json, project_memory_category_label,
    project_memory_event_time, project_memory_now, row_exists, row_string, storage_error,
    storage_message,
};
use super::super::projection::load_project_memory_projection_tx;
use super::add::{ProjectMemoryAddClassification, classify_project_memory_add_tx};
use super::{
    active_fact_count_tx, commit_batch_tx, find_project_memory_fact_by_content_digest_tx,
    initial_batch, load_current_fact_tx, load_current_projection, payload_material,
    payload_metadata, sanitize_payload, verified_payload,
};
use crate::privacy::{MemoryFactSanitizationV1, sanitize_memory_fact_payload};
use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use crate::db::tombstone_fact_derivatives_tx;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    ActorId, Confidence, FactAssertionId, FactAssertionKindV1, FactAssertionV1, FactEventId,
    FactId, FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, FactPayloadV1, LocatorDigest,
    PayloadAccessState, UtcMicros,
};
use tracedecay_store::{
    FactCommitReceipt, FactStoreError, FactStoreResult, FactWriteBatch,
    ProjectMemoryFactAddCommandV1, ProjectMemoryFactAddOutcomeV1,
    ProjectMemoryFactContentDigestQueryV1, ProjectMemoryFactFeedbackActionV1,
    ProjectMemoryFactIdV1, ProjectMemoryFactProjectionV1, ProjectMemoryFactRemoveCommandV1,
    ProjectMemoryFactRemoveOutcomeV1, ProjectMemoryFactUpdateCommandV1,
    ProjectMemoryFactUpdateOutcomeV1, StoredFactV1,
};

pub(super) fn project_memory_feedback_action_label(
    action: ProjectMemoryFactFeedbackActionV1,
) -> &'static str {
    match action {
        ProjectMemoryFactFeedbackActionV1::Helpful => "helpful",
        ProjectMemoryFactFeedbackActionV1::Unhelpful => "unhelpful",
    }
}

pub(super) fn project_memory_feedback_delta(action: ProjectMemoryFactFeedbackActionV1) -> f64 {
    match action {
        ProjectMemoryFactFeedbackActionV1::Helpful => 0.05,
        ProjectMemoryFactFeedbackActionV1::Unhelpful => -0.10,
    }
}

pub(super) async fn project_memory_update_feedback_projection_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
    action: ProjectMemoryFactFeedbackActionV1,
    timestamp: UtcMicros,
) -> FactStoreResult<()> {
    let key = OwnerKey::new(owner)?;
    let changed = transaction
        .execute(
            "UPDATE memory_v2_current_facts SET
                helpful_count = helpful_count + ?1,
                unhelpful_count = unhelpful_count + ?2,
                last_feedback_at = ?3
             WHERE fact_id = ?4 AND owner_kind = ?5 AND project_id = ?6",
            params![
                i64::from(matches!(action, ProjectMemoryFactFeedbackActionV1::Helpful)),
                i64::from(matches!(
                    action,
                    ProjectMemoryFactFeedbackActionV1::Unhelpful
                )),
                timestamp.0,
                fact_id.as_str(),
                key.kind,
                key.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    if changed != 1 {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "feedback target has no current canonical projection",
        ));
    }
    Ok(())
}

fn project_memory_correction_batch(
    fact: &StoredFactV1,
    payload: FactPayloadV1,
    access: PayloadAccessState,
    trust: Confidence,
    expected_last_event_id: Option<FactEventId>,
    actor: Option<ActorId>,
    now: UtcMicros,
) -> FactStoreResult<FactWriteBatch> {
    let assertion = FactAssertionV1::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        FactAssertionKindV1::Correction {
            supersedes: fact.active_assertion_id().clone(),
        },
        payload,
        Vec::new(),
        now,
        actor.clone(),
    )?;
    let mut events = vec![FactLineageEventV1::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        FactLineageEventKindV1::AssertionRecorded {
            assertion_id: assertion.assertion_id().clone(),
        },
        now,
        actor.clone(),
    )?];
    let mut offset = 1;
    if access != fact.payload_access() {
        events.push(FactLineageEventV1::new(
            fact.fact_id().clone(),
            fact.owner().clone(),
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: fact.payload_access(),
                current: access,
            },
            project_memory_event_time(now, offset)?,
            actor.clone(),
        )?);
        offset += 1;
    }
    if trust != fact.trust() {
        events.push(FactLineageEventV1::new(
            fact.fact_id().clone(),
            fact.owner().clone(),
            FactLineageEventKindV1::TrustChanged {
                previous: fact.trust(),
                current: trust,
                evidence_ids: Vec::new(),
            },
            project_memory_event_time(now, offset)?,
            actor,
        )?);
    }
    FactWriteBatch::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        Some(assertion),
        events,
        Vec::new(),
        Vec::new(),
        expected_last_event_id,
    )
}

fn project_memory_removal_batch(
    owner: &FactOwnerV1,
    fact_id: &FactId,
    previous: PayloadAccessState,
    expected_last_event_id: FactEventId,
    actor: Option<ActorId>,
    now: UtcMicros,
) -> FactStoreResult<FactWriteBatch> {
    let event = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous,
            current: PayloadAccessState::Deleted,
        },
        now,
        actor,
    )?;
    FactWriteBatch::new(
        fact_id.clone(),
        owner.clone(),
        None,
        vec![event],
        Vec::new(),
        Vec::new(),
        Some(expected_last_event_id),
    )
}

pub(super) fn commit_receipt_json(outcome: &'static str, receipt: &FactCommitReceipt) -> Value {
    json!({
        "outcome": outcome,
        "committed_event_ids": receipt
            .committed_event_ids()
            .iter()
            .map(FactEventId::as_str)
            .collect::<Vec<_>>(),
        "active_assertion_id": receipt
            .active_assertion_id()
            .map(FactAssertionId::as_str),
    })
}

pub(super) async fn project_memory_commit_receipt_from_operation_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    receipt: &ProjectMemoryOperationReceiptV1,
) -> FactStoreResult<FactCommitReceipt> {
    let fact_id = receipt
        .fact_id
        .clone()
        .ok_or(FactStoreError::InvalidCommitReceipt)?;
    let last_event_id = receipt
        .event_id
        .clone()
        .ok_or(FactStoreError::InvalidCommitReceipt)?;
    let committed_event_ids = receipt
        .receipt
        .get("committed_event_ids")
        .and_then(Value::as_array)
        .ok_or(FactStoreError::InvalidCommitReceipt)?
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or(FactStoreError::InvalidCommitReceipt)
                .and_then(|value| FactEventId::new(value.to_owned()).map_err(FactStoreError::from))
        })
        .collect::<FactStoreResult<Vec<_>>>()?;
    let active_assertion_id = match receipt.receipt.get("active_assertion_id") {
        Some(Value::Null) | None => None,
        Some(Value::String(value)) => {
            Some(FactAssertionId::new(value.clone()).map_err(FactStoreError::from)?)
        }
        _ => return Err(FactStoreError::InvalidCommitReceipt),
    };
    let canonical = FactCommitReceipt::new(
        fact_id,
        owner.clone(),
        committed_event_ids,
        last_event_id,
        active_assertion_id,
    )?;
    let key = OwnerKey::new(owner)?;
    for event_id in canonical.committed_event_ids() {
        if !row_exists(
            transaction,
            "SELECT 1 FROM memory_v2_lineage_events
             WHERE event_id = ?1 AND fact_id = ?2 AND owner_kind = ?3 AND project_id = ?4",
            params![
                event_id.as_str(),
                canonical.fact_id().as_str(),
                key.kind,
                key.project_id.as_str(),
            ],
        )
        .await?
        {
            return Err(FactStoreError::InvalidCommitReceipt);
        }
    }
    if let Some(assertion_id) = canonical.active_assertion_id()
        && !row_exists(
            transaction,
            "SELECT 1 FROM memory_v2_assertions
             WHERE assertion_id = ?1 AND fact_id = ?2 AND owner_kind = ?3
               AND project_id = ?4 AND owner_json = ?5",
            params![
                assertion_id.as_str(),
                canonical.fact_id().as_str(),
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
            ],
        )
        .await?
    {
        return Err(FactStoreError::InvalidCommitReceipt);
    }
    Ok(canonical)
}

pub(in crate::store::memory) async fn load_mutable_project_memory_fact_tx(
    transaction: &Transaction<'_>,
    target: &ProjectMemoryFactIdV1,
) -> FactStoreResult<StoredFactV1> {
    let fact_id = target.fact_id().clone();
    match load_project_memory_projection_tx(transaction, target.owner(), &fact_id).await? {
        None => Err(FactStoreError::FactNotFound { fact_id }),
        Some(ProjectMemoryFactProjectionV1::Unavailable(unavailable)) => {
            match unavailable.payload_access() {
                PayloadAccessState::Deleted => Err(FactStoreError::FactDeleted { fact_id }),
                PayloadAccessState::Eligible => Err(FactStoreError::PayloadAccessMismatch),
                PayloadAccessState::Redacted
                | PayloadAccessState::Quarantined
                | PayloadAccessState::RetentionExpired
                | PayloadAccessState::Unavailable
                | PayloadAccessState::Ambiguous => Err(FactStoreError::FactUnavailable { fact_id }),
            }
        }
        Some(ProjectMemoryFactProjectionV1::Available(_)) => {
            let owner_key = OwnerKey::new(target.owner())?;
            load_current_fact_tx(transaction, &owner_key, target.owner(), &fact_id)
                .await?
                .ok_or(FactStoreError::FactUnavailable { fact_id })
        }
    }
}

fn content_digest(content: &str) -> FactStoreResult<LocatorDigest> {
    LocatorDigest::new(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(content.as_bytes()))
    ))
    .map_err(FactStoreError::from)
}

async fn project_memory_replay_add_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    receipt: &ProjectMemoryOperationReceiptV1,
) -> FactStoreResult<ProjectMemoryFactAddOutcomeV1> {
    let outcome_kind = receipt
        .receipt
        .get("outcome")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            storage_message(PROJECT_MEMORY_WRITE_OPERATION, "add receipt is malformed")
        })?;
    let fact_id = receipt
        .fact_id
        .as_ref()
        .ok_or(FactStoreError::InvalidCommitReceipt)?;
    let fact = load_project_memory_projection_tx(transaction, owner, fact_id)
        .await?
        .ok_or(FactStoreError::InvalidCommitReceipt)?;
    match outcome_kind {
        "added" => ProjectMemoryFactAddOutcomeV1::added(
            fact,
            project_memory_commit_receipt_from_operation_tx(transaction, owner, receipt).await?,
            true,
        ),
        "normalized_duplicate" => ProjectMemoryFactAddOutcomeV1::normalized_duplicate(
            fact,
            ProjectMemoryFactIdV1::new(owner.clone(), fact_id.clone())?,
        ),
        "semantic_near_duplicate" | "possible_conflict" => {
            let closest_id = receipt
                .receipt
                .get("closest_fact_id")
                .and_then(Value::as_str)
                .ok_or(FactStoreError::InvalidCommitReceipt)
                .and_then(|value| FactId::new(value.to_owned()).map_err(FactStoreError::from))?;
            let closest = ProjectMemoryFactIdV1::new(owner.clone(), closest_id)?;
            let similarity = project_memory_receipt_u64(&receipt.receipt, "similarity_millionths")
                .and_then(|value| {
                    u32::try_from(value).map_err(|_| FactStoreError::InvalidCommitReceipt)
                })?;
            let commit_receipt =
                project_memory_commit_receipt_from_operation_tx(transaction, owner, receipt)
                    .await?;
            if outcome_kind == "semantic_near_duplicate" {
                return ProjectMemoryFactAddOutcomeV1::semantic_near_duplicate(
                    fact,
                    closest,
                    similarity,
                    commit_receipt,
                    true,
                );
            }
            ProjectMemoryFactAddOutcomeV1::possible_conflict(
                fact,
                closest,
                similarity,
                commit_receipt,
                true,
            )
        }
        _ => Err(FactStoreError::InvalidCommitReceipt),
    }
}

pub(in crate::store::memory) async fn add_project_memory_fact_tx(
    transaction: &Transaction<'_>,
    request: &ProjectMemoryFactAddCommandV1,
) -> FactStoreResult<ProjectMemoryFactAddOutcomeV1> {
    let payload_metadata = payload_metadata(request.metadata());
    let request_digest = request.input_digest().to_owned();
    if let Some(receipt) = project_memory_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "add",
        &request_digest,
    )
    .await?
    {
        return project_memory_replay_add_tx(transaction, request.owner(), &receipt).await;
    }

    let now = project_memory_now()?;
    let sanitized = verified_payload(
        request.content(),
        request.category(),
        request.tags(),
        request.entities(),
        &payload_metadata,
        request.source_label(),
        request.sanitization_receipt().clone(),
    )?;
    let duplicate_query = ProjectMemoryFactContentDigestQueryV1::new(
        request.owner().clone(),
        content_digest(sanitized.payload.content())?,
    )?;
    if let Some(fact) =
        find_project_memory_fact_by_content_digest_tx(transaction, &duplicate_query).await?
    {
        let closest = ProjectMemoryFactIdV1::new(request.owner().clone(), fact.fact_id().clone())?;
        project_memory_record_operation_receipt_tx(
            transaction,
            request.owner(),
            request.operation_id(),
            "add",
            &request_digest,
            Some(fact.fact_id()),
            None,
            &json!({
                "outcome": "normalized_duplicate",
            }),
            now,
        )
        .await?;
        return ProjectMemoryFactAddOutcomeV1::normalized_duplicate(fact, closest);
    }

    let proposed_content = sanitized.payload.content().to_owned();
    let proposed_entities = sanitized.payload.entities().to_vec();
    let batch = initial_batch(
        request.owner(),
        request.operation_id(),
        sanitized.payload,
        sanitized.access,
        request.default_trust(),
        request.actor().cloned(),
        now,
    )?;
    let classification = classify_project_memory_add_tx(
        transaction,
        request.owner(),
        batch.fact_id(),
        &proposed_content,
        &proposed_entities,
    )
    .await?;
    if let Some(ProjectMemoryAddClassification::NormalizedDuplicate(closest)) = &classification {
        let closest_id =
            ProjectMemoryFactIdV1::new(request.owner().clone(), closest.fact_id().clone())?;
        project_memory_record_operation_receipt_tx(
            transaction,
            request.owner(),
            request.operation_id(),
            "add",
            &request_digest,
            Some(closest.fact_id()),
            None,
            &json!({
                "outcome": "normalized_duplicate",
            }),
            now,
        )
        .await?;
        return ProjectMemoryFactAddOutcomeV1::normalized_duplicate(
            ProjectMemoryFactProjectionV1::Available(closest.clone()),
            closest_id,
        );
    }
    let comparison = match classification {
        Some(ProjectMemoryAddClassification::SemanticNearDuplicate {
            closest_fact_id,
            similarity_millionths,
        }) => Some((closest_fact_id, similarity_millionths, false)),
        Some(ProjectMemoryAddClassification::PossibleConflict {
            closest_fact_id,
            similarity_millionths,
        }) => Some((closest_fact_id, similarity_millionths, true)),
        Some(ProjectMemoryAddClassification::NormalizedDuplicate(_)) | None => None,
    };

    let (canonical_receipt, replayed) = commit_batch_tx(transaction, &batch).await?;
    let fact = load_project_memory_projection_tx(
        transaction,
        request.owner(),
        canonical_receipt.fact_id(),
    )
    .await?
    .ok_or_else(|| storage_message(PROJECT_MEMORY_WRITE_OPERATION, "added fact is missing"))?;
    let outcome_kind = match comparison {
        Some((_, _, true)) => "possible_conflict",
        Some((_, _, false)) => "semantic_near_duplicate",
        None => "added",
    };
    let mut receipt_json = commit_receipt_json(outcome_kind, &canonical_receipt);
    if let Some((closest_fact_id, similarity_millionths, _)) = &comparison {
        receipt_json["closest_fact_id"] = json!(closest_fact_id.as_str());
        receipt_json["similarity_millionths"] = json!(similarity_millionths);
    }
    project_memory_record_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "add",
        &request_digest,
        Some(canonical_receipt.fact_id()),
        Some(canonical_receipt.last_event_id()),
        &receipt_json,
        now,
    )
    .await?;
    if let Some((closest_fact_id, similarity_millionths, conflict)) = comparison {
        let closest = ProjectMemoryFactIdV1::new(request.owner().clone(), closest_fact_id)?;
        if conflict {
            return ProjectMemoryFactAddOutcomeV1::possible_conflict(
                fact,
                closest,
                similarity_millionths,
                canonical_receipt,
                replayed,
            );
        }
        return ProjectMemoryFactAddOutcomeV1::semantic_near_duplicate(
            fact,
            closest,
            similarity_millionths,
            canonical_receipt,
            replayed,
        );
    }
    ProjectMemoryFactAddOutcomeV1::added(fact, canonical_receipt, replayed)
}

async fn project_memory_replay_update_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    receipt: &ProjectMemoryOperationReceiptV1,
) -> FactStoreResult<ProjectMemoryFactUpdateOutcomeV1> {
    let fact_id = receipt.fact_id.as_ref().ok_or_else(|| {
        storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "update receipt fact is missing",
        )
    })?;
    let fact = load_project_memory_projection_tx(transaction, owner, fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "replayed update fact is missing",
            )
        })?;
    let trust_delta_millionths = receipt
        .receipt
        .get("trust_delta_millionths")
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "update receipt is malformed",
            )
        })?;
    ProjectMemoryFactUpdateOutcomeV1::committed(
        fact,
        trust_delta_millionths,
        project_memory_commit_receipt_from_operation_tx(transaction, owner, receipt).await?,
        true,
    )
}

pub(in crate::store::memory) async fn update_project_memory_fact_tx(
    transaction: &Transaction<'_>,
    request: &ProjectMemoryFactUpdateCommandV1,
) -> FactStoreResult<ProjectMemoryFactUpdateOutcomeV1> {
    let request_digest = project_memory_digest(json!({
        "fact_id": request.target().fact_id().as_str(),
        "expected_last_event_id": request.expected_last_event_id().map(FactEventId::as_str),
        "content": request.patch().content(),
        "category": request.patch().category().map(project_memory_category_label),
        "source_label": match request.patch().source_label() {
            None => json!({"changed": false}),
            Some(value) => json!({"changed": true, "value": value}),
        },
        "tags": request.patch().tags(),
        "entities": request.patch().entities(),
        "metadata": request.patch().metadata(),
        "trust": request.patch().trust().map(Confidence::as_f64),
        "actor": request.actor().map(ActorId::as_str),
    }))?;
    if let Some(receipt) = project_memory_lookup_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "update",
        &request_digest,
    )
    .await?
    {
        return project_memory_replay_update_tx(transaction, request.target().owner(), &receipt)
            .await;
    }

    let fact_id = request.target().fact_id().clone();
    let current = load_mutable_project_memory_fact_tx(transaction, request.target()).await?;
    let previous_payload = current
        .payload()
        .ok_or_else(|| FactStoreError::FactUnavailable {
            fact_id: fact_id.clone(),
        })?;
    let content = request
        .patch()
        .content()
        .unwrap_or(previous_payload.content());
    let category = request
        .patch()
        .category()
        .unwrap_or(previous_payload.category());
    let tags = request.patch().tags().unwrap_or(previous_payload.tags());
    let entities = request
        .patch()
        .entities()
        .unwrap_or(previous_payload.entities());
    let metadata = request
        .patch()
        .metadata()
        .unwrap_or(previous_payload.metadata());
    let source_label = request
        .patch()
        .source_label()
        .unwrap_or_else(|| previous_payload.source_label());
    let Some(sanitized) =
        sanitize_payload(content, category, tags, entities, metadata, source_label)?
    else {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "update payload was rejected by the privacy sanitizer",
        ));
    };
    let new_trust = request.patch().trust().unwrap_or(current.trust());
    let now = project_memory_now()?;
    let batch = project_memory_correction_batch(
        &current,
        sanitized.payload,
        sanitized.access,
        new_trust,
        request
            .expected_last_event_id()
            .cloned()
            .or_else(|| Some(current.last_event_id().clone())),
        request.actor().cloned(),
        now,
    )?;
    let (canonical_receipt, replayed) = commit_batch_tx(transaction, &batch).await?;
    purge_detector_flagged_superseded_payloads_tx(
        transaction,
        &OwnerKey::new(request.target().owner())?,
        &fact_id,
        canonical_receipt.active_assertion_id(),
    )
    .await?;
    let fact = load_project_memory_projection_tx(transaction, request.target().owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(PROJECT_MEMORY_WRITE_OPERATION, "updated fact is missing")
        })?;
    let trust_delta_millionths =
        ((new_trust.as_f64() - current.trust().as_f64()) * 1_000_000.0).round() as i32;
    let mut receipt_json = commit_receipt_json("updated", &canonical_receipt);
    receipt_json["trust_delta_millionths"] = json!(trust_delta_millionths);
    project_memory_record_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "update",
        &request_digest,
        Some(&fact_id),
        Some(canonical_receipt.last_event_id()),
        &receipt_json,
        now,
    )
    .await?;
    ProjectMemoryFactUpdateOutcomeV1::committed(
        fact,
        trust_delta_millionths,
        canonical_receipt,
        replayed,
    )
}

/// Erases superseded assertion payload rows the current detector would refuse
/// or rewrite, in the same transaction as the correction that supersedes them.
///
/// At-rest privacy remediation settles redactions through the ordinary update
/// path, so a projection-only correction would leave the original plaintext
/// readable in `memory_v2_assertion_payloads` (and its FTS backing) even
/// though the served fact is sanitized. Re-evaluating each superseded row
/// under the one canonical sanitizer keeps clean history available to as-of
/// reads while flagged rows are securely deleted; the FTS delete trigger
/// removes the index copy in the same statement.
async fn purge_detector_flagged_superseded_payloads_tx(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    fact_id: &FactId,
    active_assertion_id: Option<&FactAssertionId>,
) -> FactStoreResult<()> {
    let mut rows = transaction
        .query(
            "SELECT assertion_id, payload_json FROM memory_v2_assertion_payloads
             WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
               AND assertion_id != ?4",
            params![
                fact_id.as_str(),
                owner.kind,
                owner.project_id.as_str(),
                active_assertion_id.map_or("", FactAssertionId::as_str),
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let mut flagged = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
    {
        let assertion_id = row_string(&row, 0, PROJECT_MEMORY_WRITE_OPERATION)?;
        let payload = from_json::<FactPayloadV1>(
            &row_string(&row, 1, PROJECT_MEMORY_WRITE_OPERATION)?,
            PROJECT_MEMORY_WRITE_OPERATION,
        )?;
        let metadata = payload_metadata(payload.metadata());
        let wire = payload_material(
            payload.content(),
            payload.category(),
            payload.tags(),
            payload.entities(),
            &metadata,
            payload.source_label(),
        );
        let sanitized = sanitize_memory_fact_payload(wire.clone())
            .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
        let clean = matches!(
            sanitized,
            MemoryFactSanitizationV1::Durable { payload, .. } if payload == wire
        );
        if !clean {
            flagged.push(assertion_id);
        }
    }
    drop(rows);
    if flagged.is_empty() {
        return Ok(());
    }
    transaction
        .execute_batch("PRAGMA secure_delete = ON;")
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    for assertion_id in flagged {
        transaction
            .execute(
                "DELETE FROM memory_v2_assertion_payloads
                 WHERE assertion_id = ?1 AND fact_id = ?2
                   AND owner_kind = ?3 AND project_id = ?4",
                params![
                    assertion_id,
                    fact_id.as_str(),
                    owner.kind,
                    owner.project_id.as_str(),
                ],
            )
            .await
            .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    }
    Ok(())
}

async fn project_memory_replay_remove_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    receipt: &ProjectMemoryOperationReceiptV1,
) -> FactStoreResult<ProjectMemoryFactRemoveOutcomeV1> {
    let outcome = receipt
        .receipt
        .get("outcome")
        .and_then(Value::as_str)
        .ok_or(FactStoreError::InvalidCommitReceipt)?;
    let remaining_fact_count =
        project_memory_receipt_u64(&receipt.receipt, "remaining_fact_count")?;
    match outcome {
        "not_found" => Ok(ProjectMemoryFactRemoveOutcomeV1::not_found(
            remaining_fact_count,
        )),
        "already_removed" | "removed" => {
            let fact_id = receipt
                .fact_id
                .as_ref()
                .ok_or(FactStoreError::InvalidCommitReceipt)?;
            let fact = load_project_memory_projection_tx(transaction, owner, fact_id)
                .await?
                .ok_or(FactStoreError::InvalidCommitReceipt)?;
            if outcome == "already_removed" {
                return ProjectMemoryFactRemoveOutcomeV1::already_removed(
                    fact,
                    remaining_fact_count,
                );
            }
            ProjectMemoryFactRemoveOutcomeV1::removed(
                fact,
                remaining_fact_count,
                project_memory_commit_receipt_from_operation_tx(transaction, owner, receipt)
                    .await?,
                true,
            )
        }
        _ => Err(FactStoreError::InvalidCommitReceipt),
    }
}

pub(in crate::store::memory) async fn remove_project_memory_fact_tx(
    transaction: &Transaction<'_>,
    request: &ProjectMemoryFactRemoveCommandV1,
) -> FactStoreResult<ProjectMemoryFactRemoveOutcomeV1> {
    let request_digest = project_memory_digest(json!({
        "fact_id": request.target().fact_id().as_str(),
        "expected_last_event_id": request.expected_last_event_id().map(FactEventId::as_str),
        "actor": request.actor().map(ActorId::as_str),
    }))?;
    if let Some(receipt) = project_memory_lookup_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "remove",
        &request_digest,
    )
    .await?
    {
        return project_memory_replay_remove_tx(transaction, request.target().owner(), &receipt)
            .await;
    }

    let now = project_memory_now()?;
    let fact_id = request.target().fact_id().clone();
    let owner_key = OwnerKey::new(request.target().owner())?;
    let Some(current) = load_current_projection(transaction, &owner_key, &fact_id).await? else {
        let remaining_fact_count =
            active_fact_count_tx(transaction, request.target().owner()).await?;
        project_memory_record_operation_receipt_tx(
            transaction,
            request.target().owner(),
            request.operation_id(),
            "remove",
            &request_digest,
            None,
            None,
            &json!({
                "outcome": "not_found",
                "remaining_fact_count": remaining_fact_count,
            }),
            now,
        )
        .await?;
        return Ok(ProjectMemoryFactRemoveOutcomeV1::not_found(
            remaining_fact_count,
        ));
    };
    if current.access == PayloadAccessState::Deleted {
        let fact =
            load_project_memory_projection_tx(transaction, request.target().owner(), &fact_id)
                .await?
                .ok_or_else(|| {
                    storage_message(PROJECT_MEMORY_WRITE_OPERATION, "deleted fact is missing")
                })?;
        let remaining_fact_count =
            active_fact_count_tx(transaction, request.target().owner()).await?;
        project_memory_record_operation_receipt_tx(
            transaction,
            request.target().owner(),
            request.operation_id(),
            "remove",
            &request_digest,
            Some(&fact_id),
            None,
            &json!({
                "outcome": "already_removed",
                "remaining_fact_count": remaining_fact_count,
            }),
            now,
        )
        .await?;
        return ProjectMemoryFactRemoveOutcomeV1::already_removed(fact, remaining_fact_count);
    }
    let expected_last_event_id = request
        .expected_last_event_id()
        .cloned()
        .or(current.last_event_id.clone())
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "remove target has no lineage CAS identity",
            )
        })?;
    let batch = project_memory_removal_batch(
        request.target().owner(),
        &fact_id,
        current.access,
        expected_last_event_id,
        request.actor().cloned(),
        now,
    )?;
    let (canonical_receipt, replayed) = commit_batch_tx(transaction, &batch).await?;
    tombstone_fact_derivatives_tx(
        transaction,
        request.target().owner(),
        fact_id.as_str(),
        canonical_receipt.last_event_id().as_str(),
        now,
    )
    .await
    .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let fact = load_project_memory_projection_tx(transaction, request.target().owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(PROJECT_MEMORY_WRITE_OPERATION, "removed fact is missing")
        })?;
    let remaining_fact_count = active_fact_count_tx(transaction, request.target().owner()).await?;
    let mut receipt_json = commit_receipt_json("removed", &canonical_receipt);
    receipt_json["remaining_fact_count"] = json!(remaining_fact_count);
    project_memory_record_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "remove",
        &request_digest,
        Some(&fact_id),
        Some(canonical_receipt.last_event_id()),
        &receipt_json,
        now,
    )
    .await?;
    ProjectMemoryFactRemoveOutcomeV1::removed(
        fact,
        remaining_fact_count,
        canonical_receipt,
        replayed,
    )
}
