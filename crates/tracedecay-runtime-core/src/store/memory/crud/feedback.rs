//! Compatibility fact feedback recording, history, inspection, and proposal promotion dispatch.

use super::super::envelope::{
    CompatibilityOperationReceiptV1, compatibility_digest,
    compatibility_lookup_operation_receipt_tx, compatibility_receipt_u64,
    compatibility_record_operation_receipt_tx, compatibility_target_digest,
};
use super::super::primitives::{
    COMPATIBILITY_READ_OPERATION, COMPATIBILITY_WRITE_OPERATION, OwnerKey, compatibility_now,
    compatibility_source_label, from_json, row_f64, row_i64, row_optional_string, row_string,
    storage_error, storage_message,
};
use super::super::projection::{
    compatibility_fact_status_tx, compatibility_projection_metadata_tx,
    compatibility_required_mapping_tx, load_compatibility_projection_tx,
    resolve_compatibility_target_tx,
};
use super::super::proposals::{
    compatibility_advance_proposal_tx, compatibility_proposal_action_id,
    compatibility_proposal_record_tx, compatibility_replay_proposal_tx,
};
use super::super::scoring::compatibility_millionths;
use super::{
    CompatibilityMirrorInsertV1, compatibility_commit_batch_tx,
    compatibility_feedback_action_label, compatibility_feedback_delta, compatibility_initial_batch,
    compatibility_legacy_mapping_for_new_fact, compatibility_mirror_insert_tx,
    compatibility_payload_metadata, compatibility_sanitize_payload,
    compatibility_update_feedback_projection_tx, load_current_fact_tx, query_fact_lineage_tx,
};
use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use crate::db::{Database, publish_fact_feedback_finding_tx};
use crate::privacy::sanitize_provider_metadata_text;
use serde_json::{Value, json};
use tracedecay_domain::{
    ActorId, Confidence, FactCurationActionV1, FactEventId, FactId, FactLineageEventKindV1,
    FactLineageEventV1, FactOwnerV1, FeedbackResultId, RetrievalAnchorRecordV2, UtcMicros,
};
use tracedecay_store::{
    CompatibilityFactFeedbackActionV1, CompatibilityFactFeedbackCommandV1,
    CompatibilityFactFeedbackOutcomeV1, CompatibilityFactHistoryV1, CompatibilityFactInspectionV1,
    CompatibilityFactProjectionV1, CompatibilityFactProposalPromotionDispositionV1,
    CompatibilityFactProposalPromotionResultV1, CompatibilityFactProposalPromotionV1,
    CompatibilityFactProposalRecordV1, CompatibilityFactProposalStateV1, CompatibilityFactTargetV1,
    FactCommitOutcome, FactCompatibilityResult, FactFeedbackDetailsAvailability,
    FactFeedbackHistoryEntry, FactFeedbackHistoryPage, FactFeedbackHistoryQuery, FactLineageQuery,
    FactStoreError, FactStoreResult, FactWriteBatch, PromoteFactProposalOutcome, StoredFactV1,
};
fn compatibility_receipt_i32(receipt: &Value, field: &'static str) -> FactStoreResult<i32> {
    receipt
        .get(field)
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                format!("compatibility receipt {field} is malformed"),
            )
        })
}

fn compatibility_receipt_confidence(
    receipt: &Value,
    field: &'static str,
) -> FactStoreResult<Confidence> {
    let millionths = compatibility_receipt_u64(receipt, field)?;
    if millionths > 1_000_000 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            format!("compatibility receipt {field} is out of range"),
        ));
    }
    Confidence::new(millionths as f64 / 1_000_000.0).map_err(FactStoreError::from)
}

fn compatibility_feedback_detail(value: Option<&str>) -> Option<String> {
    value
        .and_then(sanitize_provider_metadata_text)
        .filter(|value| !value.trim().is_empty())
}

fn compatibility_feedback_details(
    source: Option<&str>,
    reason: Option<&str>,
) -> (
    String,
    Option<String>,
    Option<String>,
    FactFeedbackDetailsAvailability,
) {
    let persisted_source = match source {
        Some(source) => compatibility_feedback_detail(Some(source)),
        None => Some("mcp".to_owned()),
    };
    let persisted_note = compatibility_feedback_detail(reason);
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

fn compatibility_feedback_batch(
    fact: &StoredFactV1,
    new_trust: Confidence,
    expected_last_event_id: Option<FactEventId>,
    actor: Option<ActorId>,
    now: UtcMicros,
) -> FactStoreResult<FactWriteBatch> {
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

fn compatibility_feedback_details_label(
    availability: FactFeedbackDetailsAvailability,
) -> &'static str {
    match availability {
        FactFeedbackDetailsAvailability::Available => "available",
        FactFeedbackDetailsAvailability::Unknown => "unknown",
    }
}

fn feedback_result_id(event_id: &FactEventId) -> FactStoreResult<FeedbackResultId> {
    FeedbackResultId::new(event_id.as_str().to_owned()).map_err(FactStoreError::from)
}

fn compatibility_feedback_action(
    value: &str,
) -> FactStoreResult<CompatibilityFactFeedbackActionV1> {
    match value {
        "helpful" => Ok(CompatibilityFactFeedbackActionV1::Helpful),
        "unhelpful" => Ok(CompatibilityFactFeedbackActionV1::Unhelpful),
        _ => Err(storage_message(
            COMPATIBILITY_READ_OPERATION,
            format!("unknown compatibility feedback action {value:?}"),
        )),
    }
}

pub(in crate::store::memory) async fn query_fact_feedback_history_tx(
    transaction: &Transaction<'_>,
    query: &FactFeedbackHistoryQuery,
) -> FactStoreResult<FactFeedbackHistoryPage> {
    let owner = OwnerKey::new(query.owner())?;
    let fetch_limit = i64::try_from(query.limit().saturating_add(1)).map_err(|_| {
        FactStoreError::InvalidQueryLimit {
            limit: query.limit(),
            max: usize::MAX,
        }
    })?;
    let mut rows = match query.after() {
        Some(after) => {
            transaction
                .query(
                    "SELECT result_id, occurred_at, action, old_trust, new_trust,
                            source, note, details_availability
                     FROM memory_v2_feedback_history
                     WHERE owner_kind = ?1 AND project_id = ?2 AND fact_id = ?3
                       AND (occurred_at, result_id) > (
                           SELECT occurred_at, result_id
                           FROM memory_v2_feedback_history
                           WHERE owner_kind = ?1 AND project_id = ?2
                             AND fact_id = ?3 AND result_id = ?4
                       )
                     ORDER BY occurred_at ASC, result_id ASC
                     LIMIT ?5",
                    params![
                        owner.kind,
                        owner.project_id.as_str(),
                        query.fact_id().as_str(),
                        after.as_str(),
                        fetch_limit,
                    ],
                )
                .await
        }
        None => {
            transaction
                .query(
                    "SELECT result_id, occurred_at, action, old_trust, new_trust,
                            source, note, details_availability
                     FROM memory_v2_feedback_history
                     WHERE owner_kind = ?1 AND project_id = ?2 AND fact_id = ?3
                     ORDER BY occurred_at ASC, result_id ASC
                     LIMIT ?4",
                    params![
                        owner.kind,
                        owner.project_id.as_str(),
                        query.fact_id().as_str(),
                        fetch_limit,
                    ],
                )
                .await
        }
    }
    .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;

    let mut events = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        let availability = match row_string(&row, 7, COMPATIBILITY_READ_OPERATION)?.as_str() {
            "available" => FactFeedbackDetailsAvailability::Available,
            "unknown" => FactFeedbackDetailsAvailability::Unknown,
            value => {
                return Err(storage_message(
                    COMPATIBILITY_READ_OPERATION,
                    format!("unknown canonical feedback detail availability {value:?}"),
                ));
            }
        };
        events.push(FactFeedbackHistoryEntry::new(
            FeedbackResultId::new(row_string(&row, 0, COMPATIBILITY_READ_OPERATION)?)
                .map_err(FactStoreError::from)?,
            UtcMicros(row_i64(&row, 1, COMPATIBILITY_READ_OPERATION)?),
            compatibility_feedback_action(&row_string(&row, 2, COMPATIBILITY_READ_OPERATION)?)?,
            Confidence::new(row_f64(&row, 3, COMPATIBILITY_READ_OPERATION)?)
                .map_err(FactStoreError::from)?,
            Confidence::new(row_f64(&row, 4, COMPATIBILITY_READ_OPERATION)?)
                .map_err(FactStoreError::from)?,
            row_optional_string(&row, 5, COMPATIBILITY_READ_OPERATION)?,
            row_optional_string(&row, 6, COMPATIBILITY_READ_OPERATION)?,
            availability,
        )?);
    }
    let has_more = events.len() > query.limit();
    events.truncate(query.limit());
    let next_after = if has_more {
        events.last().map(|event| event.result_id().clone())
    } else {
        None
    };
    FactFeedbackHistoryPage::new(query.owner().clone(), events, next_after)
}

#[allow(clippy::too_many_arguments)]
async fn compatibility_record_feedback_history_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
    result_id: &FeedbackResultId,
    action: CompatibilityFactFeedbackActionV1,
    old_trust: Confidence,
    new_trust: Confidence,
    occurred_at: UtcMicros,
    source: Option<&str>,
    note: Option<&str>,
    availability: FactFeedbackDetailsAvailability,
) -> FactStoreResult<()> {
    let key = OwnerKey::new(owner)?;
    transaction
        .execute(
            "INSERT INTO memory_v2_feedback_history(
                owner_kind, project_id, fact_id, result_id, action, old_trust, new_trust,
                occurred_at, source, note, details_availability
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                key.kind,
                key.project_id.as_str(),
                fact_id.as_str(),
                result_id.as_str(),
                compatibility_feedback_action_label(action),
                old_trust.as_f64(),
                new_trust.as_f64(),
                occurred_at.0,
                source,
                note,
                compatibility_feedback_details_label(availability),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    Ok(())
}

async fn compatibility_replay_feedback_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    receipt: &CompatibilityOperationReceiptV1,
) -> FactCompatibilityResult<CompatibilityFactFeedbackOutcomeV1> {
    let fact_id = receipt.fact_id.as_ref().ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility feedback receipt fact is missing",
        )
    })?;
    let event_id = receipt.event_id.as_ref().ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility feedback receipt event is missing",
        )
    })?;
    let fact = load_compatibility_projection_tx(transaction, owner, fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility feedback replay fact is missing",
            )
        })?;
    CompatibilityFactFeedbackOutcomeV1::new(
        fact,
        event_id.clone(),
        compatibility_receipt_confidence(&receipt.receipt, "old_trust_millionths")?,
        compatibility_receipt_confidence(&receipt.receipt, "new_trust_millionths")?,
        compatibility_receipt_i32(&receipt.receipt, "trust_delta_millionths")?,
        compatibility_receipt_u64(&receipt.receipt, "helpful_count")?,
        compatibility_receipt_u64(&receipt.receipt, "unhelpful_count")?,
    )
    .map_err(Into::into)
}

pub(in crate::store::memory) async fn record_compatibility_fact_feedback_tx(
    transaction: &Transaction<'_>,
    request: &CompatibilityFactFeedbackCommandV1,
) -> FactCompatibilityResult<CompatibilityFactFeedbackOutcomeV1> {
    let request_digest = compatibility_digest(json!({
        "target": compatibility_target_digest(request.target())?,
        "expected_last_event_id": request.expected_last_event_id().map(FactEventId::as_str),
        "action": compatibility_feedback_action_label(request.action()),
        "actor": request.actor().map(ActorId::as_str),
        "source": request.source(),
        "reason": request.reason(),
    }))?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "feedback",
        &request_digest,
    )
    .await?
    {
        return compatibility_replay_feedback_tx(transaction, request.target().owner(), &receipt)
            .await;
    }
    let fact_id = resolve_compatibility_target_tx(transaction, request.target())
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility feedback target is missing",
            )
        })?;
    let owner_key = OwnerKey::new(request.target().owner())?;
    let current = load_current_fact_tx(transaction, &owner_key, request.target().owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility feedback target is unavailable",
            )
        })?;
    let old_trust = current.trust();
    let new_trust = Confidence::new(
        (old_trust.as_f64() + compatibility_feedback_delta(request.action())).clamp(0.0, 1.0),
    )
    .map_err(FactStoreError::from)?;
    let now = compatibility_now()?;
    let batch = compatibility_feedback_batch(
        &current,
        new_trust,
        request
            .expected_last_event_id()
            .cloned()
            .or_else(|| Some(current.last_event_id().clone())),
        request.actor().cloned(),
        now,
    )?;
    let (canonical_receipt, _) = compatibility_commit_batch_tx(transaction, &batch).await?;
    let event_id = canonical_receipt.last_event_id().clone();
    publish_fact_feedback_finding_tx(
        transaction,
        request.target().owner(),
        fact_id.as_str(),
        event_id.as_str(),
    )
    .await
    .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let mapping =
        compatibility_required_mapping_tx(transaction, request.target().owner(), &fact_id).await?;
    let (_canonical_source, history_source, history_note, availability) =
        compatibility_feedback_details(request.source(), request.reason());
    let result_id = feedback_result_id(&event_id)?;
    compatibility_record_feedback_history_tx(
        transaction,
        request.target().owner(),
        &fact_id,
        &result_id,
        request.action(),
        old_trust,
        new_trust,
        now,
        history_source.as_deref(),
        history_note.as_deref(),
        availability,
    )
    .await?;
    compatibility_update_feedback_projection_tx(
        transaction,
        request.target().owner(),
        &fact_id,
        request.action(),
        now,
    )
    .await?;
    let fact = load_compatibility_projection_tx(transaction, request.target().owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility feedback projection is missing",
            )
        })?;
    let (_, _, telemetry) = compatibility_projection_metadata_tx(
        transaction,
        request.target().owner(),
        &fact_id,
        Some(&mapping),
    )
    .await?;
    let trust_delta_millionths =
        ((new_trust.as_f64() - old_trust.as_f64()) * 1_000_000.0).round() as i32;
    let receipt = json!({
        "old_trust_millionths": compatibility_millionths(old_trust.as_f64()),
        "new_trust_millionths": compatibility_millionths(new_trust.as_f64()),
        "trust_delta_millionths": trust_delta_millionths,
        "helpful_count": telemetry.helpful_count(),
        "unhelpful_count": telemetry.unhelpful_count(),
    });
    compatibility_record_operation_receipt_tx(
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
    CompatibilityFactFeedbackOutcomeV1::new(
        fact,
        event_id,
        old_trust,
        new_trust,
        trust_delta_millionths,
        telemetry.helpful_count(),
        telemetry.unhelpful_count(),
    )
    .map_err(Into::into)
}

pub(in crate::store::memory) async fn inspect_compatibility_fact_tx(
    transaction: &Transaction<'_>,
    target: &CompatibilityFactTargetV1,
) -> FactCompatibilityResult<Option<CompatibilityFactInspectionV1>> {
    let Some(fact_id) = resolve_compatibility_target_tx(transaction, target).await? else {
        return Ok(None);
    };
    let Some(CompatibilityFactProjectionV1::Available(fact)) =
        load_compatibility_projection_tx(transaction, target.owner(), &fact_id).await?
    else {
        return Ok(None);
    };
    let lineage = FactLineageQuery::new(target.owner().clone(), fact_id.clone(), None, 1_000)?;
    let history = CompatibilityFactHistoryV1::new(
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
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut anchors = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        let anchor = from_json::<RetrievalAnchorRecordV2>(
            &row_string(&row, 0, COMPATIBILITY_READ_OPERATION)?,
            COMPATIBILITY_READ_OPERATION,
        )?;
        if FactOwnerV1::from(anchor.owner().clone()) != *target.owner() {
            return Err(FactStoreError::OwnerMismatch.into());
        }
        anchors.push(anchor);
    }
    let status = compatibility_fact_status_tx(transaction, target.owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility inspection status is missing",
            )
        })?;
    CompatibilityFactInspectionV1::new(*fact, history, anchors, status)
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

pub(in crate::store::memory) async fn promote_compatibility_fact_proposal_tx(
    db: &Database,
    transaction: &Transaction<'_>,
    request: &CompatibilityFactProposalPromotionV1,
) -> FactCompatibilityResult<CompatibilityFactProposalRecordV1> {
    let result =
        promote_compatibility_fact_proposal_with_disposition_tx(db, transaction, request).await?;
    Ok(result.proposal().clone())
}

pub(in crate::store::memory) async fn promote_compatibility_fact_proposal_with_disposition_tx(
    db: &Database,
    transaction: &Transaction<'_>,
    request: &CompatibilityFactProposalPromotionV1,
) -> FactCompatibilityResult<CompatibilityFactProposalPromotionResultV1> {
    let material = json!({
        "proposal_id": request.proposal_id().as_str(),
        "expected_revision": request.expected_revision().get(),
        "reviewer": request.reviewer().map(ActorId::as_str),
    });
    let request_digest = compatibility_digest(material.clone())?;
    let operation_id = compatibility_proposal_action_id("proposal-promote", material)?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        &operation_id,
        "proposal_promote",
        &request_digest,
    )
    .await?
    {
        let proposal =
            compatibility_replay_proposal_tx(transaction, request.owner(), &receipt).await?;
        let disposition = match proposal.state() {
            CompatibilityFactProposalStateV1::Applied => {
                CompatibilityFactProposalPromotionDispositionV1::AlreadyPromoted
            }
            CompatibilityFactProposalStateV1::Quarantined => {
                CompatibilityFactProposalPromotionDispositionV1::Quarantined
            }
            _ => {
                return Err(storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility promotion receipt does not resolve to a terminal proposal",
                )
                .into());
            }
        };
        return CompatibilityFactProposalPromotionResultV1::new(proposal, disposition)
            .map_err(Into::into);
    }
    let proposal =
        compatibility_proposal_record_tx(transaction, request.owner(), request.proposal_id())
            .await?
            .ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility proposal is missing",
                )
            })?;
    if proposal.state() != CompatibilityFactProposalStateV1::PendingApproval
        || proposal.revision() != request.expected_revision()
    {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility proposal revision or state changed before promotion",
        )
        .into());
    }
    let now = compatibility_now()?;
    let payload_metadata = compatibility_payload_metadata(proposal.request().metadata());
    let sanitized = compatibility_sanitize_payload(
        proposal.request().content(),
        proposal.request().category(),
        proposal.request().tags(),
        proposal.request().entities(),
        &payload_metadata,
    )?;
    let Some(sanitized) = sanitized else {
        let reason = "content rejected by privacy sanitizer";
        compatibility_advance_proposal_tx(
            transaction,
            request.owner(),
            request.proposal_id(),
            CompatibilityFactProposalStateV1::PendingApproval,
            request.expected_revision(),
            CompatibilityFactProposalStateV1::Quarantined,
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
        compatibility_record_operation_receipt_tx(
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
        let quarantined = compatibility_replay_proposal_tx(
            transaction,
            request.owner(),
            &CompatibilityOperationReceiptV1 {
                fact_id: None,
                event_id: None,
                receipt,
            },
        )
        .await?;
        return CompatibilityFactProposalPromotionResultV1::new(
            quarantined,
            CompatibilityFactProposalPromotionDispositionV1::Quarantined,
        )
        .map_err(Into::into);
    };
    let source = compatibility_source_label(proposal.request().source())?;
    let (fact_id, assertion_id, event_id) = match compatibility_mirror_insert_tx(
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
        CompatibilityMirrorInsertV1::Existing { fact_id, .. } => {
            let key = OwnerKey::new(request.owner())?;
            let fact = load_current_fact_tx(transaction, &key, request.owner(), &fact_id)
                .await?
                .ok_or_else(|| {
                    storage_message(
                        COMPATIBILITY_WRITE_OPERATION,
                        "existing compatibility mirror has no canonical current fact",
                    )
                })?;
            (
                fact_id,
                fact.active_assertion_id().clone(),
                fact.last_event_id().clone(),
            )
        }
        CompatibilityMirrorInsertV1::Inserted(legacy_fact_id) => {
            let (identity, mapping) =
                compatibility_legacy_mapping_for_new_fact(request.owner(), legacy_fact_id, now)?;
            let batch = compatibility_initial_batch(
                request.owner(),
                identity,
                mapping.clone(),
                sanitized.payload,
                sanitized.access,
                proposal.request().default_trust(),
                proposal.request().actor().cloned(),
                now,
            )?;
            let (receipt, _) = compatibility_commit_batch_tx(transaction, &batch).await?;
            let assertion_id = receipt.active_assertion_id().cloned().ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
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
    compatibility_advance_proposal_tx(
        transaction,
        request.owner(),
        request.proposal_id(),
        CompatibilityFactProposalStateV1::PendingApproval,
        request.expected_revision(),
        CompatibilityFactProposalStateV1::Applied,
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
    compatibility_record_operation_receipt_tx(
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
    let promoted = compatibility_replay_proposal_tx(
        transaction,
        request.owner(),
        &CompatibilityOperationReceiptV1 {
            fact_id: Some(fact_id),
            event_id: Some(event_id),
            receipt,
        },
    )
    .await?;
    CompatibilityFactProposalPromotionResultV1::new(
        promoted,
        CompatibilityFactProposalPromotionDispositionV1::NewlyPromoted,
    )
    .map_err(Into::into)
}
