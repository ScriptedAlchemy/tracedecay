//! Compatibility fact add/update/remove commands and their replay batches.

use super::super::envelope::{
    OperationReceipt, digest, lookup_operation_receipt_tx, record_operation_receipt_tx,
    target_digest,
};
use super::super::primitives::{
    FACT_WRITE_OPERATION, OwnerKey, category_label, event_time, fact_source_label, now,
    storage_error, storage_message,
};
use super::super::projection::{
    load_projection_tx, required_mapping_tx, resolve_target_tx, source_for_fact_tx,
};
use super::{
    MirrorInsert, active_fact_count_tx, commit_batch_tx, initial_batch, last_insert_rowid_tx,
    legacy_mapping_for_new_fact, load_current_fact_tx, load_current_projection,
    mark_owner_banks_dirty_tx, mirror_delete_tx, mirror_insert_tx, mirror_update_tx,
    payload_metadata, sanitize_payload,
};
use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use crate::db::{Database, tombstone_fact_derivatives_tx};
use serde_json::{Value, json};
use tracedecay_domain::{
    ActorId, Confidence, FactAssertionKindV1, FactAssertionV1, FactEventId, FactId,
    FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, FactPayloadV1,
    LegacyHistoryCoverageV1, PayloadAccessState, UtcMicros,
};
use tracedecay_store::{
    FactAddCommand, FactAddDisposition, FactAddOutcome, FactFeedbackAction, FactLineageError,
    FactLineageResult, FactRemoveCommand, FactRemoveOutcome, FactStoreResult, FactUpdateCommand,
    FactUpdateOutcome, FactWriteBatch, OwnedFactId, StoredFactV1,
};
pub(super) fn feedback_action_label(action: FactFeedbackAction) -> &'static str {
    match action {
        FactFeedbackAction::Helpful => "helpful",
        FactFeedbackAction::Unhelpful => "unhelpful",
    }
}

pub(super) fn feedback_delta(action: FactFeedbackAction) -> f64 {
    match action {
        FactFeedbackAction::Helpful => 0.05,
        FactFeedbackAction::Unhelpful => -0.10,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn mirror_feedback_tx(
    transaction: &Transaction<'_>,
    legacy_fact_id: i64,
    action: FactFeedbackAction,
    old_trust: Confidence,
    new_trust: Confidence,
    timestamp: i64,
    source: &str,
    note: Option<&str>,
) -> FactLineageResult<i64> {
    let changed = transaction
        .execute(
            "UPDATE memory_facts SET
                trust_score = ?1,
                helpful_count = helpful_count + ?2,
                unhelpful_count = unhelpful_count + ?3,
                last_feedback_at = ?4,
                updated_at = ?4
             WHERE fact_id = ?5",
            params![
                new_trust.as_f64(),
                i64::from(matches!(action, FactFeedbackAction::Helpful)),
                i64::from(matches!(action, FactFeedbackAction::Unhelpful)),
                timestamp,
                legacy_fact_id,
            ],
        )
        .await
        .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?;
    if changed != 1 {
        return Err(storage_message(
            FACT_WRITE_OPERATION,
            "compatibility feedback target is missing from the legacy mirror",
        ));
    }
    transaction
        .execute(
            "INSERT INTO memory_feedback_events (
                fact_id, action, trust_delta, old_trust, new_trust,
                created_at, source, note
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                legacy_fact_id,
                feedback_action_label(action),
                new_trust.as_f64() - old_trust.as_f64(),
                old_trust.as_f64(),
                new_trust.as_f64(),
                timestamp,
                source,
                note,
            ],
        )
        .await
        .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?;
    last_insert_rowid_tx(transaction).await
}

pub(super) async fn update_feedback_projection_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
    action: FactFeedbackAction,
    timestamp: UtcMicros,
) -> FactLineageResult<()> {
    let key = OwnerKey::new(owner)?;
    let changed = transaction
        .execute(
            "UPDATE memory_v2_current_facts SET
                helpful_count = helpful_count + ?1,
                unhelpful_count = unhelpful_count + ?2,
                last_feedback_at = ?3
             WHERE fact_id = ?4 AND owner_kind = ?5 AND project_id = ?6",
            params![
                i64::from(matches!(action, FactFeedbackAction::Helpful)),
                i64::from(matches!(action, FactFeedbackAction::Unhelpful)),
                timestamp.0,
                fact_id.as_str(),
                key.kind,
                key.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?;
    if changed != 1 {
        return Err(storage_message(
            FACT_WRITE_OPERATION,
            "compatibility feedback target has no current projection",
        ));
    }
    Ok(())
}

fn correction_batch(
    fact: &StoredFactV1,
    payload: FactPayloadV1,
    access: PayloadAccessState,
    trust: Confidence,
    expected_last_event_id: Option<FactEventId>,
    actor: Option<ActorId>,
    now: UtcMicros,
) -> FactLineageResult<FactWriteBatch> {
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
            event_time(now, offset)?,
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
            event_time(now, offset)?,
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
        None,
        expected_last_event_id,
    )
}

fn removal_batch(
    owner: &FactOwnerV1,
    fact_id: &FactId,
    previous: PayloadAccessState,
    expected_last_event_id: Option<FactEventId>,
    actor: Option<ActorId>,
    now: UtcMicros,
) -> FactLineageResult<FactWriteBatch> {
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
        None,
        expected_last_event_id,
    )
}

async fn replay_add_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    receipt: &OperationReceipt,
) -> FactStoreResult<FactAddOutcome> {
    let outcome = receipt
        .receipt
        .get("outcome")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            storage_message(
                FACT_WRITE_OPERATION,
                "compatibility add receipt is malformed",
            )
        })?;
    match outcome {
        "rejected_secret_like" => FactAddOutcome::new(
            None,
            FactAddDisposition::RejectedSecretLike,
            None,
            None,
            receipt
                .receipt
                .get("reason")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        )
        .map_err(Into::into),
        "added" | "near_duplicate" => {
            let fact_id = receipt.fact_id.as_ref().ok_or_else(|| {
                storage_message(
                    FACT_WRITE_OPERATION,
                    "compatibility add receipt fact is missing",
                )
            })?;
            let fact = load_projection_tx(transaction, owner, fact_id)
                .await?
                .ok_or_else(|| {
                    storage_message(FACT_WRITE_OPERATION, "compatibility replay fact is missing")
                })?;
            let closest = if outcome == "near_duplicate" {
                Some(OwnedFactId::new(owner.clone(), fact_id.clone())?)
            } else {
                None
            };
            FactAddOutcome::new(
                Some(fact),
                if outcome == "added" {
                    FactAddDisposition::Added
                } else {
                    FactAddDisposition::NearDuplicate
                },
                closest,
                None,
                None,
            )
            .map_err(Into::into)
        }
        _ => Err(storage_message(
            FACT_WRITE_OPERATION,
            "unknown compatibility add receipt outcome",
        )
        .into()),
    }
}

pub(in crate::store::memory) async fn add_fact_tx(
    db: &Database,
    transaction: &Transaction<'_>,
    request: &FactAddCommand,
) -> FactStoreResult<FactAddOutcome> {
    let payload_metadata = payload_metadata(request.metadata());
    let request_digest = digest(json!({
        "owner": request.owner(),
        "content": request.content(),
        "category": category_label(request.category()),
        "source": request.source(),
        "tags": request.tags(),
        "entities": request.entities(),
        "metadata": &payload_metadata,
        "automation_run_id": request.automation_run_id(),
        "default_trust": request.default_trust().as_f64(),
        "actor": request.actor().map(ActorId::as_str),
    }))?;
    if let Some(receipt) = lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "add",
        &request_digest,
    )
    .await?
    {
        return replay_add_tx(transaction, request.owner(), &receipt).await;
    }
    let now = now()?;
    let Some(sanitized) = sanitize_payload(
        request.content(),
        request.category(),
        request.tags(),
        request.entities(),
        &payload_metadata,
    )?
    else {
        let receipt = json!({
            "outcome": "rejected_secret_like",
            "reason": "content rejected by privacy sanitizer",
        });
        record_operation_receipt_tx(
            transaction,
            request.owner(),
            request.operation_id(),
            "add",
            &request_digest,
            None,
            None,
            &receipt,
            now,
        )
        .await?;
        return FactAddOutcome::new(
            None,
            FactAddDisposition::RejectedSecretLike,
            None,
            None,
            Some("content rejected by privacy sanitizer".to_owned()),
        )
        .map_err(Into::into);
    };
    let source = fact_source_label(request.source())?;
    match mirror_insert_tx(
        db,
        transaction,
        request.owner(),
        &sanitized.payload,
        &source,
        request.default_trust(),
        now,
    )
    .await?
    {
        MirrorInsert::Existing { fact_id, .. } => {
            let fact = load_projection_tx(transaction, request.owner(), &fact_id)
                .await?
                .ok_or_else(|| {
                    storage_message(
                        FACT_WRITE_OPERATION,
                        "duplicate compatibility fact projection is missing",
                    )
                })?;
            let closest = OwnedFactId::new(request.owner().clone(), fact_id.clone())?;
            let receipt = json!({ "outcome": "near_duplicate" });
            record_operation_receipt_tx(
                transaction,
                request.owner(),
                request.operation_id(),
                "add",
                &request_digest,
                Some(&fact_id),
                None,
                &receipt,
                now,
            )
            .await?;
            FactAddOutcome::new(
                Some(fact),
                FactAddDisposition::NearDuplicate,
                Some(closest),
                None,
                None,
            )
            .map_err(Into::into)
        }
        MirrorInsert::Inserted(legacy_fact_id) => {
            let (identity, mapping) =
                legacy_mapping_for_new_fact(request.owner(), legacy_fact_id, now)?;
            let batch = initial_batch(
                request.owner(),
                identity,
                mapping.clone(),
                sanitized.payload,
                sanitized.access,
                request.default_trust(),
                request.actor().cloned(),
                now,
            )?;
            let (canonical_receipt, _) = commit_batch_tx(transaction, &batch).await?;
            let fact = load_projection_tx(transaction, request.owner(), mapping.fact_id())
                .await?
                .ok_or_else(|| {
                    storage_message(
                        FACT_WRITE_OPERATION,
                        "added compatibility fact projection is missing",
                    )
                })?;
            let receipt = json!({ "outcome": "added" });
            record_operation_receipt_tx(
                transaction,
                request.owner(),
                request.operation_id(),
                "add",
                &request_digest,
                Some(mapping.fact_id()),
                Some(canonical_receipt.last_event_id()),
                &receipt,
                now,
            )
            .await?;
            FactAddOutcome::new(Some(fact), FactAddDisposition::Added, None, None, None)
                .map_err(Into::into)
        }
    }
}

async fn replay_update_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    receipt: &OperationReceipt,
) -> FactStoreResult<FactUpdateOutcome> {
    let fact_id = receipt.fact_id.as_ref().ok_or_else(|| {
        storage_message(
            FACT_WRITE_OPERATION,
            "compatibility update receipt fact is missing",
        )
    })?;
    let fact = load_projection_tx(transaction, owner, fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                FACT_WRITE_OPERATION,
                "compatibility update replay fact is missing",
            )
        })?;
    let trust_delta_millionths = receipt
        .receipt
        .get("trust_delta_millionths")
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| {
            storage_message(
                FACT_WRITE_OPERATION,
                "compatibility update receipt is malformed",
            )
        })?;
    FactUpdateOutcome::new(fact, trust_delta_millionths).map_err(Into::into)
}

pub(in crate::store::memory) async fn update_fact_tx(
    db: &Database,
    transaction: &Transaction<'_>,
    request: &FactUpdateCommand,
) -> FactStoreResult<FactUpdateOutcome> {
    let request_digest = digest(json!({
        "target": target_digest(request.target())?,
        "expected_last_event_id": request.expected_last_event_id().map(FactEventId::as_str),
        "content": request.patch().content(),
        "category": request.patch().category().map(category_label),
        "source": match request.patch().source() {
            None => json!({"changed": false}),
            Some(value) => json!({"changed": true, "value": value}),
        },
        "tags": request.patch().tags(),
        "entities": request.patch().entities(),
        "metadata": request.patch().metadata(),
        "trust": request.patch().trust().map(Confidence::as_f64),
        "actor": request.actor().map(ActorId::as_str),
    }))?;
    if let Some(receipt) = lookup_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "update",
        &request_digest,
    )
    .await?
    {
        return replay_update_tx(transaction, request.target().owner(), &receipt).await;
    }
    let fact_id = resolve_target_tx(transaction, request.target())
        .await?
        .ok_or_else(|| {
            storage_message(
                FACT_WRITE_OPERATION,
                "compatibility update target is missing",
            )
        })?;
    let owner_key = OwnerKey::new(request.target().owner())?;
    let current = load_current_fact_tx(transaction, &owner_key, request.target().owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                FACT_WRITE_OPERATION,
                "compatibility update target is unavailable",
            )
        })?;
    let previous_payload = current
        .payload()
        .ok_or(FactLineageError::PayloadAccessMismatch)?;
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
    let source = match request.patch().source() {
        Some(Some(source)) => fact_source_label(Some(source))?,
        Some(None) => "manual".to_owned(),
        None => {
            let mapping =
                required_mapping_tx(transaction, request.target().owner(), &fact_id).await?;
            source_for_fact_tx(transaction, &mapping).await?
        }
    };
    let Some(sanitized) = sanitize_payload(content, category, tags, entities, metadata)? else {
        return Err(storage_message(
            FACT_WRITE_OPERATION,
            "compatibility update payload was rejected by the privacy sanitizer",
        )
        .into());
    };
    let new_trust = request.patch().trust().unwrap_or(current.trust());
    let now = now()?;
    let batch = correction_batch(
        &current,
        sanitized.payload.clone(),
        sanitized.access,
        new_trust,
        request
            .expected_last_event_id()
            .cloned()
            .or_else(|| Some(current.last_event_id().clone())),
        request.actor().cloned(),
        now,
    )?;
    let (canonical_receipt, _) = commit_batch_tx(transaction, &batch).await?;
    let mapping = required_mapping_tx(transaction, request.target().owner(), &fact_id).await?;
    mirror_update_tx(
        db,
        transaction,
        request.target().owner(),
        mapping.legacy_fact_id(),
        &sanitized.payload,
        &source,
        new_trust,
        now,
    )
    .await?;
    let fact = load_projection_tx(transaction, request.target().owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                FACT_WRITE_OPERATION,
                "updated compatibility projection is missing",
            )
        })?;
    let trust_delta_millionths =
        ((new_trust.as_f64() - current.trust().as_f64()) * 1_000_000.0).round() as i32;
    let receipt = json!({ "trust_delta_millionths": trust_delta_millionths });
    record_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "update",
        &request_digest,
        Some(&fact_id),
        Some(canonical_receipt.last_event_id()),
        &receipt,
        now,
    )
    .await?;
    FactUpdateOutcome::new(fact, trust_delta_millionths).map_err(Into::into)
}

async fn replay_remove_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    receipt: &OperationReceipt,
) -> FactStoreResult<FactRemoveOutcome> {
    let fact_id = receipt.fact_id.as_ref().ok_or_else(|| {
        storage_message(
            FACT_WRITE_OPERATION,
            "compatibility remove receipt fact is missing",
        )
    })?;
    let fact = load_projection_tx(transaction, owner, fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                FACT_WRITE_OPERATION,
                "compatibility remove replay fact is missing",
            )
        })?;
    let removed = receipt
        .receipt
        .get("removed")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            storage_message(
                FACT_WRITE_OPERATION,
                "compatibility remove receipt is malformed",
            )
        })?;
    let remaining_fact_count = active_fact_count_tx(transaction, owner).await?;
    Ok(FactRemoveOutcome::new(fact, removed, remaining_fact_count))
}

pub(in crate::store::memory) async fn remove_fact_tx(
    db: &Database,
    transaction: &Transaction<'_>,
    request: &FactRemoveCommand,
) -> FactStoreResult<FactRemoveOutcome> {
    let request_digest = digest(json!({
        "target": target_digest(request.target())?,
        "expected_last_event_id": request.expected_last_event_id().map(FactEventId::as_str),
        "actor": request.actor().map(ActorId::as_str),
    }))?;
    if let Some(receipt) = lookup_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "remove",
        &request_digest,
    )
    .await?
    {
        return replay_remove_tx(transaction, request.target().owner(), &receipt).await;
    }
    let now = now()?;
    // Resolving the target and loading its current projection inside this
    // same transaction lets an absent fact -- whether it was never added, or
    // was concurrently removed by another operation just before this one --
    // surface as the idempotent no-op outcome below instead of a hard
    // authority error. Callers no longer need a separate pre-read
    // transaction to get that idempotency (see `remove_fact_v1`).
    let Some(fact_id) = resolve_target_tx(transaction, request.target()).await? else {
        let remaining_fact_count =
            active_fact_count_tx(transaction, request.target().owner()).await?;
        return Ok(FactRemoveOutcome::not_found(remaining_fact_count));
    };
    let owner_key = OwnerKey::new(request.target().owner())?;
    let Some(current) = load_current_projection(transaction, &owner_key, &fact_id).await? else {
        let remaining_fact_count =
            active_fact_count_tx(transaction, request.target().owner()).await?;
        return Ok(FactRemoveOutcome::not_found(remaining_fact_count));
    };
    let removed = current.access != PayloadAccessState::Deleted;
    let event_id = if removed {
        let stored =
            load_current_fact_tx(transaction, &owner_key, request.target().owner(), &fact_id)
                .await?
                .ok_or_else(|| {
                    storage_message(
                        FACT_WRITE_OPERATION,
                        "compatibility remove target is unavailable",
                    )
                })?;
        let category = stored
            .payload()
            .ok_or(FactLineageError::PayloadAccessMismatch)?
            .category();
        let mapping = required_mapping_tx(transaction, request.target().owner(), &fact_id).await?;
        let expected_last_event_id = request
            .expected_last_event_id()
            .cloned()
            .or_else(|| current.last_event_id.clone())
            .ok_or_else(|| {
                storage_message(
                    FACT_WRITE_OPERATION,
                    "compatibility remove target has no lineage CAS identity",
                )
            })?;
        let canonical_event_id = if mapping.history_coverage() == LegacyHistoryCoverageV1::Unknown {
            let purge = db
                .purge_memory_v2_legacy_fact_payload_in_transaction(
                    transaction,
                    request.target().owner(),
                    mapping.source_store_id(),
                    &fact_id,
                    &expected_last_event_id,
                    request.actor(),
                    now,
                )
                .await
                .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?;
            if !purge.payload_purged() {
                return Err(storage_message(
                    FACT_WRITE_OPERATION,
                    "migrated compatibility fact was not purged",
                )
                .into());
            }
            mark_owner_banks_dirty_tx(db, transaction, request.target().owner(), category, now)
                .await?;
            load_current_projection(transaction, &owner_key, &fact_id)
                .await?
                .and_then(|projection| projection.last_event_id)
                .ok_or_else(|| {
                    storage_message(
                        FACT_WRITE_OPERATION,
                        "migrated compatibility purge produced no lineage event",
                    )
                })?
        } else {
            let batch = removal_batch(
                request.target().owner(),
                &fact_id,
                current.access,
                Some(expected_last_event_id),
                request.actor().cloned(),
                now,
            )?;
            let (canonical_receipt, _) = commit_batch_tx(transaction, &batch).await?;
            mirror_delete_tx(
                db,
                transaction,
                request.target().owner(),
                mapping.legacy_fact_id(),
                category,
                now,
            )
            .await?;
            canonical_receipt.last_event_id().clone()
        };
        tombstone_fact_derivatives_tx(
            transaction,
            request.target().owner(),
            fact_id.as_str(),
            canonical_event_id.as_str(),
            now,
        )
        .await
        .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?;
        Some(canonical_event_id)
    } else {
        None
    };
    let fact = load_projection_tx(transaction, request.target().owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                FACT_WRITE_OPERATION,
                "removed compatibility projection is missing",
            )
        })?;
    let remaining_fact_count = active_fact_count_tx(transaction, request.target().owner()).await?;
    let receipt = json!({ "removed": removed });
    record_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "remove",
        &request_digest,
        Some(&fact_id),
        event_id.as_ref(),
        &receipt,
        now,
    )
    .await?;
    Ok(FactRemoveOutcome::new(fact, removed, remaining_fact_count))
}
