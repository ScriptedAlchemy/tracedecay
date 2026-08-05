//! Compatibility fact feedback recording, history, inspection, and proposal promotion dispatch.

use super::super::envelope::{
    OperationReceipt, digest, lookup_operation_receipt_tx, receipt_u64,
    record_operation_receipt_tx, target_digest,
};
use super::super::primitives::{
    FACT_READ_OPERATION, FACT_WRITE_OPERATION, OwnerKey, fact_source_label, from_json,
    legacy_timestamp, now, row_f64, row_i64, row_optional_string, row_string, source_store_id,
    storage_error, storage_message,
};
use super::super::projection::{
    fact_status_tx, load_projection_tx, projection_metadata_tx, required_mapping_tx,
    resolve_target_tx,
};
use super::super::proposals::{
    advance_proposal_tx, proposal_action_id, proposal_record_tx, replay_proposal_tx,
};
use super::super::scoring::millionths;
use super::{
    MirrorInsert, commit_batch_tx, feedback_action_label, feedback_delta, initial_batch,
    legacy_mapping_for_new_fact, load_current_fact_tx, mirror_feedback_tx, mirror_insert_tx,
    payload_metadata, query_fact_lineage_tx, sanitize_payload, update_feedback_projection_tx,
};
use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use crate::db::{Database, publish_fact_feedback_finding_tx};
use crate::privacy::sanitize_provider_metadata_text;
use serde_json::{Value, json};
use tracedecay_domain::{
    ActorId, Confidence, FactCurationActionV1, FactEventId, FactId, FactLineageEventKindV1,
    FactLineageEventV1, FactOwnerV1, RetrievalAnchorRecordV2, UtcMicros,
};
use tracedecay_store::{
    FactCommitOutcome, FactFeedbackAction, FactFeedbackCommand, FactFeedbackDetailsAvailability,
    FactFeedbackHistory, FactFeedbackHistoryEntry, FactFeedbackHistoryQuery, FactFeedbackOutcome,
    FactHistory, FactInspection, FactLineageCursor, FactLineageError, FactLineageQuery,
    FactLineageResult, FactProjection, FactProposalPromotion, FactProposalPromotionDisposition,
    FactProposalPromotionResult, FactProposalRecord, FactProposalState, FactStoreResult,
    FactTarget, FactWriteBatch, FeedbackRepairProgress, PromoteFactProposalOutcome, StoredFactV1,
};
fn receipt_i32(receipt: &Value, field: &'static str) -> FactLineageResult<i32> {
    receipt
        .get(field)
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| {
            storage_message(
                FACT_WRITE_OPERATION,
                format!("compatibility receipt {field} is malformed"),
            )
        })
}

fn receipt_confidence(receipt: &Value, field: &'static str) -> FactLineageResult<Confidence> {
    let millionths = receipt_u64(receipt, field)?;
    if millionths > 1_000_000 {
        return Err(storage_message(
            FACT_WRITE_OPERATION,
            format!("compatibility receipt {field} is out of range"),
        ));
    }
    Confidence::new(millionths as f64 / 1_000_000.0).map_err(FactLineageError::from)
}

fn feedback_detail(value: Option<&str>) -> Option<String> {
    value
        .and_then(sanitize_provider_metadata_text)
        .filter(|value| !value.trim().is_empty())
}

fn feedback_details(
    source: Option<&str>,
    reason: Option<&str>,
) -> (
    String,
    Option<String>,
    Option<String>,
    FactFeedbackDetailsAvailability,
) {
    let persisted_source = match source {
        Some(source) => feedback_detail(Some(source)),
        None => Some("mcp".to_owned()),
    };
    let persisted_note = feedback_detail(reason);
    let details_available = reason.is_none() || persisted_note.is_some();
    if let Some(source) = persisted_source
        && details_available
    {
        (
            source.clone(),
            Some(source),
            persisted_note,
            FactFeedbackDetailsAvailability::Available,
        )
    } else {
        (
            "mcp".to_owned(),
            None,
            None,
            FactFeedbackDetailsAvailability::Unknown,
        )
    }
}

fn feedback_batch(
    fact: &StoredFactV1,
    new_trust: Confidence,
    expected_last_event_id: Option<FactEventId>,
    actor: Option<ActorId>,
    now: UtcMicros,
) -> FactLineageResult<FactWriteBatch> {
    let kind = if new_trust == fact.trust() {
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::Retained,
            evidence_ids: Vec::new(),
        }
    } else {
        FactLineageEventKindV1::TrustChanged {
            previous: fact.trust(),
            current: new_trust,
            evidence_ids: Vec::new(),
        }
    };
    let event = FactLineageEventV1::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        kind,
        now,
        actor,
    )?;
    FactWriteBatch::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        None,
        vec![event],
        Vec::new(),
        Vec::new(),
        None,
        expected_last_event_id,
    )
}

fn feedback_details_label(availability: FactFeedbackDetailsAvailability) -> &'static str {
    match availability {
        FactFeedbackDetailsAvailability::Available => "available",
        FactFeedbackDetailsAvailability::LegacyRedacted => "legacy_redacted",
        FactFeedbackDetailsAvailability::Unknown => "unknown",
    }
}

fn feedback_details_availability(
    value: &str,
) -> FactLineageResult<FactFeedbackDetailsAvailability> {
    match value {
        "available" => Ok(FactFeedbackDetailsAvailability::Available),
        "legacy_redacted" => Ok(FactFeedbackDetailsAvailability::LegacyRedacted),
        "unknown" => Ok(FactFeedbackDetailsAvailability::Unknown),
        _ => Err(storage_message(
            FACT_READ_OPERATION,
            format!("unknown compatibility feedback detail availability {value:?}"),
        )),
    }
}

fn feedback_action(value: &str) -> FactLineageResult<FactFeedbackAction> {
    match value {
        "helpful" => Ok(FactFeedbackAction::Helpful),
        "unhelpful" => Ok(FactFeedbackAction::Unhelpful),
        _ => Err(storage_message(
            FACT_READ_OPERATION,
            format!("unknown compatibility feedback action {value:?}"),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
async fn record_feedback_history_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
    event_id: &FactEventId,
    legacy_feedback_event_id: i64,
    action: FactFeedbackAction,
    old_trust: Confidence,
    new_trust: Confidence,
    occurred_at: UtcMicros,
    source: Option<&str>,
    note: Option<&str>,
    availability: FactFeedbackDetailsAvailability,
) -> FactLineageResult<()> {
    if legacy_feedback_event_id <= 0 {
        return Err(storage_message(
            FACT_WRITE_OPERATION,
            "compatibility legacy feedback event id must be positive",
        ));
    }
    let key = OwnerKey::new(owner)?;
    let source_store_id = source_store_id()?;
    transaction
        .execute(
            "INSERT INTO memory_v2_legacy_feedback_event_map(
                owner_kind, project_id, source_store_id, legacy_feedback_event_id, fact_id, event_id
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                key.kind,
                key.project_id.as_str(),
                source_store_id.as_str(),
                legacy_feedback_event_id,
                fact_id.as_str(),
                event_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?;
    transaction
        .execute(
            "INSERT INTO memory_v2_feedback_history(
                owner_kind, project_id, fact_id, event_id, action, old_trust, new_trust,
                occurred_at, source, note, details_availability
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                key.kind,
                key.project_id.as_str(),
                fact_id.as_str(),
                event_id.as_str(),
                feedback_action_label(action),
                old_trust.as_f64(),
                new_trust.as_f64(),
                occurred_at.0,
                source,
                note,
                feedback_details_label(availability),
            ],
        )
        .await
        .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?;
    Ok(())
}

async fn replay_feedback_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    receipt: &OperationReceipt,
) -> FactStoreResult<FactFeedbackOutcome> {
    let fact_id = receipt.fact_id.as_ref().ok_or_else(|| {
        storage_message(
            FACT_WRITE_OPERATION,
            "compatibility feedback receipt fact is missing",
        )
    })?;
    let event_id = receipt.event_id.as_ref().ok_or_else(|| {
        storage_message(
            FACT_WRITE_OPERATION,
            "compatibility feedback receipt event is missing",
        )
    })?;
    let fact = load_projection_tx(transaction, owner, fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                FACT_WRITE_OPERATION,
                "compatibility feedback replay fact is missing",
            )
        })?;
    let legacy_feedback_event_id =
        i64::try_from(receipt_u64(&receipt.receipt, "legacy_feedback_event_id")?).map_err(
            |_| {
                storage_message(
                    FACT_WRITE_OPERATION,
                    "compatibility feedback receipt legacy event id is out of range",
                )
            },
        )?;
    FactFeedbackOutcome::new(
        fact,
        event_id.clone(),
        Some(legacy_feedback_event_id),
        receipt_confidence(&receipt.receipt, "old_trust_millionths")?,
        receipt_confidence(&receipt.receipt, "new_trust_millionths")?,
        receipt_i32(&receipt.receipt, "trust_delta_millionths")?,
        receipt_u64(&receipt.receipt, "helpful_count")?,
        receipt_u64(&receipt.receipt, "unhelpful_count")?,
    )
    .map_err(Into::into)
}

pub(in crate::store::memory) async fn record_fact_feedback_tx(
    transaction: &Transaction<'_>,
    request: &FactFeedbackCommand,
) -> FactStoreResult<FactFeedbackOutcome> {
    let request_digest = digest(json!({
        "target": target_digest(request.target())?,
        "expected_last_event_id": request.expected_last_event_id().map(FactEventId::as_str),
        "action": feedback_action_label(request.action()),
        "actor": request.actor().map(ActorId::as_str),
        "source": request.source(),
        "reason": request.reason(),
    }))?;
    if let Some(receipt) = lookup_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "feedback",
        &request_digest,
    )
    .await?
    {
        return replay_feedback_tx(transaction, request.target().owner(), &receipt).await;
    }
    let fact_id = resolve_target_tx(transaction, request.target())
        .await?
        .ok_or_else(|| {
            storage_message(
                FACT_WRITE_OPERATION,
                "compatibility feedback target is missing",
            )
        })?;
    let owner_key = OwnerKey::new(request.target().owner())?;
    let current = load_current_fact_tx(transaction, &owner_key, request.target().owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                FACT_WRITE_OPERATION,
                "compatibility feedback target is unavailable",
            )
        })?;
    let old_trust = current.trust();
    let new_trust =
        Confidence::new((old_trust.as_f64() + feedback_delta(request.action())).clamp(0.0, 1.0))
            .map_err(FactLineageError::from)?;
    let now = now()?;
    let batch = feedback_batch(
        &current,
        new_trust,
        request
            .expected_last_event_id()
            .cloned()
            .or_else(|| Some(current.last_event_id().clone())),
        request.actor().cloned(),
        now,
    )?;
    let (canonical_receipt, _) = commit_batch_tx(transaction, &batch).await?;
    let event_id = canonical_receipt.last_event_id().clone();
    publish_fact_feedback_finding_tx(
        transaction,
        request.target().owner(),
        fact_id.as_str(),
        event_id.as_str(),
    )
    .await
    .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?;
    let mapping = required_mapping_tx(transaction, request.target().owner(), &fact_id).await?;
    let (mirror_source, history_source, history_note, availability) =
        feedback_details(request.source(), request.reason());
    let legacy_feedback_event_id = mirror_feedback_tx(
        transaction,
        mapping.legacy_fact_id(),
        request.action(),
        old_trust,
        new_trust,
        legacy_timestamp(now),
        &mirror_source,
        history_note.as_deref(),
    )
    .await?;
    record_feedback_history_tx(
        transaction,
        request.target().owner(),
        &fact_id,
        &event_id,
        legacy_feedback_event_id,
        request.action(),
        old_trust,
        new_trust,
        now,
        history_source.as_deref(),
        history_note.as_deref(),
        availability,
    )
    .await?;
    update_feedback_projection_tx(
        transaction,
        request.target().owner(),
        &fact_id,
        request.action(),
        now,
    )
    .await?;
    let fact = load_projection_tx(transaction, request.target().owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                FACT_WRITE_OPERATION,
                "compatibility feedback projection is missing",
            )
        })?;
    let (_, _, telemetry) = projection_metadata_tx(
        transaction,
        request.target().owner(),
        &fact_id,
        Some(&mapping),
    )
    .await?;
    let trust_delta_millionths =
        ((new_trust.as_f64() - old_trust.as_f64()) * 1_000_000.0).round() as i32;
    let receipt = json!({
        "old_trust_millionths": millionths(old_trust.as_f64()),
        "new_trust_millionths": millionths(new_trust.as_f64()),
        "trust_delta_millionths": trust_delta_millionths,
        "helpful_count": telemetry.helpful_count(),
        "unhelpful_count": telemetry.unhelpful_count(),
        "legacy_feedback_event_id": legacy_feedback_event_id,
    });
    record_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "feedback",
        &request_digest,
        Some(&fact_id),
        Some(&event_id),
        &receipt,
        now,
    )
    .await?;
    FactFeedbackOutcome::new(
        fact,
        event_id,
        Some(legacy_feedback_event_id),
        old_trust,
        new_trust,
        trust_delta_millionths,
        telemetry.helpful_count(),
        telemetry.unhelpful_count(),
    )
    .map_err(Into::into)
}

pub(in crate::store::memory) async fn fact_feedback_history_tx(
    transaction: &Transaction<'_>,
    query: &FactFeedbackHistoryQuery,
    repair_progress: FeedbackRepairProgress,
) -> FactStoreResult<FactFeedbackHistory> {
    let fact_id = resolve_target_tx(transaction, query.target())
        .await?
        .ok_or_else(|| {
            storage_message(
                FACT_READ_OPERATION,
                "compatibility feedback history target is missing",
            )
        })?;
    let key = OwnerKey::new(query.target().owner())?;
    let fetch_limit = i64::try_from(query.limit().saturating_add(1)).map_err(|_| {
        FactLineageError::InvalidQueryLimit {
            limit: query.limit(),
            max: usize::MAX,
        }
    })?;
    let after_time = query
        .after()
        .map(FactLineageCursor::occurred_at)
        .map(|time| time.0);
    let after_event = query.after().map(|cursor| cursor.event_id().as_str());
    let mut rows = transaction
        .query(
            "SELECT event_id, occurred_at, action, old_trust, new_trust,
                    source, note, details_availability
             FROM memory_v2_feedback_history
             WHERE owner_kind = ?1 AND project_id = ?2 AND fact_id = ?3
               AND (
                    ?4 IS NULL
                    OR occurred_at > ?4
                    OR (occurred_at = ?4 AND event_id > ?5)
               )
             ORDER BY occurred_at ASC, event_id ASC
             LIMIT ?6",
            params![
                key.kind,
                key.project_id.as_str(),
                fact_id.as_str(),
                after_time,
                after_event,
                fetch_limit,
            ],
        )
        .await
        .map_err(|error| storage_error(FACT_READ_OPERATION, error))?;
    let mut events = Vec::with_capacity(query.limit().saturating_add(1));
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(FACT_READ_OPERATION, error))?
    {
        events.push(FactFeedbackHistoryEntry::new(
            FactEventId::new(row_string(&row, 0, FACT_READ_OPERATION)?)
                .map_err(FactLineageError::from)?,
            UtcMicros(row_i64(&row, 1, FACT_READ_OPERATION)?),
            feedback_action(&row_string(&row, 2, FACT_READ_OPERATION)?)?,
            Confidence::new(row_f64(&row, 3, FACT_READ_OPERATION)?)
                .map_err(FactLineageError::from)?,
            Confidence::new(row_f64(&row, 4, FACT_READ_OPERATION)?)
                .map_err(FactLineageError::from)?,
            row_optional_string(&row, 5, FACT_READ_OPERATION)?,
            row_optional_string(&row, 6, FACT_READ_OPERATION)?,
            feedback_details_availability(&row_string(&row, 7, FACT_READ_OPERATION)?)?,
        )?);
    }
    let has_more = events.len() > query.limit();
    events.truncate(query.limit());
    let next_after = has_more
        .then(|| {
            events
                .last()
                .map(|event| FactLineageCursor::new(event.occurred_at(), event.event_id().clone()))
        })
        .flatten()
        .transpose()?;
    FactFeedbackHistory::new_with_repair_progress(
        query.target().owner().clone(),
        events,
        next_after,
        repair_progress,
    )
    .map_err(Into::into)
}

pub(in crate::store::memory) async fn inspect_fact_tx(
    transaction: &Transaction<'_>,
    target: &FactTarget,
) -> FactStoreResult<Option<FactInspection>> {
    let Some(fact_id) = resolve_target_tx(transaction, target).await? else {
        return Ok(None);
    };
    let Some(FactProjection::Available(fact)) =
        load_projection_tx(transaction, target.owner(), &fact_id).await?
    else {
        return Ok(None);
    };
    let lineage = FactLineageQuery::new(target.owner().clone(), fact_id.clone(), None, 1_000)?;
    let history = FactHistory::new(
        target.owner().clone(),
        fact_id.clone(),
        query_fact_lineage_tx(transaction, &lineage).await?,
        None,
    )?;
    let key = OwnerKey::new(target.owner())?;
    let mut rows = transaction
        .query(
            "SELECT DISTINCT anchors.anchor_json
             FROM memory_v2_evidence AS evidence
             JOIN retrieval_anchors AS anchors
               ON anchors.anchor_id = evidence.anchor_id
              AND anchors.owner_json = evidence.owner_json
             WHERE evidence.fact_id = ?1
               AND evidence.owner_kind = ?2
               AND evidence.project_id = ?3
               AND evidence.owner_json = ?4
               AND COALESCE((
                   SELECT disposition.state
                   FROM retrieval_anchor_dispositions AS disposition
                   WHERE disposition.anchor_id = anchors.anchor_id
                     AND disposition.owner_json = anchors.owner_json
                   ORDER BY disposition.sequence DESC LIMIT 1
               ), 'active') = 'active'
               AND NOT EXISTS (
                   SELECT 1
                   FROM retrieval_anchor_derivative_tombstones AS tombstone
                   WHERE tombstone.source_anchor_id = evidence.anchor_id
                     AND tombstone.owner_json = evidence.owner_json
                     AND tombstone.derivative_kind = 'contribution'
                     AND tombstone.derivative_id = evidence.evidence_id
               )
             ORDER BY anchors.anchor_id ASC
             LIMIT 1000",
            params![
                fact_id.as_str(),
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(FACT_READ_OPERATION, error))?;
    let mut anchors = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(FACT_READ_OPERATION, error))?
    {
        let anchor = from_json::<RetrievalAnchorRecordV2>(
            &row_string(&row, 0, FACT_READ_OPERATION)?,
            FACT_READ_OPERATION,
        )?;
        if FactOwnerV1::from(anchor.owner().clone()) != *target.owner() {
            return Err(FactLineageError::OwnerMismatch.into());
        }
        anchors.push(anchor);
    }
    let status = fact_status_tx(transaction, target.owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                FACT_READ_OPERATION,
                "compatibility inspection status is missing",
            )
        })?;
    FactInspection::new(*fact, history, anchors, status)
        .map(Some)
        .map_err(Into::into)
}

pub(super) struct CommitAttempt {
    pub(super) outcome: FactCommitOutcome,
    pub(super) wrote: bool,
}

pub(in crate::store::memory) struct PromotionAttempt {
    pub(in crate::store::memory) outcome: PromoteFactProposalOutcome,
    pub(in crate::store::memory) wrote: bool,
}

pub(in crate::store::memory) async fn promote_fact_proposal_tx(
    db: &Database,
    transaction: &Transaction<'_>,
    request: &FactProposalPromotion,
) -> FactStoreResult<FactProposalRecord> {
    let result = promote_fact_proposal_with_disposition_tx(db, transaction, request).await?;
    Ok(result.proposal().clone())
}

pub(in crate::store::memory) async fn promote_fact_proposal_with_disposition_tx(
    db: &Database,
    transaction: &Transaction<'_>,
    request: &FactProposalPromotion,
) -> FactStoreResult<FactProposalPromotionResult> {
    let material = json!({
        "proposal_id": request.proposal_id().as_str(),
        "expected_revision": request.expected_revision().get(),
        "reviewer": request.reviewer().map(ActorId::as_str),
    });
    let request_digest = digest(material.clone())?;
    let operation_id = proposal_action_id("proposal-promote", material)?;
    if let Some(receipt) = lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        &operation_id,
        "proposal_promote",
        &request_digest,
    )
    .await?
    {
        let proposal = replay_proposal_tx(transaction, request.owner(), &receipt).await?;
        let disposition = match proposal.state() {
            FactProposalState::Applied => FactProposalPromotionDisposition::AlreadyPromoted,
            FactProposalState::Quarantined => FactProposalPromotionDisposition::Quarantined,
            _ => {
                return Err(storage_message(
                    FACT_WRITE_OPERATION,
                    "compatibility promotion receipt does not resolve to a terminal proposal",
                )
                .into());
            }
        };
        return FactProposalPromotionResult::new(proposal, disposition).map_err(Into::into);
    }
    let proposal = proposal_record_tx(transaction, request.owner(), request.proposal_id())
        .await?
        .ok_or_else(|| storage_message(FACT_WRITE_OPERATION, "fact proposal is missing"))?;
    if proposal.state() != FactProposalState::PendingApproval
        || proposal.revision() != request.expected_revision()
    {
        return Err(storage_message(
            FACT_WRITE_OPERATION,
            "fact proposal revision or state changed before promotion",
        )
        .into());
    }
    let now = now()?;
    let payload_metadata = payload_metadata(proposal.request().metadata());
    let sanitized = sanitize_payload(
        proposal.request().content(),
        proposal.request().category(),
        proposal.request().tags(),
        proposal.request().entities(),
        &payload_metadata,
    )?;
    let Some(sanitized) = sanitized else {
        let reason = "content rejected by privacy sanitizer";
        advance_proposal_tx(
            transaction,
            request.owner(),
            request.proposal_id(),
            FactProposalState::PendingApproval,
            request.expected_revision(),
            FactProposalState::Quarantined,
            request.reviewer(),
            Some(reason),
            &request_digest,
            None,
            None,
            None,
            now,
        )
        .await?;
        let receipt = json!({
            "proposal_id": request.proposal_id().as_str(),
            "state": "quarantined",
            "revision": request.expected_revision().get().saturating_add(1),
        });
        record_operation_receipt_tx(
            transaction,
            request.owner(),
            &operation_id,
            "proposal_promote",
            &request_digest,
            None,
            None,
            &receipt,
            now,
        )
        .await?;
        let quarantined = replay_proposal_tx(
            transaction,
            request.owner(),
            &OperationReceipt {
                fact_id: None,
                event_id: None,
                receipt,
            },
        )
        .await?;
        return FactProposalPromotionResult::new(
            quarantined,
            FactProposalPromotionDisposition::Quarantined,
        )
        .map_err(Into::into);
    };
    let source = fact_source_label(proposal.request().source())?;
    let (fact_id, assertion_id, event_id) = match mirror_insert_tx(
        db,
        transaction,
        request.owner(),
        &sanitized.payload,
        &source,
        proposal.request().default_trust(),
        now,
    )
    .await?
    {
        MirrorInsert::Existing { fact_id, .. } => {
            let key = OwnerKey::new(request.owner())?;
            let fact = load_current_fact_tx(transaction, &key, request.owner(), &fact_id)
                .await?
                .ok_or_else(|| {
                    storage_message(
                        FACT_WRITE_OPERATION,
                        "existing compatibility mirror has no canonical current fact",
                    )
                })?;
            (
                fact_id,
                fact.active_assertion_id().clone(),
                fact.last_event_id().clone(),
            )
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
                proposal.request().default_trust(),
                proposal.request().actor().cloned(),
                now,
            )?;
            let (receipt, _) = commit_batch_tx(transaction, &batch).await?;
            let assertion_id = receipt.active_assertion_id().cloned().ok_or_else(|| {
                storage_message(
                    FACT_WRITE_OPERATION,
                    "promoted compatibility fact has no active assertion",
                )
            })?;
            (
                mapping.fact_id().clone(),
                assertion_id,
                receipt.last_event_id().clone(),
            )
        }
    };
    advance_proposal_tx(
        transaction,
        request.owner(),
        request.proposal_id(),
        FactProposalState::PendingApproval,
        request.expected_revision(),
        FactProposalState::Applied,
        request.reviewer(),
        None,
        &request_digest,
        Some(&fact_id),
        Some(&assertion_id),
        Some(&event_id),
        now,
    )
    .await?;
    let receipt = json!({
        "proposal_id": request.proposal_id().as_str(),
        "state": "applied",
        "revision": request.expected_revision().get().saturating_add(1),
    });
    record_operation_receipt_tx(
        transaction,
        request.owner(),
        &operation_id,
        "proposal_promote",
        &request_digest,
        Some(&fact_id),
        Some(&event_id),
        &receipt,
        now,
    )
    .await?;
    let promoted = replay_proposal_tx(
        transaction,
        request.owner(),
        &OperationReceipt {
            fact_id: Some(fact_id),
            event_id: Some(event_id),
            receipt,
        },
    )
    .await?;
    FactProposalPromotionResult::new(promoted, FactProposalPromotionDisposition::NewlyPromoted)
        .map_err(Into::into)
}
