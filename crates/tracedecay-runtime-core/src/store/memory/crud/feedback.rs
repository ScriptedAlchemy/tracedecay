//! Compatibility fact feedback, history, inspection, and automatic fact apply dispatch.

use super::super::automatic_facts::{
    project_memory_automatic_fact_request_digest,
    project_memory_existing_automatic_fact_receipt_tx,
    project_memory_lookup_automatic_fact_operation_tx,
    project_memory_record_automatic_fact_operation_tx,
    project_memory_record_automatic_fact_receipt_tx,
};
use super::super::envelope::{
    ProjectMemoryOperationReceiptV1, project_memory_digest,
    project_memory_lookup_operation_receipt_tx, project_memory_receipt_u64,
    project_memory_record_operation_receipt_tx, project_memory_target_digest,
};
use super::super::primitives::{
    OwnerKey, PROJECT_MEMORY_READ_OPERATION, PROJECT_MEMORY_WRITE_OPERATION,
    compatibility_legacy_timestamp, from_json, project_memory_event_time, project_memory_now,
    row_f64, row_i64, row_optional_string, row_string, storage_error, storage_message,
};
use super::super::projection::{
    load_project_memory_projection_tx, project_memory_fact_status_tx,
    project_memory_projection_metadata_tx, project_memory_required_mapping_tx,
    resolve_project_memory_target_tx,
};
use super::super::scoring::project_memory_millionths;
use super::{
    DEFAULT_TRUST, compatibility_commit_batch_tx, compatibility_mirror_feedback_tx,
    compatibility_payload_metadata, compatibility_sanitize_payload, load_current_fact_tx,
    project_memory_feedback_action_label, project_memory_feedback_delta,
    project_memory_update_feedback_projection_tx, query_fact_lineage_tx,
};
use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use crate::db::publish_fact_feedback_finding_tx;
use crate::privacy::sanitize_provider_metadata_text;
use serde_json::{Value, json};
use tracedecay_domain::{
    ActorId, Confidence, FactAssertionKindV1, FactAssertionV1, FactCurationActionV1, FactEventId,
    FactId, FactIdentityMaterialV1, FactIdentitySourceV1, FactLineageEventKindV1,
    FactLineageEventV1, FactOwnerV1, PayloadAccessState, ProvenanceId, RetrievalAnchorRecordV2,
    UtcMicros,
};
use tracedecay_store::{
    FactCommitOutcome, FactLineageCursor, FactLineageQuery, FactStoreError, FactStoreResult,
    FactWriteBatch, ProjectMemoryAutomaticFactApplyDispositionV1,
    ProjectMemoryAutomaticFactApplyResultV1, ProjectMemoryAutomaticFactEffectV1,
    ProjectMemoryAutomaticFactEvidenceV1, ProjectMemoryAutomaticFactReceiptV1,
    ProjectMemoryFactAddCommandV1, ProjectMemoryFactFeedbackActionV1,
    ProjectMemoryFactFeedbackCommandV1, ProjectMemoryFactFeedbackDetailsAvailabilityV1,
    ProjectMemoryFactFeedbackHistoryEntryV1, ProjectMemoryFactFeedbackHistoryQueryV1,
    ProjectMemoryFactFeedbackHistoryV1, ProjectMemoryFactFeedbackOutcomeV1,
    ProjectMemoryFactHistoryV1, ProjectMemoryFactIdV1, ProjectMemoryFactInspectionV1,
    ProjectMemoryFactMappingV1, ProjectMemoryFactProjectionV1, ProjectMemoryFactTargetV1,
    ProjectMemoryFeedbackRepairProgressV1, ProjectMemoryResult, StoredFactV1,
};
fn project_memory_receipt_i32(receipt: &Value, field: &'static str) -> FactStoreResult<i32> {
    receipt
        .get(field)
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                format!("compatibility receipt {field} is malformed"),
            )
        })
}

fn project_memory_receipt_confidence(
    receipt: &Value,
    field: &'static str,
) -> FactStoreResult<Confidence> {
    let millionths = project_memory_receipt_u64(receipt, field)?;
    if millionths > 1_000_000 {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            format!("compatibility receipt {field} is out of range"),
        ));
    }
    Confidence::new(millionths as f64 / 1_000_000.0).map_err(FactStoreError::from)
}

fn project_memory_feedback_detail(value: Option<&str>) -> Option<String> {
    value
        .and_then(sanitize_provider_metadata_text)
        .filter(|value| !value.trim().is_empty())
}

fn project_memory_feedback_details(
    source: Option<&str>,
    reason: Option<&str>,
) -> (
    String,
    Option<String>,
    Option<String>,
    ProjectMemoryFactFeedbackDetailsAvailabilityV1,
) {
    let persisted_source = match source {
        Some(source) => project_memory_feedback_detail(Some(source)),
        None => Some("mcp".to_owned()),
    };
    let persisted_note = project_memory_feedback_detail(reason);
    let details_available = reason.is_none() || persisted_note.is_some();
    if let Some(source) = persisted_source
        && details_available
    {
        (
            source.clone(),
            Some(source),
            persisted_note,
            ProjectMemoryFactFeedbackDetailsAvailabilityV1::Available,
        )
    } else {
        (
            "mcp".to_owned(),
            None,
            None,
            ProjectMemoryFactFeedbackDetailsAvailabilityV1::Unknown,
        )
    }
}

fn project_memory_feedback_batch(
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

fn project_memory_feedback_details_label(
    availability: ProjectMemoryFactFeedbackDetailsAvailabilityV1,
) -> &'static str {
    match availability {
        ProjectMemoryFactFeedbackDetailsAvailabilityV1::Available => "available",
        ProjectMemoryFactFeedbackDetailsAvailabilityV1::LegacyRedacted => "redacted",
        ProjectMemoryFactFeedbackDetailsAvailabilityV1::Unknown => "unknown",
    }
}

fn project_memory_feedback_details_availability(
    value: &str,
) -> FactStoreResult<ProjectMemoryFactFeedbackDetailsAvailabilityV1> {
    match value {
        "available" => Ok(ProjectMemoryFactFeedbackDetailsAvailabilityV1::Available),
        "redacted" => Ok(ProjectMemoryFactFeedbackDetailsAvailabilityV1::LegacyRedacted),
        "unknown" => Ok(ProjectMemoryFactFeedbackDetailsAvailabilityV1::Unknown),
        _ => Err(storage_message(
            PROJECT_MEMORY_READ_OPERATION,
            format!("unknown compatibility feedback detail availability {value:?}"),
        )),
    }
}

fn automatic_fact_initial_batch(
    request: &ProjectMemoryFactAddCommandV1,
    payload: tracedecay_domain::FactPayloadV1,
    access: PayloadAccessState,
    now: UtcMicros,
) -> FactStoreResult<FactWriteBatch> {
    let identity = FactIdentityMaterialV1::new(
        request.owner().clone(),
        FactIdentitySourceV1::Application {
            operation_id: request.operation_id().clone(),
        },
    )?;
    let fact_id = FactId::derive(&identity)?;
    let assertion = FactAssertionV1::new(
        fact_id.clone(),
        request.owner().clone(),
        FactAssertionKindV1::Initial,
        payload,
        Vec::new(),
        now,
        request.actor().cloned(),
    )?;
    let mut events = vec![FactLineageEventV1::new(
        fact_id.clone(),
        request.owner().clone(),
        FactLineageEventKindV1::AssertionRecorded {
            assertion_id: assertion.assertion_id().clone(),
        },
        now,
        request.actor().cloned(),
    )?];
    let mut next_offset = 1;
    if access != PayloadAccessState::Eligible {
        events.push(FactLineageEventV1::new(
            fact_id.clone(),
            request.owner().clone(),
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: PayloadAccessState::Eligible,
                current: access,
            },
            project_memory_event_time(now, next_offset)?,
            request.actor().cloned(),
        )?);
        next_offset += 1;
    }
    let default_trust = Confidence::new(DEFAULT_TRUST)?;
    if request.default_trust() != default_trust {
        events.push(FactLineageEventV1::new(
            fact_id.clone(),
            request.owner().clone(),
            FactLineageEventKindV1::TrustChanged {
                previous: default_trust,
                current: request.default_trust(),
                evidence_ids: Vec::new(),
            },
            project_memory_event_time(now, next_offset)?,
            request.actor().cloned(),
        )?);
    }
    FactWriteBatch::new(
        fact_id,
        request.owner().clone(),
        Some(assertion),
        events,
        Vec::new(),
        Vec::new(),
        None,
        None,
    )?
    .with_identity_material(identity)
}

fn project_memory_feedback_action(
    value: &str,
) -> FactStoreResult<ProjectMemoryFactFeedbackActionV1> {
    match value {
        "helpful" => Ok(ProjectMemoryFactFeedbackActionV1::Helpful),
        "unhelpful" => Ok(ProjectMemoryFactFeedbackActionV1::Unhelpful),
        _ => Err(storage_message(
            PROJECT_MEMORY_READ_OPERATION,
            format!("unknown compatibility feedback action {value:?}"),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
async fn project_memory_record_feedback_history_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
    event_id: &FactEventId,
    action: ProjectMemoryFactFeedbackActionV1,
    old_trust: Confidence,
    new_trust: Confidence,
    occurred_at: UtcMicros,
    source: Option<&str>,
    note: Option<&str>,
    availability: ProjectMemoryFactFeedbackDetailsAvailabilityV1,
) -> FactStoreResult<()> {
    let key = OwnerKey::new(owner)?;
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
                project_memory_feedback_action_label(action),
                old_trust.as_f64(),
                new_trust.as_f64(),
                occurred_at.0,
                source,
                note,
                project_memory_feedback_details_label(availability),
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    Ok(())
}

async fn project_memory_replay_feedback_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    receipt: &ProjectMemoryOperationReceiptV1,
) -> ProjectMemoryResult<ProjectMemoryFactFeedbackOutcomeV1> {
    let fact_id = receipt.fact_id.as_ref().ok_or_else(|| {
        storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "compatibility feedback receipt fact is missing",
        )
    })?;
    let event_id = receipt.event_id.as_ref().ok_or_else(|| {
        storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "compatibility feedback receipt event is missing",
        )
    })?;
    let fact = load_project_memory_projection_tx(transaction, owner, fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "compatibility feedback replay fact is missing",
            )
        })?;
    let legacy_feedback_event_id = i64::try_from(project_memory_receipt_u64(
        &receipt.receipt,
        "legacy_feedback_event_id",
    )?)
    .map_err(|_| {
        storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "compatibility feedback receipt legacy event id is out of range",
        )
    })?;
    ProjectMemoryFactFeedbackOutcomeV1::new(
        fact,
        event_id.clone(),
        Some(legacy_feedback_event_id),
        project_memory_receipt_confidence(&receipt.receipt, "old_trust_millionths")?,
        project_memory_receipt_confidence(&receipt.receipt, "new_trust_millionths")?,
        project_memory_receipt_i32(&receipt.receipt, "trust_delta_millionths")?,
        project_memory_receipt_u64(&receipt.receipt, "helpful_count")?,
        project_memory_receipt_u64(&receipt.receipt, "unhelpful_count")?,
    )
    .map_err(Into::into)
}

pub(in crate::store::memory) async fn record_project_memory_fact_feedback_tx(
    transaction: &Transaction<'_>,
    request: &ProjectMemoryFactFeedbackCommandV1,
) -> ProjectMemoryResult<ProjectMemoryFactFeedbackOutcomeV1> {
    let request_digest = project_memory_digest(json!({
        "target": project_memory_target_digest(request.target())?,
        "expected_last_event_id": request.expected_last_event_id().map(FactEventId::as_str),
        "action": project_memory_feedback_action_label(request.action()),
        "actor": request.actor().map(ActorId::as_str),
        "source": request.source(),
        "reason": request.reason(),
    }))?;
    if let Some(receipt) = project_memory_lookup_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "feedback",
        &request_digest,
    )
    .await?
    {
        return project_memory_replay_feedback_tx(transaction, request.target().owner(), &receipt)
            .await;
    }
    let fact_id = resolve_project_memory_target_tx(transaction, request.target())
        .await?
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "compatibility feedback target is missing",
            )
        })?;
    let owner_key = OwnerKey::new(request.target().owner())?;
    let current = load_current_fact_tx(transaction, &owner_key, request.target().owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "compatibility feedback target is unavailable",
            )
        })?;
    let old_trust = current.trust();
    let new_trust = Confidence::new(
        (old_trust.as_f64() + project_memory_feedback_delta(request.action())).clamp(0.0, 1.0),
    )
    .map_err(FactStoreError::from)?;
    let now = project_memory_now()?;
    let batch = project_memory_feedback_batch(
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
    .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let mapping =
        project_memory_required_mapping_tx(transaction, request.target().owner(), &fact_id).await?;
    let (mirror_source, history_source, history_note, availability) =
        project_memory_feedback_details(request.source(), request.reason());
    let legacy_feedback_event_id = compatibility_mirror_feedback_tx(
        transaction,
        mapping.legacy_fact_id(),
        request.action(),
        old_trust,
        new_trust,
        compatibility_legacy_timestamp(now),
        &mirror_source,
        history_note.as_deref(),
    )
    .await?;
    project_memory_record_feedback_history_tx(
        transaction,
        request.target().owner(),
        &fact_id,
        &event_id,
        request.action(),
        old_trust,
        new_trust,
        now,
        history_source.as_deref(),
        history_note.as_deref(),
        availability,
    )
    .await?;
    project_memory_update_feedback_projection_tx(
        transaction,
        request.target().owner(),
        &fact_id,
        request.action(),
        now,
    )
    .await?;
    let fact = load_project_memory_projection_tx(transaction, request.target().owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "compatibility feedback projection is missing",
            )
        })?;
    let (_, _, telemetry) = project_memory_projection_metadata_tx(
        transaction,
        request.target().owner(),
        &fact_id,
        Some(&mapping),
    )
    .await?;
    let trust_delta_millionths =
        ((new_trust.as_f64() - old_trust.as_f64()) * 1_000_000.0).round() as i32;
    let receipt = json!({
        "old_trust_millionths": project_memory_millionths(old_trust.as_f64()),
        "new_trust_millionths": project_memory_millionths(new_trust.as_f64()),
        "trust_delta_millionths": trust_delta_millionths,
        "helpful_count": telemetry.helpful_count(),
        "unhelpful_count": telemetry.unhelpful_count(),
        "legacy_feedback_event_id": legacy_feedback_event_id,
    });
    project_memory_record_operation_receipt_tx(
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
    ProjectMemoryFactFeedbackOutcomeV1::new(
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

pub(in crate::store::memory) async fn project_memory_fact_feedback_history_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactFeedbackHistoryQueryV1,
) -> ProjectMemoryResult<ProjectMemoryFactFeedbackHistoryV1> {
    let fact_id = resolve_project_memory_target_tx(transaction, query.target())
        .await?
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_READ_OPERATION,
                "compatibility feedback history target is missing",
            )
        })?;
    let key = OwnerKey::new(query.target().owner())?;
    let fetch_limit = i64::try_from(query.limit().saturating_add(1)).map_err(|_| {
        FactStoreError::InvalidQueryLimit {
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
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let mut events = Vec::with_capacity(query.limit().saturating_add(1));
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        events.push(ProjectMemoryFactFeedbackHistoryEntryV1::new(
            FactEventId::new(row_string(&row, 0, PROJECT_MEMORY_READ_OPERATION)?)
                .map_err(FactStoreError::from)?,
            UtcMicros(row_i64(&row, 1, PROJECT_MEMORY_READ_OPERATION)?),
            project_memory_feedback_action(&row_string(&row, 2, PROJECT_MEMORY_READ_OPERATION)?)?,
            Confidence::new(row_f64(&row, 3, PROJECT_MEMORY_READ_OPERATION)?)
                .map_err(FactStoreError::from)?,
            Confidence::new(row_f64(&row, 4, PROJECT_MEMORY_READ_OPERATION)?)
                .map_err(FactStoreError::from)?,
            row_optional_string(&row, 5, PROJECT_MEMORY_READ_OPERATION)?,
            row_optional_string(&row, 6, PROJECT_MEMORY_READ_OPERATION)?,
            project_memory_feedback_details_availability(&row_string(
                &row,
                7,
                PROJECT_MEMORY_READ_OPERATION,
            )?)?,
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
    ProjectMemoryFactFeedbackHistoryV1::new_with_repair_progress(
        query.target().owner().clone(),
        events,
        next_after,
        ProjectMemoryFeedbackRepairProgressV1::NotRequired,
    )
    .map_err(Into::into)
}

pub(in crate::store::memory) async fn inspect_project_memory_fact_tx(
    transaction: &Transaction<'_>,
    target: &ProjectMemoryFactTargetV1,
) -> ProjectMemoryResult<Option<ProjectMemoryFactInspectionV1>> {
    let Some(fact_id) = resolve_project_memory_target_tx(transaction, target).await? else {
        return Ok(None);
    };
    let Some(ProjectMemoryFactProjectionV1::Available(fact)) =
        load_project_memory_projection_tx(transaction, target.owner(), &fact_id).await?
    else {
        return Ok(None);
    };
    let lineage = FactLineageQuery::new(target.owner().clone(), fact_id.clone(), None, 1_000)?;
    let history = ProjectMemoryFactHistoryV1::new(
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
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let mut anchors = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        let anchor = from_json::<RetrievalAnchorRecordV2>(
            &row_string(&row, 0, PROJECT_MEMORY_READ_OPERATION)?,
            PROJECT_MEMORY_READ_OPERATION,
        )?;
        if FactOwnerV1::from(anchor.owner().clone()) != *target.owner() {
            return Err(FactStoreError::OwnerMismatch.into());
        }
        anchors.push(anchor);
    }
    let status = project_memory_fact_status_tx(transaction, target.owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_READ_OPERATION,
                "compatibility inspection status is missing",
            )
        })?;
    ProjectMemoryFactInspectionV1::new(*fact, history, anchors, status)
        .map(Some)
        .map_err(Into::into)
}

pub(super) struct CommitAttempt {
    pub(super) outcome: FactCommitOutcome,
    pub(super) wrote: bool,
}

pub(in crate::store::memory) async fn apply_project_memory_automatic_fact_tx(
    transaction: &Transaction<'_>,
    apply_id: ProvenanceId,
    request: &ProjectMemoryFactAddCommandV1,
    evidence: &ProjectMemoryAutomaticFactEvidenceV1,
) -> ProjectMemoryResult<ProjectMemoryAutomaticFactApplyResultV1> {
    let request_digest = project_memory_automatic_fact_request_digest(request)?;
    if let Some(receipt) =
        project_memory_lookup_automatic_fact_operation_tx(transaction, request, &request_digest)
            .await?
    {
        let disposition = match receipt.state() {
            tracedecay_store::ProjectMemoryAutomaticFactStateV1::Applied => {
                ProjectMemoryAutomaticFactApplyDispositionV1::AlreadyApplied
            }
            tracedecay_store::ProjectMemoryAutomaticFactStateV1::Quarantined => {
                ProjectMemoryAutomaticFactApplyDispositionV1::Quarantined
            }
        };
        return automatic_fact_apply_result(receipt, disposition);
    }
    if let Some(receipt) = project_memory_existing_automatic_fact_receipt_tx(
        transaction,
        request.owner(),
        &apply_id,
        &request_digest,
    )
    .await?
    {
        let disposition = match receipt.state() {
            tracedecay_store::ProjectMemoryAutomaticFactStateV1::Applied => {
                ProjectMemoryAutomaticFactApplyDispositionV1::AlreadyApplied
            }
            tracedecay_store::ProjectMemoryAutomaticFactStateV1::Quarantined => {
                ProjectMemoryAutomaticFactApplyDispositionV1::Quarantined
            }
        };
        return automatic_fact_apply_result(receipt, disposition);
    }
    let now = project_memory_now()?;
    let payload_metadata = compatibility_payload_metadata(request.metadata());
    let sanitized = compatibility_sanitize_payload(
        request.content(),
        request.category(),
        request.tags(),
        request.entities(),
        &payload_metadata,
    )?;
    let Some(sanitized) = sanitized else {
        let effect = ProjectMemoryAutomaticFactEffectV1::Quarantined {
            reason: "content declined by privacy sanitizer".to_owned(),
        };
        project_memory_record_automatic_fact_receipt_tx(
            transaction,
            &apply_id,
            request,
            &request_digest,
            evidence,
            &effect,
            now,
        )
        .await?;
        let receipt = ProjectMemoryAutomaticFactReceiptV1::new(
            apply_id,
            request.owner().clone(),
            effect.state(),
            request.clone(),
            evidence.clone(),
            effect,
            now,
        )?;
        project_memory_record_automatic_fact_operation_tx(transaction, &receipt, &request_digest)
            .await?;
        return automatic_fact_apply_result(
            receipt,
            ProjectMemoryAutomaticFactApplyDispositionV1::Quarantined,
        );
    };
    let batch = automatic_fact_initial_batch(request, sanitized.payload, sanitized.access, now)?;
    let (canonical_receipt, _) = compatibility_commit_batch_tx(transaction, &batch).await?;
    let fact_id = canonical_receipt.fact_id().clone();
    let assertion_id = canonical_receipt
        .active_assertion_id()
        .cloned()
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "applied automatic fact has no active assertion",
            )
        })?;
    let event_id = canonical_receipt.last_event_id().clone();
    let mapping = ProjectMemoryFactMappingV1::new(
        ProjectMemoryFactIdV1::new(request.owner().clone(), fact_id.clone())?,
        None,
    )?;
    let effect = ProjectMemoryAutomaticFactEffectV1::Applied {
        fact_id,
        mapping,
        assertion_id,
        event_id,
    };
    project_memory_record_automatic_fact_receipt_tx(
        transaction,
        &apply_id,
        request,
        &request_digest,
        evidence,
        &effect,
        now,
    )
    .await?;
    let receipt = ProjectMemoryAutomaticFactReceiptV1::new(
        apply_id,
        request.owner().clone(),
        effect.state(),
        request.clone(),
        evidence.clone(),
        effect,
        now,
    )?;
    project_memory_record_automatic_fact_operation_tx(transaction, &receipt, &request_digest)
        .await?;
    automatic_fact_apply_result(
        receipt,
        ProjectMemoryAutomaticFactApplyDispositionV1::Applied,
    )
}

fn automatic_fact_apply_result(
    receipt: ProjectMemoryAutomaticFactReceiptV1,
    disposition: ProjectMemoryAutomaticFactApplyDispositionV1,
) -> ProjectMemoryResult<ProjectMemoryAutomaticFactApplyResultV1> {
    ProjectMemoryAutomaticFactApplyResultV1::new(receipt, disposition).map_err(Into::into)
}
