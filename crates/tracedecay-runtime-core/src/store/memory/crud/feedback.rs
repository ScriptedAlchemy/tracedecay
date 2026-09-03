//! Canonical fact feedback, history, inspection, and automatic fact apply dispatch.

use super::super::automatic_facts::{
    project_memory_existing_automatic_fact_receipt_tx,
    project_memory_lookup_automatic_fact_operation_tx,
    project_memory_record_automatic_fact_operation_tx,
    project_memory_record_automatic_fact_receipt_tx,
};
use super::super::envelope::{
    ProjectMemoryOperationReceiptV1, project_memory_digest,
    project_memory_lookup_operation_receipt_tx, project_memory_receipt_u64,
    project_memory_record_operation_receipt_tx,
};
use super::super::primitives::{
    OwnerKey, PROJECT_MEMORY_READ_OPERATION, PROJECT_MEMORY_WRITE_OPERATION,
    ensure_project_memory_read_active, from_json, project_memory_now, row_f64, row_i64,
    row_optional_string, row_string, storage_error, storage_message,
};
use super::super::projection::{
    load_project_memory_projection_controlled_tx, load_project_memory_projection_tx,
    project_memory_fact_status_tx, project_memory_projection_metadata_tx,
};
use super::super::scoring::project_memory_millionths;
use super::{
    commit_batch_tx, commit_receipt_json, initial_batch, load_mutable_project_memory_fact_tx,
    payload_metadata, project_memory_commit_receipt_from_operation_tx,
    project_memory_feedback_action_label, project_memory_feedback_delta,
    project_memory_update_feedback_projection_tx, query_fact_lineage_controlled_tx,
    query_fact_lineage_tx, sanitize_payload,
};
use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use crate::db::publish_fact_feedback_finding_tx;
use crate::privacy::sanitize_provider_metadata_text;
use serde_json::{Value, json};
use tracedecay_domain::{
    ActorId, Confidence, FactCurationActionV1, FactEventId, FactId, FactLineageEventKindV1,
    FactLineageEventV1, FactOwnerV1, ProvenanceId, RetrievalAnchorRecordV2, UtcMicros,
};
use tracedecay_store::{
    FactCommitOutcome, FactLineageCursor, FactLineageQuery, FactReadControl, FactStoreError,
    FactStoreResult, FactWriteBatch, ProjectMemoryAutomaticFactApplyDispositionV1,
    ProjectMemoryAutomaticFactApplyResultV1, ProjectMemoryAutomaticFactEffectV1,
    ProjectMemoryAutomaticFactEvidenceV1, ProjectMemoryAutomaticFactReceiptV1,
    ProjectMemoryFactAddCommandV1, ProjectMemoryFactFeedbackActionV1,
    ProjectMemoryFactFeedbackCommandV1, ProjectMemoryFactFeedbackDetailsAvailabilityV1,
    ProjectMemoryFactFeedbackHistoryEntryV1, ProjectMemoryFactFeedbackHistoryQueryV1,
    ProjectMemoryFactFeedbackHistoryV1, ProjectMemoryFactFeedbackOutcomeV1,
    ProjectMemoryFactHistoryV1, ProjectMemoryFactIdV1, ProjectMemoryFactInspectionV1,
    ProjectMemoryFactProjectionV1, StoredFactV1,
};
fn project_memory_receipt_i32(receipt: &Value, field: &'static str) -> FactStoreResult<i32> {
    receipt
        .get(field)
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                format!("receipt {field} is malformed"),
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
            format!("receipt {field} is out of range"),
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
    source_label: Option<&str>,
    reason: Option<&str>,
) -> (
    Option<String>,
    Option<String>,
    ProjectMemoryFactFeedbackDetailsAvailabilityV1,
) {
    let persisted_source = project_memory_feedback_detail(source_label);
    let persisted_note = project_memory_feedback_detail(reason);
    if persisted_source.is_some() || persisted_note.is_some() {
        (
            persisted_source,
            persisted_note,
            ProjectMemoryFactFeedbackDetailsAvailabilityV1::Available,
        )
    } else if source_label.is_some() || reason.is_some() {
        (
            None,
            None,
            ProjectMemoryFactFeedbackDetailsAvailabilityV1::Redacted,
        )
    } else {
        (
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
        expected_last_event_id,
    )
}

fn project_memory_feedback_details_label(
    availability: ProjectMemoryFactFeedbackDetailsAvailabilityV1,
) -> &'static str {
    match availability {
        ProjectMemoryFactFeedbackDetailsAvailabilityV1::Available => "available",
        ProjectMemoryFactFeedbackDetailsAvailabilityV1::Redacted => "redacted",
        ProjectMemoryFactFeedbackDetailsAvailabilityV1::Unknown => "unknown",
    }
}

fn project_memory_feedback_details_availability(
    value: &str,
) -> FactStoreResult<ProjectMemoryFactFeedbackDetailsAvailabilityV1> {
    match value {
        "available" => Ok(ProjectMemoryFactFeedbackDetailsAvailabilityV1::Available),
        "redacted" => Ok(ProjectMemoryFactFeedbackDetailsAvailabilityV1::Redacted),
        "unknown" => Ok(ProjectMemoryFactFeedbackDetailsAvailabilityV1::Unknown),
        _ => Err(storage_message(
            PROJECT_MEMORY_READ_OPERATION,
            format!("unknown feedback detail availability {value:?}"),
        )),
    }
}

fn project_memory_feedback_action(
    value: &str,
) -> FactStoreResult<ProjectMemoryFactFeedbackActionV1> {
    match value {
        "helpful" => Ok(ProjectMemoryFactFeedbackActionV1::Helpful),
        "unhelpful" => Ok(ProjectMemoryFactFeedbackActionV1::Unhelpful),
        _ => Err(storage_message(
            PROJECT_MEMORY_READ_OPERATION,
            format!("unknown feedback action {value:?}"),
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
) -> FactStoreResult<ProjectMemoryFactFeedbackOutcomeV1> {
    let fact_id = receipt.fact_id.as_ref().ok_or_else(|| {
        storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "feedback receipt fact is missing",
        )
    })?;
    let event_id = receipt.event_id.as_ref().ok_or_else(|| {
        storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "feedback receipt event is missing",
        )
    })?;
    let fact = load_project_memory_projection_tx(transaction, owner, fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "replayed feedback fact is missing",
            )
        })?;
    ProjectMemoryFactFeedbackOutcomeV1::committed(
        fact,
        event_id.clone(),
        project_memory_receipt_confidence(&receipt.receipt, "old_trust_millionths")?,
        project_memory_receipt_confidence(&receipt.receipt, "new_trust_millionths")?,
        project_memory_receipt_i32(&receipt.receipt, "trust_delta_millionths")?,
        project_memory_receipt_u64(&receipt.receipt, "helpful_count")?,
        project_memory_receipt_u64(&receipt.receipt, "unhelpful_count")?,
        project_memory_commit_receipt_from_operation_tx(transaction, owner, receipt).await?,
        true,
    )
}

pub(in crate::store::memory) async fn record_project_memory_fact_feedback_tx(
    transaction: &Transaction<'_>,
    request: &ProjectMemoryFactFeedbackCommandV1,
) -> FactStoreResult<ProjectMemoryFactFeedbackOutcomeV1> {
    let request_digest = project_memory_digest(json!({
        "fact_id": request.target().fact_id().as_str(),
        "expected_last_event_id": request.expected_last_event_id().map(FactEventId::as_str),
        "action": project_memory_feedback_action_label(request.action()),
        "actor": request.actor().map(ActorId::as_str),
        "source_label": request.source_label(),
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
    let fact_id = request.target().fact_id().clone();
    let current = load_mutable_project_memory_fact_tx(transaction, request.target()).await?;
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
    let (canonical_receipt, replayed) = commit_batch_tx(transaction, &batch).await?;
    let event_id = canonical_receipt.last_event_id().clone();
    publish_fact_feedback_finding_tx(
        transaction,
        request.target().owner(),
        fact_id.as_str(),
        event_id.as_str(),
    )
    .await
    .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let (history_source, history_note, availability) =
        project_memory_feedback_details(request.source_label(), request.reason());
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
                "feedback projection is missing",
            )
        })?;
    let (_, telemetry) =
        project_memory_projection_metadata_tx(transaction, request.target().owner(), &fact_id)
            .await?;
    let trust_delta_millionths =
        ((new_trust.as_f64() - old_trust.as_f64()) * 1_000_000.0).round() as i32;
    let mut receipt = commit_receipt_json("feedback", &canonical_receipt);
    receipt["old_trust_millionths"] = json!(project_memory_millionths(old_trust.as_f64()));
    receipt["new_trust_millionths"] = json!(project_memory_millionths(new_trust.as_f64()));
    receipt["trust_delta_millionths"] = json!(trust_delta_millionths);
    receipt["helpful_count"] = json!(telemetry.helpful_count());
    receipt["unhelpful_count"] = json!(telemetry.unhelpful_count());
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
    ProjectMemoryFactFeedbackOutcomeV1::committed(
        fact,
        event_id,
        old_trust,
        new_trust,
        trust_delta_millionths,
        telemetry.helpful_count(),
        telemetry.unhelpful_count(),
        canonical_receipt,
        replayed,
    )
}

pub(in crate::store::memory) async fn project_memory_fact_feedback_history_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactFeedbackHistoryQueryV1,
    read_control: &FactReadControl,
) -> FactStoreResult<ProjectMemoryFactFeedbackHistoryV1> {
    ensure_project_memory_read_active(read_control)?;
    let fact_id = query.target().fact_id().clone();
    let projection =
        load_project_memory_projection_tx(transaction, query.target().owner(), &fact_id).await?;
    ensure_project_memory_read_active(read_control)?;
    if projection.is_none() {
        return Err(storage_message(
            PROJECT_MEMORY_READ_OPERATION,
            "feedback history target is missing",
        ));
    }
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
        ensure_project_memory_read_active(read_control)?;
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
    ensure_project_memory_read_active(read_control)?;
    events.truncate(query.limit());
    let next_after = has_more
        .then(|| {
            events
                .last()
                .map(|event| FactLineageCursor::new(event.occurred_at(), event.event_id().clone()))
        })
        .flatten()
        .transpose()?;
    ProjectMemoryFactFeedbackHistoryV1::new(query.target().owner().clone(), events, next_after)
}

pub(in crate::store::memory) async fn inspect_project_memory_fact_controlled_tx(
    transaction: &Transaction<'_>,
    target: &ProjectMemoryFactIdV1,
    read_control: &FactReadControl,
) -> FactStoreResult<Option<ProjectMemoryFactInspectionV1>> {
    inspect_project_memory_fact_inner_tx(transaction, target, Some(read_control)).await
}

async fn inspect_project_memory_fact_inner_tx(
    transaction: &Transaction<'_>,
    target: &ProjectMemoryFactIdV1,
    read_control: Option<&FactReadControl>,
) -> FactStoreResult<Option<ProjectMemoryFactInspectionV1>> {
    if let Some(read_control) = read_control {
        ensure_project_memory_read_active(read_control)?;
    }
    let fact_id = target.fact_id().clone();
    let projection = match read_control {
        Some(read_control) => {
            load_project_memory_projection_controlled_tx(
                transaction,
                target.owner(),
                &fact_id,
                read_control,
            )
            .await?
        }
        None => load_project_memory_projection_tx(transaction, target.owner(), &fact_id).await?,
    };
    let Some(ProjectMemoryFactProjectionV1::Available(fact)) = projection else {
        return Ok(None);
    };
    if let Some(read_control) = read_control {
        ensure_project_memory_read_active(read_control)?;
    }
    let lineage = FactLineageQuery::new(target.owner().clone(), fact_id.clone(), None, 1_000)?;
    let events = match read_control {
        Some(read_control) => {
            query_fact_lineage_controlled_tx(transaction, &lineage, read_control).await?
        }
        None => query_fact_lineage_tx(transaction, &lineage).await?,
    };
    if let Some(read_control) = read_control {
        ensure_project_memory_read_active(read_control)?;
    }
    let history =
        ProjectMemoryFactHistoryV1::new(target.owner().clone(), fact_id.clone(), events, None)?;
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
    if let Some(read_control) = read_control {
        ensure_project_memory_read_active(read_control)?;
    }
    let mut anchors = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        if let Some(read_control) = read_control {
            ensure_project_memory_read_active(read_control)?;
        }
        let anchor = from_json::<RetrievalAnchorRecordV2>(
            &row_string(&row, 0, PROJECT_MEMORY_READ_OPERATION)?,
            PROJECT_MEMORY_READ_OPERATION,
        )?;
        if FactOwnerV1::from(anchor.owner().clone()) != *target.owner() {
            return Err(FactStoreError::OwnerMismatch);
        }
        anchors.push(anchor);
    }
    if let Some(read_control) = read_control {
        ensure_project_memory_read_active(read_control)?;
    }
    let status = project_memory_fact_status_tx(transaction, target.owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_READ_OPERATION,
                "fact inspection status is missing",
            )
        })?;
    if let Some(read_control) = read_control {
        ensure_project_memory_read_active(read_control)?;
    }
    ProjectMemoryFactInspectionV1::new(*fact, history, anchors, status).map(Some)
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
) -> FactStoreResult<ProjectMemoryAutomaticFactApplyResultV1> {
    let request_digest = request.input_digest().to_owned();
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
    let payload_metadata = payload_metadata(request.metadata());
    let sanitized = sanitize_payload(
        request.content(),
        request.category(),
        request.tags(),
        request.entities(),
        &payload_metadata,
        request.source_label(),
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
    let batch = initial_batch(
        request.owner(),
        request.operation_id(),
        sanitized.payload,
        sanitized.access,
        request.default_trust(),
        request.actor().cloned(),
        now,
    )?;
    let (canonical_receipt, _) = commit_batch_tx(transaction, &batch).await?;
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
    let target = ProjectMemoryFactIdV1::new(request.owner().clone(), fact_id.clone())?;
    let effect = ProjectMemoryAutomaticFactEffectV1::Applied {
        fact_id,
        target,
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
) -> FactStoreResult<ProjectMemoryAutomaticFactApplyResultV1> {
    ProjectMemoryAutomaticFactApplyResultV1::new(receipt, disposition)
}
